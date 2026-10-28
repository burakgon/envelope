//! JSON batching (`POST /$batch`).
//!
//! Graph accepts at most [`MAX_BATCH_SIZE`] requests per batch, executes them
//! in any order (Outlook runs up to four at a time per mailbox) and may
//! throttle individual requests with `429` while the batch itself succeeds.
//! [`GraphClient::batch`] hides all of that: it splits large inputs, retries
//! throttled sub-requests after the longest `Retry-After`, and returns one
//! [`BatchResponse`] per request in input order.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{GraphClient, GraphRequest};
use crate::error::{ApiError, GraphError};

/// Maximum number of requests Graph accepts in one JSON batch.
pub const MAX_BATCH_SIZE: usize = 20;

/// One request of a JSON batch.
#[derive(Debug, Clone)]
pub struct BatchRequest {
    /// Caller-chosen id, unique (case-insensitively) within one
    /// [`GraphClient::batch`] call.
    pub id: String,
    /// The request. It must use a relative path (no absolute links) and, if
    /// it has a body, a JSON body.
    pub request: GraphRequest,
}

impl BatchRequest {
    /// Pairs a request with its id.
    pub fn new(id: impl Into<String>, request: GraphRequest) -> Self {
        BatchRequest {
            id: id.into(),
            request,
        }
    }
}

/// The outcome of one batched request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchResponse {
    /// The id of the corresponding [`BatchRequest`].
    pub id: String,
    /// HTTP status of the individual request; `0` when no HTTP response
    /// was received for it (see [`GraphClient::batch`]).
    pub status: u16,
    /// Response headers of the individual request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Response body, if any.
    #[serde(default)]
    pub body: Option<Value>,
}

impl BatchResponse {
    /// True for 2xx statuses.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The Graph error, for non-2xx statuses.
    pub fn error(&self) -> Option<ApiError> {
        if self.is_success() {
            None
        } else {
            Some(ApiError::from_batch(
                self.status,
                &self.headers,
                self.body.as_ref(),
            ))
        }
    }

    /// The body on success, the error otherwise.
    pub fn into_result(self) -> Result<Option<Value>, GraphError> {
        match self.error() {
            Some(error) => Err(error.into()),
            None => Ok(self.body),
        }
    }

    /// Decodes the body of a successful response.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, GraphError> {
        if let Some(error) = self.error() {
            return Err(error.into());
        }
        let body = self.body.clone().unwrap_or(Value::Null);
        serde_json::from_value(body).map_err(|e| GraphError::decode("batch response body", e))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRequest {
    id: String,
    method: String,
    url: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<Value>,
}

#[derive(Serialize)]
struct WireBatch<'a> {
    requests: Vec<&'a WireRequest>,
}

#[derive(Deserialize)]
struct WireBatchResponse {
    #[serde(default)]
    responses: Vec<BatchResponse>,
}

/// Methods whose repetition has no additional effect (see the client's
/// retry rules): a `POST` sub-request that may have run is not replayed.
fn is_replayable(method: &str) -> bool {
    matches!(
        method,
        "GET" | "HEAD" | "OPTIONS" | "PUT" | "DELETE" | "PATCH"
    )
}

/// `429`/`503` sub-responses were not processed and can always be retried;
/// a `504` only when the request is safe to replay.
fn is_retryable(status: u16, method: &str) -> bool {
    match status {
        429 | 503 => true,
        504 => is_replayable(method),
        _ => false,
    }
}

/// A per-request result standing in for a batch request that failed as a
/// whole: the API error's status, or `0` when there was no HTTP response.
fn failure_response(id: &str, error: &GraphError) -> BatchResponse {
    let (status, code, retry_after) = match error {
        GraphError::Api(api) => (api.status, api.code.clone(), api.retry_after),
        GraphError::Auth(_) => (401, "authenticationFailed".to_owned(), None),
        _ => (0, "batchRequestFailed".to_owned(), None),
    };
    let mut headers = BTreeMap::new();
    if let Some(after) = retry_after {
        headers.insert("Retry-After".to_owned(), after.as_secs().to_string());
    }
    BatchResponse {
        id: id.to_owned(),
        status,
        headers,
        body: Some(json!({ "error": { "code": code, "message": error.to_string() } })),
    }
}

/// A per-request result for a request Graph did not answer.
fn missing_response(id: &str) -> BatchResponse {
    BatchResponse {
        id: id.to_owned(),
        status: 0,
        headers: BTreeMap::new(),
        body: Some(json!({ "error": {
            "code": "missingBatchResponse",
            "message": "Microsoft Graph returned no result for this request",
        } })),
    }
}

impl GraphClient {
    /// Executes requests through `POST /$batch`.
    ///
    /// Inputs larger than [`MAX_BATCH_SIZE`] are split into several batches.
    /// Sub-requests answered with `429` or `503` (and `504` for requests
    /// that are safe to replay) are retried in a new batch after the longest
    /// `Retry-After`, according to the client's
    /// [`RetryPolicy`](crate::RetryPolicy).
    ///
    /// The result holds one response per request, in input order, and
    /// individual failures are reported through [`BatchResponse::status`]:
    /// when a whole batch request fails (transport error, throttling beyond
    /// the retry budget, …) its requests get the batch's error status, or `0`
    /// when no HTTP response was received; requests in other batches keep
    /// their results. `Err` is returned only for invalid input.
    pub async fn batch(
        &self,
        requests: Vec<BatchRequest>,
    ) -> Result<Vec<BatchResponse>, GraphError> {
        let prepared = self.prepare_batch(&requests)?;
        let mut results: Vec<Option<BatchResponse>> = vec![None; prepared.len()];
        let mut pending: Vec<usize> = (0..prepared.len()).collect();
        let retry = self.retry_policy().clone();
        let mut attempt: u32 = 0;
        while !pending.is_empty() {
            // Retry candidates; `None` marks a request Graph did not answer.
            let mut again: Vec<(usize, Option<BatchResponse>)> = Vec::new();
            for chunk in pending.chunks(MAX_BATCH_SIZE) {
                let response = match self.send_chunk(&prepared, chunk).await {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::warn!(%error, requests = chunk.len(), "JSON batch request failed");
                        for &index in chunk {
                            results[index] = Some(failure_response(&prepared[index].id, &error));
                        }
                        continue;
                    }
                };
                let mut answered = vec![false; chunk.len()];
                for item in response.responses {
                    let Some(pos) = chunk
                        .iter()
                        .position(|&i| prepared[i].id.eq_ignore_ascii_case(&item.id))
                    else {
                        tracing::warn!(id = %item.id, "ignoring batch response with unknown id");
                        continue;
                    };
                    if std::mem::replace(&mut answered[pos], true) {
                        continue;
                    }
                    let index = chunk[pos];
                    if is_retryable(item.status, &prepared[index].method) {
                        again.push((index, Some(item)));
                    } else {
                        results[index] = Some(item);
                    }
                }
                for (&index, _) in chunk.iter().zip(&answered).filter(|(_, done)| !**done) {
                    if is_replayable(&prepared[index].method) {
                        again.push((index, None));
                    } else {
                        results[index] = Some(missing_response(&prepared[index].id));
                    }
                }
            }
            if again.is_empty() {
                break;
            }
            let delay = again
                .iter()
                .filter_map(|(_, r)| r.as_ref()?.error()?.retry_after)
                .max()
                .unwrap_or_else(|| retry.backoff(attempt));
            if attempt >= retry.max_retries || delay > retry.max_retry_after {
                for (index, response) in again {
                    results[index] =
                        Some(response.unwrap_or_else(|| missing_response(&prepared[index].id)));
                }
                break;
            }
            tracing::warn!(
                requests = again.len(),
                attempt,
                ?delay,
                "retrying part of a JSON batch"
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
            pending = again.into_iter().map(|(index, _)| index).collect();
            pending.sort_unstable();
        }
        Ok(results
            .into_iter()
            .zip(&prepared)
            .map(|(result, request)| result.unwrap_or_else(|| missing_response(&request.id)))
            .collect())
    }

    async fn send_chunk(
        &self,
        prepared: &[WireRequest],
        chunk: &[usize],
    ) -> Result<WireBatchResponse, GraphError> {
        let body = WireBatch {
            requests: chunk.iter().map(|&i| &prepared[i]).collect(),
        };
        let request = GraphRequest::post(["$batch"]).json(&body)?;
        self.send_json(&request).await
    }

    fn prepare_batch(&self, requests: &[BatchRequest]) -> Result<Vec<WireRequest>, GraphError> {
        let mut prepared: Vec<WireRequest> = Vec::with_capacity(requests.len());
        for request in requests {
            if request.id.is_empty() {
                return Err(GraphError::InvalidInput(
                    "batch request ids must not be empty".into(),
                ));
            }
            if prepared
                .iter()
                .any(|p| p.id.eq_ignore_ascii_case(&request.id))
            {
                return Err(GraphError::InvalidInput(format!(
                    "duplicate batch request id `{}`",
                    request.id
                )));
            }
            let body = request.request.json_body()?;
            let mut headers: BTreeMap<String, String> =
                request.request.extra_headers().iter().cloned().collect();
            if let Some(prefer) = self.prefer_header(request.request.preferences()) {
                headers.insert("Prefer".into(), prefer);
            }
            if body.is_some() {
                headers.insert("Content-Type".into(), "application/json".into());
            }
            prepared.push(WireRequest {
                id: request.id.clone(),
                method: request.request.method().as_str().to_owned(),
                url: request.request.relative_url()?,
                headers,
                body,
            });
        }
        Ok(prepared)
    }
}

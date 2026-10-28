//! Core HTTP layer: authentication refresh, throttling, error decoding,
//! paging and JSON batching against a mock Graph server.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use common::{fixture, prefer_contains, requests, setup};
use envelope_graph::{
    BatchRequest, EventResponse, EventWriteOptions, GraphClient, GraphError, GraphRequest,
    MessageDeltaOptions, MessageListOptions, MessageUpdate, StaticToken, TimeWindow,
};
use envelope_ical::{Event, EventTime};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, Request, ResponseTemplate};

#[tokio::test]
async fn refreshes_the_token_once_after_401() {
    let (server, client, tokens) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .and(header("authorization", "Bearer token-1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "InvalidAuthenticationToken", "message": "Access token has expired or is not yet valid."}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .and(header("authorization", "Bearer token-2"))
        .and(query_param(
            "$select",
            "id,displayName,mail,userPrincipalName",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, "me.json")))
        .expect(1)
        .mount(&server)
        .await;

    let me = client.me().await.expect("profile after refresh");
    assert_eq!(me.display_name.as_deref(), Some("Megan Bowen"));
    assert_eq!(me.email(), Some("MeganB@contoso.com"));
    assert_eq!(tokens.invalidations(), 1);
    assert_eq!(tokens.issued(), 2);
}

#[tokio::test]
async fn gives_up_when_the_fresh_token_is_rejected_too() {
    let (server, client, tokens) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(401).insert_header("request-id", "rid-401"))
        .expect(2)
        .mount(&server)
        .await;

    let err = client.me().await.expect_err("still unauthorized");
    assert!(err.is_unauthorized());
    assert_eq!(
        err.api().and_then(|e| e.request_id.as_deref()),
        Some("rid-401")
    );
    assert_eq!(tokens.invalidations(), 1);
}

#[tokio::test]
async fn token_source_failures_surface_as_auth_errors() {
    struct Broken;
    #[async_trait::async_trait]
    impl envelope_graph::TokenSource for Broken {
        async fn access_token(&self) -> Result<String, String> {
            Err("refresh token revoked".into())
        }
        async fn invalidate(&self) {}
    }
    let client = GraphClient::builder(Arc::new(Broken))
        .base_url("http://127.0.0.1:9/v1.0")
        .build()
        .expect("client");
    match client.me().await {
        Err(GraphError::Auth(message)) => assert_eq!(message, "refresh token revoked"),
        other => panic!("expected an auth error, got {other:?}"),
    }
}

#[tokio::test]
async fn honors_retry_after_on_429() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "1")
                .set_body_json(fixture(&server, "error_throttled.json")),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, "me.json")))
        .expect(1)
        .mount(&server)
        .await;

    let started = Instant::now();
    client.me().await.expect("retried after throttling");
    // The fast test policy would back off for milliseconds; one second means
    // the server's Retry-After was used.
    assert!(started.elapsed() >= Duration::from_millis(950));
}

#[tokio::test]
async fn reports_throttling_after_the_retry_budget() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .set_body_json(fixture(&server, "error_throttled.json")),
        )
        .expect(4)
        .mount(&server)
        .await;

    let err = client.me().await.expect_err("throttled");
    assert!(err.is_throttled());
    assert!(err.is_transient());
    let api = err.api().expect("api error");
    assert_eq!(api.code, "TooManyRequests");
    assert_eq!(api.message, "Please retry again later.");
    assert_eq!(api.inner_code.as_deref(), Some("429"));
    assert_eq!(
        api.request_id.as_deref(),
        Some("94fb3b52-452a-4535-a601-69e0a90e3aa2")
    );
    assert_eq!(api.retry_after, Some(Duration::ZERO));
}

#[tokio::test]
async fn returns_long_retry_after_to_the_caller() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3600"))
        .expect(1)
        .mount(&server)
        .await;

    let err = client.me().await.expect_err("not waiting an hour");
    assert_eq!(err.retry_after(), Some(Duration::from_secs(3600)));
}

#[tokio::test]
async fn retries_503_and_504_with_backoff() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(504))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, "me.json")))
        .mount(&server)
        .await;

    client.me().await.expect("eventually succeeds");
    assert_eq!(requests(&server).await.len(), 3);
}

#[tokio::test]
async fn does_not_replay_posts_after_gateway_timeouts() {
    let (server, client, _) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/sendMail"))
        .respond_with(ResponseTemplate::new(504))
        .expect(1)
        .mount(&server)
        .await;
    let err = client
        .send_mime(b"Subject: hello\r\n\r\nbody\r\n")
        .await
        .expect_err("a possibly delivered message is not sent twice");
    assert_eq!(err.status(), Some(504));
}

#[tokio::test]
async fn detects_paging_loops() {
    let (server, client, _) = setup().await;
    let link = format!("{}/me/things?$skiptoken=same", common::base(&server));
    Mock::given(method("GET"))
        .and(path("/v1.0/me/things"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [{"n": 1}], "@odata.nextLink": link})),
        )
        .expect(2)
        .mount(&server)
        .await;
    let result: Result<Vec<Value>, _> = client
        .get_all(&GraphRequest::get(["me", "things"]), None)
        .await;
    assert!(matches!(result, Err(GraphError::Decode(_))));
}

#[tokio::test]
async fn does_not_retry_client_errors() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages/missing"))
        .respond_with(
            ResponseTemplate::new(404)
                .insert_header("request-id", "header-request-id")
                .set_body_json(fixture(&server, "error_not_found.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let err = client.get_message("missing").await.expect_err("missing");
    assert!(err.is_not_found());
    assert!(!err.is_transient());
    let api = err.api().expect("api error");
    assert_eq!(api.status, 404);
    assert_eq!(api.code, "ErrorItemNotFound");
    assert_eq!(api.request_id.as_deref(), Some("header-request-id"));
    assert_eq!(
        api.client_request_id.as_deref(),
        Some("b3a7f1a4-0c0f-4b5e-8e9f-3f1d2c4b5a70")
    );
    assert!(err.to_string().contains("ErrorItemNotFound"));
}

#[tokio::test]
async fn sends_standard_headers() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .and(header("accept", "application/json"))
        .and(prefer_contains("IdType=\"ImmutableId\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, "me.json")))
        .expect(1)
        .mount(&server)
        .await;
    client.me().await.expect("profile");
    let sent = requests(&server).await;
    let request = sent.first().expect("one request");
    let client_request_id = request
        .headers
        .get("client-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(client_request_id.len(), 36);
    assert!(
        request
            .headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ua| ua.starts_with("Envelope/"))
    );
}

#[tokio::test]
async fn follows_next_links_keeping_preferences() {
    let (server, client, _) = setup().await;
    let base = common::base(&server);
    Mock::given(method("GET"))
        .and(path("/v1.0/me/things"))
        .and(query_param("$top", "2"))
        .and(prefer_contains("outlook.timezone=\"UTC\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"n": 1}, {"n": 2}],
            "@odata.nextLink": format!("{base}/me/things?$skiptoken=b")
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/things"))
        .and(query_param("$skiptoken", "b"))
        .and(prefer_contains("outlook.timezone=\"UTC\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [],
            "@odata.nextLink": format!("{base}/me/things?$skiptoken=c")
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/things"))
        .and(query_param("$skiptoken", "c"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [{"n": 3}]})))
        .expect(1)
        .mount(&server)
        .await;

    let request = GraphRequest::get(["me", "things"])
        .query("$top", "2")
        .prefer("outlook.timezone=\"UTC\"");
    let all: Vec<Value> = client.get_all(&request, None).await.expect("all pages");
    assert_eq!(all, vec![json!({"n": 1}), json!({"n": 2}), json!({"n": 3})]);
}

#[tokio::test]
async fn stops_at_the_item_limit() {
    let (server, client, _) = setup().await;
    let base = common::base(&server);
    Mock::given(method("GET"))
        .and(path("/v1.0/me/things"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"n": 1}, {"n": 2}, {"n": 3}],
            "@odata.nextLink": format!("{base}/me/things?$skiptoken=b")
        })))
        .expect(1)
        .mount(&server)
        .await;
    let all: Vec<Value> = client
        .get_all(&GraphRequest::get(["me", "things"]), Some(2))
        .await
        .expect("limited");
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn refuses_to_follow_links_to_other_hosts() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/things"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [],
            "@odata.nextLink": "https://attacker.example/v1.0/me/things?$skiptoken=b"
        })))
        .mount(&server)
        .await;
    let result: Result<Vec<Value>, _> = client
        .get_all(&GraphRequest::get(["me", "things"]), None)
        .await;
    assert!(matches!(result, Err(GraphError::InvalidInput(_))));
}

#[tokio::test]
async fn batches_are_split_ordered_and_retried() {
    let (server, client, _) = setup().await;
    let throttled_once = Arc::new(AtomicBool::new(false));
    let batch_calls = Arc::new(AtomicUsize::new(0));
    let (flag, calls) = (throttled_once.clone(), batch_calls.clone());
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(move |request: &Request| {
            calls.fetch_add(1, Ordering::SeqCst);
            let body: Value = request.body_json().expect("batch body");
            let requests = body["requests"].as_array().cloned().unwrap_or_default();
            assert!(requests.len() <= 20, "batch too large: {}", requests.len());
            let mut responses: Vec<Value> = requests
                .iter()
                .map(|r| {
                    let id = r["id"].as_str().unwrap_or_default().to_owned();
                    assert_eq!(r["method"], "PATCH");
                    assert_eq!(r["headers"]["Content-Type"], "application/json");
                    assert_eq!(r["headers"]["Prefer"], "IdType=\"ImmutableId\"");
                    assert_eq!(r["body"], json!({"isRead": true}));
                    if id == "7" && !flag.swap(true, Ordering::SeqCst) {
                        json!({"id": id, "status": 429, "headers": {"Retry-After": "0"},
                               "body": {"error": {"code": "ApplicationThrottled", "message": "slow down"}}})
                    } else if id == "13" {
                        json!({"id": id, "status": 404, "body": {"error": {"code": "ErrorItemNotFound", "message": "gone"}}})
                    } else {
                        json!({"id": id, "status": 200, "headers": {}, "body": {"id": format!("m{id}")}})
                    }
                })
                .collect();
            responses.reverse();
            ResponseTemplate::new(200).set_body_json(json!({ "responses": responses }))
        })
        .mount(&server)
        .await;

    let updates: Vec<(String, MessageUpdate)> = (0..45)
        .map(|i| (format!("m{i}"), MessageUpdate::new().read(true)))
        .collect();
    let results = client.update_messages(&updates).await.expect("batch");
    assert_eq!(results.len(), 45);
    for (i, result) in results.iter().enumerate() {
        if i == 13 {
            assert!(result.as_ref().is_err_and(GraphError::is_not_found));
        } else {
            assert!(result.is_ok(), "{i}: {result:?}");
        }
    }
    // 45 requests -> 3 batches, plus one retry batch for the throttled request.
    assert_eq!(batch_calls.load(Ordering::SeqCst), 4);
    let urls: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|r| {
            r.body_json::<Value>().map_or(String::new(), |b| {
                b["requests"][0]["url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
        })
        .collect();
    assert_eq!(urls.first().map(String::as_str), Some("/me/messages/m0"));
}

#[tokio::test]
async fn a_failed_batch_keeps_the_results_of_the_others() {
    let (server, client, _) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(|request: &Request| {
            let body: Value = request.body_json().expect("batch body");
            let requests = body["requests"].as_array().cloned().unwrap_or_default();
            if requests.len() < 20 {
                return ResponseTemplate::new(400).set_body_json(json!({
                    "error": {"code": "BadRequest", "message": "Invalid batch payload."}
                }));
            }
            let responses: Vec<Value> = requests
                .iter()
                .map(|r| json!({"id": r["id"], "status": 201, "body": {"id": format!("new-{}", r["id"].as_str().unwrap_or_default())}}))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({ "responses": responses }))
        })
        .expect(2)
        .mount(&server)
        .await;

    let ids: Vec<String> = (0..25).map(|i| format!("m{i}")).collect();
    let moved = client
        .move_messages(&ids, "archive")
        .await
        .expect("per-request results");
    assert_eq!(moved.len(), 25);
    assert_eq!(moved[0].as_deref().ok(), Some("new-0"));
    assert_eq!(moved[19].as_deref().ok(), Some("new-19"));
    for result in &moved[20..] {
        let err = result.as_ref().expect_err("second batch failed");
        assert_eq!(err.status(), Some(400));
        assert_eq!(err.code(), Some("BadRequest"));
    }
}

#[tokio::test]
async fn batched_posts_are_not_replayed_after_504() {
    let (server, client, _) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"responses": [
            {"id": "0", "status": 504, "body": {"error": {"code": "GatewayTimeout", "message": "timed out"}}},
            {"id": "1", "status": 201, "body": {"id": "new-b"}}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let moved = client
        .move_messages(&["a".into(), "b".into()], "archive")
        .await
        .expect("results");
    assert_eq!(
        moved[0].as_ref().err().and_then(GraphError::status),
        Some(504)
    );
    assert_eq!(moved[1].as_deref().ok(), Some("new-b"));
}

#[tokio::test]
async fn a_retried_delete_that_finds_nothing_succeeds() {
    let (server, client, _) = setup().await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/events/AAMkGone="))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/events/AAMkGone="))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    client
        .delete_event("AAMkGone=")
        .await
        .expect("already deleted");
    // A first-attempt 404 is still an error.
    let err = client.delete_event("AAMkGone=").await.expect_err("missing");
    assert!(err.is_not_found());
}

#[tokio::test]
async fn generic_batch_returns_responses_in_input_order() {
    let (server, client, _) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "responses": [
                {"id": "B", "status": 204, "headers": {}},
                {"id": "a", "status": 200, "headers": {"Content-Type": "application/json"}, "body": {"value": 1}}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let responses = client
        .batch(vec![
            BatchRequest::new("a", GraphRequest::get(["me"])),
            BatchRequest::new("b", GraphRequest::delete(["me", "messages", "x"])),
        ])
        .await
        .expect("batch");
    assert_eq!(responses[0].id, "a");
    assert_eq!(responses[0].json::<Value>().ok(), Some(json!({"value": 1})));
    assert_eq!(responses[1].status, 204);
    assert!(responses[1].is_success());
}

#[tokio::test]
async fn batch_rejects_invalid_requests() {
    let (_server, client, _) = setup().await;
    let duplicate = client
        .batch(vec![
            BatchRequest::new("1", GraphRequest::get(["me"])),
            BatchRequest::new("1", GraphRequest::get(["me"])),
        ])
        .await;
    assert!(matches!(duplicate, Err(GraphError::InvalidInput(_))));
    let link = client
        .batch(vec![BatchRequest::new(
            "1",
            GraphRequest::link(reqwest::Method::GET, "https://graph.microsoft.com/v1.0/me"),
        )])
        .await;
    assert!(matches!(link, Err(GraphError::InvalidInput(_))));
}

fn assert_send<T: Send>(_: &T) {}

#[test]
fn public_futures_are_send() {
    let client = GraphClient::new(Arc::new(StaticToken::new("t"))).expect("client");
    let start = Utc
        .with_ymd_and_hms(2024, 3, 1, 0, 0, 0)
        .single()
        .expect("date");
    let window = TimeWindow::new(start, start + chrono::Duration::days(30)).expect("window");
    let event = Event::new(
        "uid",
        EventTime::Utc { at: start },
        EventTime::Utc { at: start },
    );
    let options = EventWriteOptions::default();
    let ids = vec!["a".to_owned()];
    let updates = vec![("a".to_owned(), MessageUpdate::new().read(true))];
    let request = GraphRequest::get(["me"]);

    assert_send(&client.execute(&request));
    assert_send(&client.get_all::<Value>(&request, None));
    assert_send(&client.batch(Vec::new()));
    assert_send(&client.me());
    assert_send(&client.mailbox_settings());
    assert_send(&client.set_automatic_replies(&Default::default()));
    assert_send(&client.mail_folder_tree());
    assert_send(&client.create_mail_folder(None, "x"));
    assert_send(&client.message_delta("inbox", None, &MessageDeltaOptions::default()));
    assert_send(&client.list_messages("inbox", &MessageListOptions::default()));
    assert_send(&client.search_messages("x", None, 10));
    assert_send(&client.get_message_mime("a"));
    assert_send(&client.update_messages(&updates));
    assert_send(&client.move_messages(&ids, "archive"));
    assert_send(&client.send_mime(b"x"));
    assert_send(&client.create_draft_mime(b"x", None));
    assert_send(&client.master_categories());
    assert_send(&client.list_calendars());
    assert_send(&client.calendar_view_delta(&window, None, None));
    assert_send(&client.calendar_view(None, &window));
    assert_send(&client.get_series_master("a"));
    assert_send(&client.list_series_masters(None));
    assert_send(&client.create_event(None, &event, &options));
    assert_send(&client.update_event_diff("a", &event, &event, &options));
    assert_send(&client.respond_to_event("a", EventResponse::Accept, None, true));
    assert_send(&client.get_schedule(&ids, &window, 30));
    assert_send(&client.all_contacts());
    assert_send(&client.contacts_delta("f", None, None));
    assert_send(&client.search_people("x", 5));
}

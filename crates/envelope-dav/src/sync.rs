//! Incremental synchronisation with `sync-collection` reports (RFC 6578),
//! and the `PROPFIND` listing used with servers that do not support them.

use std::collections::HashMap;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use url::Url;

use crate::client::{DavClient, Depth, HttpResponse, normalize_etag};
use crate::error::{DavError, Result};
use crate::href;
use crate::object::ObjectRef;
use crate::xml::{self, Ns, REQUEST_NAMESPACES, XML_DECLARATION};

/// Upper bound on follow-up requests for one truncated sync, guarding
/// against servers that never stop reporting truncation.
const MAX_SYNC_PAGES: usize = 1000;

/// Changes reported by [`DavClient::sync_collection`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncDelta {
    /// Members created or modified since the given token (all members for an
    /// initial sync), with their current entity tags.
    pub changed: Vec<ObjectRef>,
    /// Members deleted since the given token.
    pub removed: Vec<String>,
    /// Token to pass to the next sync. `None` only if the server failed to
    /// provide one, in which case the next sync has to start from scratch.
    pub sync_token: Option<String>,
    /// True when the server still has more changes than it reported (it kept
    /// truncating the result); call again with `sync_token` to continue.
    pub truncated: bool,
}

/// One page of a sync-collection report.
struct SyncPage {
    changed: Vec<ObjectRef>,
    removed: Vec<String>,
    sync_token: Option<String>,
    truncated: bool,
}

/// Accumulates pages so that later pages win for the same href.
#[derive(Default)]
struct SyncAccumulator {
    order: Vec<String>,
    state: HashMap<String, Option<Option<String>>>,
}

impl SyncAccumulator {
    /// `Some(etag)` for a change, `None` for a removal.
    fn record(&mut self, href: String, change: Option<Option<String>>) {
        if !self.state.contains_key(&href) {
            self.order.push(href.clone());
        }
        self.state.insert(href, change);
    }

    fn add(&mut self, page: SyncPage) {
        for object in page.changed {
            self.record(object.href, Some(object.etag));
        }
        for href in page.removed {
            self.record(href, None);
        }
    }

    fn finish(mut self, sync_token: Option<String>, truncated: bool) -> SyncDelta {
        let mut delta = SyncDelta {
            sync_token,
            truncated,
            ..SyncDelta::default()
        };
        for href in self.order {
            match self.state.remove(&href) {
                Some(Some(etag)) => delta.changed.push(ObjectRef { href, etag }),
                Some(None) => delta.removed.push(href),
                None => {}
            }
        }
        delta
    }
}

impl DavClient {
    /// Reports what changed in a collection since `token` with a
    /// `sync-collection` report (RFC 6578). Pass `None` for an initial sync,
    /// which reports every member as changed.
    ///
    /// Truncated results (the server answers `507` for the collection) are
    /// continued automatically with the intermediate token and merged.
    ///
    /// Fails with [`DavError::InvalidSyncToken`] when the server no longer
    /// accepts `token`: forget it and sync again with `None`. Servers without
    /// RFC 6578 support (no `sync_token` on the collection) can be handled
    /// with [`list_etags`](DavClient::list_etags) instead.
    pub async fn sync_collection(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<SyncDelta> {
        let url = self.resolve_input(collection)?;
        let mut token = token
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        let mut accumulator = SyncAccumulator::default();

        for page_number in 1..=MAX_SYNC_PAGES {
            let page = self.sync_page(&url, token.as_deref()).await?;
            let next_token = page.sync_token.clone();
            let truncated = page.truncated;
            accumulator.add(page);
            if !truncated {
                return Ok(accumulator.finish(next_token.or(token), false));
            }
            match next_token {
                Some(next) if Some(&next) != token.as_ref() => {
                    debug!(collection = %url, page = page_number, "sync result truncated; continuing");
                    token = Some(next);
                }
                // Truncated without progress: report what we have.
                other => return Ok(accumulator.finish(other.or(token), true)),
            }
        }
        warn!(collection = %url, "sync still truncated after {MAX_SYNC_PAGES} pages");
        Ok(accumulator.finish(token, true))
    }

    async fn sync_page(&self, url: &Url, token: Option<&str>) -> Result<SyncPage> {
        let body = sync_collection_body(token);
        // RFC 6578 requires `Depth: 0` (the depth is in the body). A few
        // servers implementing the earlier drafts insist on `Depth: 1`.
        let mut response = self.report_raw(url, Depth::Zero, body.clone()).await?;
        if response.status == StatusCode::BAD_REQUEST {
            debug!(collection = %url, "sync-collection with Depth 0 rejected; retrying with Depth 1");
            response = self.report_raw(url, Depth::One, body).await?;
        }
        if !matches!(response.status, StatusCode::MULTI_STATUS | StatusCode::OK) {
            return Err(sync_error(response, token.is_some()));
        }
        let reply = response.into_multistatus()?;

        let mut page = SyncPage {
            changed: Vec::new(),
            removed: Vec::new(),
            sync_token: reply.multistatus.sync_token.clone(),
            truncated: false,
        };
        // The collection may be named by the requested URL or, after a
        // redirect, by the URL that answered.
        let answered = href::normalize(reply.url.clone());
        for (member, response) in reply.entries() {
            if href::same_resource(&member, url) || href::same_resource(&member, &answered) {
                // RFC 6578 §3.6: a truncated result carries a 507 for the
                // collection itself, usually with this error element.
                let limited = response.error.as_ref().is_some_and(|error| {
                    error.has_child(Ns::Dav, "number-of-matches-within-limits")
                });
                if response.status == Some(507) || limited {
                    page.truncated = true;
                }
                continue;
            }
            if response.is_not_found() {
                page.removed.push(member.into());
                continue;
            }
            // Sub-collections are not calendar objects or vCards.
            if href::looks_like_collection(&member) || response.is_collection() {
                continue;
            }
            match response.prop_text(Ns::Dav, "getetag") {
                Some(etag) => page.changed.push(ObjectRef {
                    href: member.into(),
                    etag: normalize_etag(&etag),
                }),
                // Some servers report deletions as a 404 propstat instead of a
                // response-level status.
                None if !response.has_successful_propstat() && response.all_propstats_missing() => {
                    page.removed.push(member.into());
                }
                None => page.changed.push(ObjectRef {
                    href: member.into(),
                    etag: None,
                }),
            }
        }
        Ok(page)
    }

    /// Lists the members of a collection with their entity tags
    /// (`PROPFIND` depth 1 on `getetag`).
    ///
    /// This is the fallback for servers without `sync-collection` support:
    /// compare the result with the local state and fetch what changed with
    /// [`multiget`](DavClient::multiget) /
    /// [`addressbook_multiget`](DavClient::addressbook_multiget).
    pub async fn list_etags(&self, collection: &str) -> Result<Vec<ObjectRef>> {
        let url = self.resolve_input(collection)?;
        let body = xml::propfind_body(&[(Ns::Dav, "getetag"), (Ns::Dav, "resourcetype")]);
        let reply = self.propfind(&url, Depth::One, body).await?;
        let mut members = Vec::new();
        for (member, response) in reply.entries() {
            if href::same_resource(&member, &url)
                || response.is_not_found()
                || response.is_collection()
                || href::looks_like_collection(&member)
            {
                continue;
            }
            members.push(ObjectRef {
                href: member.into(),
                etag: response
                    .prop_text(Ns::Dav, "getetag")
                    .and_then(|etag| normalize_etag(&etag)),
            });
        }
        Ok(members)
    }
}

fn sync_collection_body(token: Option<&str>) -> String {
    let token = match token {
        Some(token) => xml::text_element(Ns::Dav, "sync-token", token),
        None => "<D:sync-token/>".to_owned(),
    };
    format!(
        "{XML_DECLARATION}<D:sync-collection {REQUEST_NAMESPACES}>{token}<D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"
    )
}

/// Maps a failed sync-collection response. A rejected token must surface as
/// [`DavError::InvalidSyncToken`] so the caller falls back to a full sync:
/// RFC 6578 servers report the `DAV:valid-sync-token` precondition with 403
/// or 409; others just answer 403, 409 or 410 when given a stale token.
fn sync_error(response: HttpResponse, had_token: bool) -> DavError {
    let condition = response.error_condition();
    let token_rejected = condition
        .as_ref()
        .is_some_and(|c| c.ns == Ns::Dav && c.name == "valid-sync-token");
    if token_rejected || (had_token && matches!(response.status.as_u16(), 400 | 403 | 409 | 410)) {
        return DavError::InvalidSyncToken;
    }
    response.into_error()
}

#[cfg(test)]
mod tests {
    use reqwest::Method;
    use reqwest::header::HeaderMap;

    use super::*;

    #[test]
    fn body_contains_the_escaped_token() {
        let body = sync_collection_body(Some("http://example.com/sync/1?a=1&b=2"));
        let root = xml::parse(&body).unwrap();
        assert!(root.is(Ns::Dav, "sync-collection"));
        assert_eq!(
            root.child(Ns::Dav, "sync-token").unwrap().text(),
            "http://example.com/sync/1?a=1&b=2"
        );
        assert_eq!(root.child(Ns::Dav, "sync-level").unwrap().text(), "1");
        let initial = xml::parse(&sync_collection_body(None)).unwrap();
        assert_eq!(initial.child(Ns::Dav, "sync-token").unwrap().text(), "");
    }

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            method: Method::from_bytes(b"REPORT").unwrap(),
            status: StatusCode::from_u16(status).unwrap(),
            headers: HeaderMap::new(),
            url: Url::parse("https://dav.example.com/cal/").unwrap(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn invalid_token_detection() {
        let precondition = r#"<D:error xmlns:D="DAV:"><D:valid-sync-token/></D:error>"#;
        assert!(matches!(
            sync_error(response(403, precondition), true),
            DavError::InvalidSyncToken
        ));
        assert!(matches!(
            sync_error(response(409, precondition), false),
            DavError::InvalidSyncToken
        ));
        assert!(matches!(
            sync_error(response(403, ""), true),
            DavError::InvalidSyncToken
        ));
        assert!(matches!(
            sync_error(response(410, ""), true),
            DavError::InvalidSyncToken
        ));
        // Without a token a 403 is a real permission problem.
        assert!(matches!(
            sync_error(response(403, ""), false),
            DavError::Forbidden { .. }
        ));
        assert!(matches!(
            sync_error(response(500, ""), true),
            DavError::Http { status: 500, .. }
        ));
    }

    #[test]
    fn accumulator_keeps_the_latest_state() {
        let mut acc = SyncAccumulator::default();
        acc.add(SyncPage {
            changed: vec![
                ObjectRef {
                    href: "a".into(),
                    etag: Some("1".into()),
                },
                ObjectRef {
                    href: "b".into(),
                    etag: Some("1".into()),
                },
            ],
            removed: vec!["c".into()],
            sync_token: None,
            truncated: true,
        });
        acc.add(SyncPage {
            changed: vec![
                ObjectRef {
                    href: "a".into(),
                    etag: Some("2".into()),
                },
                ObjectRef {
                    href: "c".into(),
                    etag: Some("1".into()),
                },
            ],
            removed: vec!["b".into()],
            sync_token: None,
            truncated: false,
        });
        let delta = acc.finish(Some("t".into()), false);
        assert_eq!(
            delta.changed,
            [
                ObjectRef {
                    href: "a".into(),
                    etag: Some("2".into())
                },
                ObjectRef {
                    href: "c".into(),
                    etag: Some("1".into())
                },
            ]
        );
        assert_eq!(delta.removed, ["b"]);
        assert_eq!(delta.sync_token.as_deref(), Some("t"));
        assert!(!delta.truncated);
    }
}

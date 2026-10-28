//! End-to-end tests against a local mock of Google's HTTP APIs: paging,
//! sync tokens, `410 Gone`, token refresh on `401`, retries and the request
//! shapes of writes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use envelope_google::people::CONNECTION_PERSON_FIELDS;
use envelope_google::{
    ClientConfig, ContactSource, Error, EventPatch, EventSyncOptions, FreeBusyRequest,
    GoogleClient, GoogleEvent, RetryPolicy, SendUpdates, StaticToken, TokenSource,
    VerificationStatus, WriteOptions,
};
use envelope_ical::{Attendee, Event, EventTime, PartStat};
use serde_json::{Value, json};
use wiremock::matchers::{
    bearer_token, body_partial_json, header, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const COLORS: &[u8] = include_bytes!("fixtures/colors.json");
const CALENDAR_LIST: &[u8] = include_bytes!("fixtures/calendar_list.json");
const FULL_SYNC: &[u8] = include_bytes!("fixtures/events_full_sync.json");
const INCREMENTAL: &[u8] = include_bytes!("fixtures/events_incremental.json");
const CONNECTIONS: &[u8] = include_bytes!("fixtures/people_connections.json");
const SEND_AS: &[u8] = include_bytes!("fixtures/send_as.json");

const EVENTS_PATH: &str = "/calendar/v3/calendars/primary/events";

fn json_body(bytes: &[u8]) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(bytes.to_vec(), "application/json")
}

fn google_error(status: u16, reason: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "error": {
            "errors": [{ "domain": "global", "reason": reason, "message": message }],
            "code": status,
            "message": message
        }
    }))
}

/// One event of the full-sync fixture, as Google JSON.
fn fixture_event(id: &str) -> Value {
    let page: Value = serde_json::from_slice(FULL_SYNC).expect("fixture parses");
    page["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == json!(id)))
        .cloned()
        .unwrap_or_else(|| panic!("fixture event {id} missing"))
}

fn client_with(
    server: &MockServer,
    tokens: Arc<dyn TokenSource>,
    max_retries: u32,
) -> GoogleClient {
    let config = ClientConfig {
        retry: RetryPolicy {
            max_retries,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(4),
        },
        ..ClientConfig::with_api_root(&server.uri())
    };
    GoogleClient::with_config(tokens, config).expect("client builds")
}

fn client(server: &MockServer) -> GoogleClient {
    client_with(server, Arc::new(StaticToken::new("test-token")), 3)
}

/// Mounts `colors.get` and the primary calendar list entry used to resolve
/// colors and default reminders.
async fn mount_calendar_basics(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/calendar/v3/colors"))
        .respond_with(json_body(COLORS))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList/primary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "ada@example.com",
            "summary": "ada@example.com",
            "accessRole": "owner",
            "primary": true,
            "defaultReminders": [{ "method": "popup", "minutes": 15 }]
        })))
        .mount(server)
        .await;
}

async fn requests_to(server: &MockServer, http_method: &str, request_path: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("request recording is enabled")
        .into_iter()
        .filter(|r| r.method.as_str() == http_method && r.url.path() == request_path)
        .collect()
}

fn query(request: &Request, key: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn local(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").expect("valid local time")
}

/// A token source that hands out `stale` until invalidated, then `fresh`.
struct RefreshingToken {
    current: Mutex<String>,
    invalidations: AtomicUsize,
}

impl RefreshingToken {
    fn new() -> Arc<Self> {
        Arc::new(RefreshingToken {
            current: Mutex::new("stale".to_owned()),
            invalidations: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl TokenSource for RefreshingToken {
    async fn access_token(&self) -> Result<String, String> {
        Ok(self.current.lock().map_err(|e| e.to_string())?.clone())
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut current) = self.current.lock() {
            *current = "fresh".to_owned();
        }
    }
}

#[tokio::test]
async fn calendar_list_resolves_colors_and_access_roles() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/colors"))
        .respond_with(json_body(COLORS))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .and(query_param("showHidden", "true"))
        .and(query_param("maxResults", "250"))
        .respond_with(json_body(CALENDAR_LIST))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let calendars = client.list_calendars().await.expect("calendar list");
    assert_eq!(calendars.len(), 4);
    let primary = &calendars[0];
    assert!(primary.primary && primary.selected && !primary.read_only);
    assert_eq!(primary.time_zone.as_deref(), Some("Europe/Istanbul"));
    assert_eq!(primary.background_color.as_deref(), Some("#9fe1e7"));
    assert_eq!(primary.default_reminders.len(), 2);
    assert_eq!(primary.default_alarms()[1].minutes_before, 1440);
    assert!(primary.supports_google_meet());

    let shared = &calendars[1];
    assert_eq!(shared.display_name(), "Eng team");
    assert_eq!(shared.summary, "Engineering");
    assert!(!shared.read_only);
    assert_eq!(shared.data_owner.as_deref(), Some("grace@example.com"));

    let holidays = &calendars[2];
    assert!(holidays.read_only);
    assert_eq!(
        holidays.background_color.as_deref(),
        Some("#16a765"),
        "colorId resolved via colors.get"
    );

    let busy_only = &calendars[3];
    assert!(busy_only.hidden && busy_only.read_only);

    // The palette is cached for the lifetime of the client.
    let palette = client.colors().await.expect("palette");
    assert_eq!(palette.event["11"].background, "#dc2127");
}

#[tokio::test]
async fn full_sync_pages_until_the_sync_token() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("GET"))
        .and(path(EVENTS_PATH))
        .and(query_param("singleEvents", "false"))
        .and(query_param("maxResults", "2500"))
        .and(query_param("timeMin", "2024-01-01T00:00:00Z"))
        .and(query_param_is_missing("syncToken"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "calendar#events",
            "timeZone": "Europe/Istanbul",
            "defaultReminders": [{ "method": "popup", "minutes": 30 }],
            "nextPageToken": "page-2",
            "items": [fixture_event("allday0001aa")]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(EVENTS_PATH))
        .and(query_param("pageToken", "page-2"))
        .and(query_param("timeMin", "2024-01-01T00:00:00Z"))
        .respond_with(json_body(FULL_SYNC))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let start = Utc
        .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
        .single()
        .expect("instant");
    let mut sync = client.event_sync("primary", EventSyncOptions::full().time_min(start));
    let first = sync.next_page().await.expect("page 1").expect("has page 1");
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.next_page_token.as_deref(), Some("page-2"));
    assert_eq!(
        first.next_sync_token, None,
        "only the last page has a sync token"
    );

    let second = sync.next_page().await.expect("page 2").expect("has page 2");
    assert_eq!(second.events.len(), 7);
    assert_eq!(second.cancelled_occurrences.len(), 1);
    assert_eq!(
        second.next_sync_token.as_deref(),
        Some("CPDAlvWDx70CEPDAlvWDx70CGAU=")
    );
    assert_eq!(second.events[2].event.color, None, "event 3 has no color");
    assert_eq!(second.events[1].event.color.as_deref(), Some("#dc2127"));
    assert!(sync.next_page().await.expect("done").is_none());
}

#[tokio::test]
async fn incremental_sync_sends_the_token_and_reports_deletions() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("GET"))
        .and(path(EVENTS_PATH))
        .and(query_param("syncToken", "CPDAlvWDx70CEPDAlvWDx70CGAU="))
        .and(query_param("showDeleted", "true"))
        .and(query_param("singleEvents", "false"))
        .and(query_param_is_missing("timeMin"))
        .respond_with(json_body(INCREMENTAL))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let mut sync = client.event_sync(
        "primary",
        EventSyncOptions::incremental("CPDAlvWDx70CEPDAlvWDx70CGAU="),
    );
    let page = sync.next_page().await.expect("page").expect("has page");
    assert_eq!(page.deleted_event_ids, vec!["allday0001aa".to_owned()]);
    assert_eq!(
        page.cancelled_occurrences[0].recurring_event_id,
        "weekly0003cc"
    );
    assert_eq!(
        page.events[0].event.summary,
        "Quarterly planning (agenda updated)"
    );
    assert_eq!(
        page.next_sync_token.as_deref(),
        Some("CPDAlvWDx70CEPDAlvWDx70CGAY=")
    );
    assert!(sync.next_page().await.expect("done").is_none());
}

#[tokio::test]
async fn gone_sync_token_requires_a_full_sync() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("GET"))
        .and(path(EVENTS_PATH))
        .and(query_param("syncToken", "expired"))
        .respond_with(google_error(
            410,
            "fullSyncRequired",
            "Sync token is no longer valid, a full sync is required.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let err = client
        .events_page("primary", &EventSyncOptions::incremental("expired"), None)
        .await
        .expect_err("410 must fail");
    assert!(matches!(err, Error::SyncTokenExpired), "got {err:?}");
}

#[tokio::test]
async fn unauthorized_refreshes_the_token_once_and_retries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .and(bearer_token("stale"))
        .respond_with(google_error(401, "authError", "Invalid Credentials"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .and(bearer_token("fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": "ada@example.com",
            "messagesTotal": 51234,
            "threadsTotal": 20311,
            "historyId": "8172634"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = RefreshingToken::new();
    let client = client_with(&server, tokens.clone(), 3);

    let profile = client.gmail_profile().await.expect("profile after refresh");
    assert_eq!(profile.email_address, "ada@example.com");
    assert_eq!(profile.messages_total, 51234);
    assert_eq!(profile.history_id.as_deref(), Some("8172634"));
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn persistent_unauthorized_is_surfaced_after_one_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(google_error(401, "authError", "Invalid Credentials"))
        .expect(2)
        .mount(&server)
        .await;
    let tokens = RefreshingToken::new();
    let client = client_with(&server, tokens.clone(), 3);

    let err = client.gmail_profile().await.expect_err("must fail");
    assert!(err.is_unauthorized(), "got {err:?}");
    assert_eq!(err.api_error().and_then(|e| e.reason()), Some("authError"));
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rate_limits_are_retried_with_backoff() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/settings/sendAs"))
        .respond_with(
            google_error(429, "rateLimitExceeded", "Rate Limit Exceeded")
                .insert_header("Retry-After", "0"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/settings/sendAs"))
        .respond_with(google_error(
            403,
            "userRateLimitExceeded",
            "User Rate Limit Exceeded",
        ))
        .up_to_n_times(1)
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/settings/sendAs"))
        .respond_with(json_body(SEND_AS))
        .with_priority(3)
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let aliases = client.list_send_as().await.expect("succeeds after retries");
    assert_eq!(aliases.len(), 3);
    let primary = &aliases[0];
    assert!(primary.is_primary && primary.is_default);
    assert_eq!(primary.display_name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(primary.reply_to, None);
    assert_eq!(
        primary.signature_html.as_deref(),
        Some("<div dir=\"ltr\">Ada Lovelace<br>Analytical Engines Ltd</div>")
    );
    let personal = &aliases[1];
    assert_eq!(
        personal.reply_to.as_deref(),
        Some("replies@lovelace.example")
    );
    assert_eq!(personal.verification_status, VerificationStatus::Accepted);
    assert_eq!(personal.smtp.as_ref().map(|s| s.port), Some(587));
    let team = &aliases[2];
    assert!(team.treat_as_alias);
    assert_eq!(team.display_name, None);
    assert_eq!(team.verification_status, VerificationStatus::Pending);
}

#[tokio::test]
async fn permanent_errors_are_not_retried() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{EVENTS_PATH}/timed0002bb")))
        .respond_with(google_error(
            403,
            "forbiddenForNonOrganizer",
            "Shared properties can only be changed by the organizer of the event.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let err = client
        .patch_event(
            "primary",
            "timed0002bb",
            &EventPatch::new().guests_can_modify(true),
            &WriteOptions::default(),
        )
        .await
        .expect_err("must fail");
    assert_eq!(err.status(), Some(403));
    assert!(!err.is_rate_limited());
    assert_eq!(
        err.api_error().and_then(|e| e.reason()),
        Some("forbiddenForNonOrganizer")
    );
}

#[tokio::test]
async fn server_errors_give_up_after_the_retry_budget() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(google_error(503, "backendError", "Backend Error"))
        .expect(3)
        .mount(&server)
        .await;
    let client = client_with(&server, Arc::new(StaticToken::new("t")), 2);

    let err = client.gmail_profile().await.expect_err("must fail");
    assert_eq!(err.status(), Some(503));
}

#[tokio::test]
async fn insert_creates_a_google_meet_conference() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("POST"))
        .and(path(EVENTS_PATH))
        .and(query_param("conferenceDataVersion", "1"))
        .and(query_param("sendUpdates", "all"))
        .and(body_partial_json(json!({
            "conferenceData": { "createRequest": { "conferenceSolutionKey": { "type": "hangoutsMeet" } } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "calendar#event",
            "etag": "\"3407041218049999\"",
            "id": "new0010jj",
            "status": "confirmed",
            "summary": "Design review",
            "iCalUID": "design-review-1@envelope.example",
            "start": { "dateTime": "2024-03-04T14:00:00+03:00", "timeZone": "Europe/Istanbul" },
            "end": { "dateTime": "2024-03-04T15:00:00+03:00", "timeZone": "Europe/Istanbul" },
            "organizer": { "email": "ada@example.com", "self": true },
            "attendees": [
                { "email": "ada@example.com", "organizer": true, "self": true, "responseStatus": "accepted" },
                { "email": "grace@example.com", "responseStatus": "needsAction" }
            ],
            "hangoutLink": "https://meet.google.com/xyz-abcd-efg",
            "conferenceData": {
                "createRequest": {
                    "requestId": "ignored",
                    "conferenceSolutionKey": { "type": "hangoutsMeet" },
                    "status": { "statusCode": "success" }
                },
                "entryPoints": [{ "entryPointType": "video", "uri": "https://meet.google.com/xyz-abcd-efg" }],
                "conferenceSolution": { "key": { "type": "hangoutsMeet" }, "name": "Google Meet" },
                "conferenceId": "xyz-abcd-efg"
            },
            "reminders": { "useDefault": true },
            "eventType": "default"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let mut event = Event::new(
        "design-review-1@envelope.example",
        EventTime::Zoned {
            local: local("2024-03-04T14:00:00"),
            tzid: "Europe/Istanbul".to_owned(),
        },
        EventTime::Zoned {
            local: local("2024-03-04T15:00:00"),
            tzid: "Europe/Istanbul".to_owned(),
        },
    );
    event.summary = "Design review".to_owned();
    event.attendees = vec![Attendee {
        email: "grace@example.com".to_owned(),
        ..Attendee::default()
    }];
    let options = WriteOptions::default()
        .send_updates(SendUpdates::All)
        .with_google_meet();
    let created = client
        .insert_event("primary", &GoogleEvent::new(event), &options)
        .await
        .expect("insert");
    assert_eq!(created.id, "new0010jj");
    assert_eq!(
        created.event.conference_url.as_deref(),
        Some("https://meet.google.com/xyz-abcd-efg")
    );
    let conference = created.conference.as_ref().expect("conference");
    assert_eq!(conference.create_status.as_deref(), Some("success"));
    // Default reminders resolved through the calendar list entry.
    assert_eq!(created.event.alarms.len(), 1);
    assert_eq!(created.event.alarms[0].minutes_before, 15);

    let sent = requests_to(&server, "POST", EVENTS_PATH).await;
    let body: Value = sent[0].body_json().expect("json body");
    assert!(body.get("id").is_none(), "Google assigns the id");
    assert_eq!(body["iCalUID"], json!("design-review-1@envelope.example"));
    assert_eq!(
        body["start"],
        json!({"date": null, "dateTime": "2024-03-04T14:00:00+03:00", "timeZone": "Europe/Istanbul"})
    );
    assert_eq!(body["reminders"], json!({"useDefault": true}));
    assert_eq!(
        body["attendees"],
        json!([{"email": "grace@example.com", "responseStatus": "needsAction"}])
    );
    let request_id = body["conferenceData"]["createRequest"]["requestId"]
        .as_str()
        .expect("request id");
    assert!(!request_id.is_empty());
    assert_eq!(
        sent[0].headers.get("content-type").map(|v| v.as_bytes()),
        Some(&b"application/json; charset=utf-8"[..])
    );
}

#[tokio::test]
async fn rsvp_patches_only_the_own_attendee() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    let event_path = format!("{EVENTS_PATH}/timed0002bb");
    Mock::given(method("GET"))
        .and(path(event_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture_event("timed0002bb")))
        .expect(1)
        .mount(&server)
        .await;
    let mut updated = fixture_event("timed0002bb");
    updated["attendees"][1]["responseStatus"] = json!("accepted");
    Mock::given(method("PATCH"))
        .and(path(event_path.as_str()))
        .and(query_param("sendUpdates", "none"))
        .respond_with(ResponseTemplate::new(200).set_body_json(updated))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let answered = client
        .respond_to_event(
            "primary",
            "timed0002bb",
            PartStat::Accepted,
            None,
            &WriteOptions::default().send_updates(SendUpdates::None),
        )
        .await
        .expect("rsvp");
    assert_eq!(answered.self_response(), Some(PartStat::Accepted));

    let patches = requests_to(&server, "PATCH", &event_path).await;
    let body: Value = patches[0].body_json().expect("json body");
    assert_eq!(
        body,
        json!({
            "attendeesOmitted": true,
            "attendees": [{
                "email": "ada@example.com",
                "displayName": "Ada Lovelace",
                "comment": "Might be late",
                "responseStatus": "accepted"
            }]
        })
    );
}

#[tokio::test]
async fn rsvp_requires_being_invited() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{EVENTS_PATH}/allday0001aa")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture_event("allday0001aa")))
        .mount(&server)
        .await;
    let client = client(&server);

    let err = client
        .respond_to_event(
            "primary",
            "allday0001aa",
            PartStat::Declined,
            Some("Sorry"),
            &WriteOptions::default(),
        )
        .await
        .expect_err("not an attendee");
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
}

#[tokio::test]
async fn patch_sends_if_match_and_explicit_nulls() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    let event_path = format!("{EVENTS_PATH}/timed0002bb");
    Mock::given(method("PATCH"))
        .and(path(event_path.as_str()))
        .and(header("if-match", "\"3407041218044001\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture_event("timed0002bb")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(event_path.as_str()))
        .and(header("if-match", "\"stale-etag\""))
        .respond_with(google_error(412, "conditionNotMet", "Precondition Failed"))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let patch = EventPatch::new()
        .summary("Quarterly planning")
        .location(None)
        .start(EventTime::Utc {
            at: local("2024-01-15T14:00:00").and_utc(),
        });
    client
        .patch_event(
            "primary",
            "timed0002bb",
            &patch,
            &WriteOptions::default().if_match("\"3407041218044001\""),
        )
        .await
        .expect("patch");
    let sent = requests_to(&server, "PATCH", &event_path).await;
    let body: Value = sent[0].body_json().expect("json body");
    assert_eq!(
        body,
        json!({
            "summary": "Quarterly planning",
            "location": null,
            "start": {"date": null, "dateTime": "2024-01-15T14:00:00Z", "timeZone": null}
        })
    );
    assert_eq!(query(&sent[0], "conferenceDataVersion"), None);

    let err = client
        .patch_event(
            "primary",
            "timed0002bb",
            &patch,
            &WriteOptions::default().if_match("\"stale-etag\""),
        )
        .await
        .expect_err("etag mismatch");
    assert!(err.is_precondition_failed(), "got {err:?}");
}

#[tokio::test]
async fn quick_add_move_update_and_delete() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{EVENTS_PATH}/quickAdd")))
        .and(query_param("text", "Lunch with Ana tomorrow 12:30"))
        .and(header("content-length", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture_event("timed0002bb")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{EVENTS_PATH}/timed0002bb/move")))
        .and(query_param(
            "destination",
            "c_8f2a6b1e@group.calendar.google.com",
        ))
        .and(query_param("sendUpdates", "externalOnly"))
        .and(header("content-length", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture_event("timed0002bb")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/calendar/v3/users/me/calendarList/c_8f2a6b1e@group.calendar.google.com",
        ))
        .respond_with(google_error(404, "notFound", "Not Found"))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{EVENTS_PATH}/weekly0003cc")))
        .and(header("if-match", "\"3407041218044002\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture_event("weekly0003cc")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{EVENTS_PATH}/ooo0006ff")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{EVENTS_PATH}/allday0001aa")))
        .respond_with(google_error(410, "deleted", "Resource has been deleted"))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let added = client
        .quick_add(
            "primary",
            "Lunch with Ana tomorrow 12:30",
            &WriteOptions::default(),
        )
        .await
        .expect("quick add");
    assert_eq!(added.id, "timed0002bb");

    // The destination calendar is not in the list: mapping still succeeds.
    let moved = client
        .move_event(
            "primary",
            "timed0002bb",
            "c_8f2a6b1e@group.calendar.google.com",
            &WriteOptions::default().send_updates(SendUpdates::ExternalOnly),
        )
        .await
        .expect("move");
    assert!(moved.event.alarms.is_empty());

    let page = envelope_google::EventPage::from_json(FULL_SYNC, None).expect("fixture");
    let master = page
        .events
        .iter()
        .find(|e| e.id == "weekly0003cc")
        .expect("master");
    let etag = master.etag.clone().expect("etag");
    client
        .update_event("primary", master, &WriteOptions::default().if_match(etag))
        .await
        .expect("update");
    let puts = requests_to(&server, "PUT", &format!("{EVENTS_PATH}/weekly0003cc")).await;
    let body: Value = puts[0].body_json().expect("json body");
    assert_eq!(
        body["recurrence"][0],
        json!("RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20240325T055959Z")
    );
    assert_eq!(query(&puts[0], "conferenceDataVersion"), None);

    client
        .delete_event("primary", "ooo0006ff", &WriteOptions::default())
        .await
        .expect("delete");
    client
        .delete_event("primary", "allday0001aa", &WriteOptions::default())
        .await
        .expect("deleting an already deleted event succeeds");

    let unsaved = GoogleEvent::new(master.event.clone());
    let err = client
        .update_event("primary", &unsaved, &WriteOptions::default())
        .await
        .expect_err("update needs an id");
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[tokio::test]
async fn find_instance_uses_the_original_start() {
    let server = MockServer::start().await;
    mount_calendar_basics(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{EVENTS_PATH}/weekly0003cc/instances")))
        .and(query_param("originalStart", "2024-01-29T06:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "calendar#events",
            "timeZone": "Europe/Istanbul",
            "defaultReminders": [],
            "items": [{
                "id": "weekly0003cc_20240129T060000Z",
                "status": "confirmed",
                "summary": "Team sync",
                "start": { "dateTime": "2024-01-29T09:00:00+03:00", "timeZone": "Europe/Istanbul" },
                "end": { "dateTime": "2024-01-29T09:30:00+03:00", "timeZone": "Europe/Istanbul" },
                "recurringEventId": "weekly0003cc",
                "originalStartTime": { "dateTime": "2024-01-29T09:00:00+03:00", "timeZone": "Europe/Istanbul" },
                "iCalUID": "weekly0003cc@google.com"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let occurrence = EventTime::Zoned {
        local: local("2024-01-29T09:00:00"),
        tzid: "Europe/Istanbul".to_owned(),
    };
    let instance = client
        .find_instance("primary", "weekly0003cc", &occurrence)
        .await
        .expect("lookup")
        .expect("found");
    assert_eq!(instance.id, "weekly0003cc_20240129T060000Z");
    assert_eq!(instance.event.recurrence_id, Some(occurrence));
}

#[tokio::test]
async fn free_busy_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/calendar/v3/freeBusy"))
        .and(body_partial_json(json!({
            "timeMin": "2024-03-04T00:00:00Z",
            "timeMax": "2024-03-05T00:00:00Z",
            "items": [{ "id": "grace@example.com" }, { "id": "nobody@example.com" }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "calendar#freeBusy",
            "timeMin": "2024-03-04T00:00:00.000Z",
            "timeMax": "2024-03-05T00:00:00.000Z",
            "calendars": {
                "grace@example.com": {
                    "busy": [{ "start": "2024-03-04T09:00:00Z", "end": "2024-03-04T10:30:00Z" }]
                },
                "nobody@example.com": {
                    "errors": [{ "domain": "global", "reason": "notFound" }],
                    "busy": []
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let at = |h| {
        Utc.with_ymd_and_hms(2024, 3, 4, h, 0, 0)
            .single()
            .expect("instant")
    };
    let result = client
        .free_busy(&FreeBusyRequest {
            time_min: at(0),
            time_max: Utc
                .with_ymd_and_hms(2024, 3, 5, 0, 0, 0)
                .single()
                .expect("instant"),
            calendar_ids: vec![
                "grace@example.com".to_owned(),
                "nobody@example.com".to_owned(),
            ],
        })
        .await
        .expect("free/busy");
    let grace = &result.calendars["grace@example.com"];
    assert_eq!(grace.busy.len(), 1);
    assert_eq!(grace.busy[0].start, at(9));
    assert_eq!(
        result.calendars["nobody@example.com"].errors,
        vec!["notFound".to_owned()]
    );
}

#[tokio::test]
async fn people_connections_sync_with_paging_and_expiry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .and(query_param("personFields", CONNECTION_PERSON_FIELDS))
        .and(query_param("requestSyncToken", "true"))
        .and(query_param("pageSize", "1000"))
        .and(query_param_is_missing("syncToken"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(json_body(CONNECTIONS))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .and(query_param("pageToken", "connections-page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [{
                "resourceName": "people/c1618033988749894848",
                "names": [{ "metadata": { "primary": true }, "displayName": "Linus Pauling" }],
                "emailAddresses": [{ "value": "linus@example.com", "type": "home" }]
            }],
            "nextSyncToken": "people-sync-1",
            "totalItems": 3
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .and(query_param("syncToken", "people-sync-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [{
                "resourceName": "people/c2718281828459045235",
                "etag": "%EgcBAgkLLjc9GgQBAgUHIgxNQnB0TG5QdW1ZST0=",
                "metadata": { "deleted": true }
            }],
            "nextSyncToken": "people-sync-2"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .and(query_param("syncToken", "week-old"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": 400,
                "message": "Sync token is expired. Clear local cache and retry call without the sync token.",
                "status": "FAILED_PRECONDITION",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "EXPIRED_SYNC_TOKEN",
                    "domain": "people.googleapis.com"
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let mut sync = client.contacts_sync(ContactSource::Connections, None);
    let mut contacts = Vec::new();
    let mut sync_token = None;
    while let Some(page) = sync.next_page().await.expect("page") {
        contacts.extend(page.contacts);
        if page.next_sync_token.is_some() {
            sync_token = page.next_sync_token;
        }
    }
    assert_eq!(contacts.len(), 3);
    assert_eq!(sync_token.as_deref(), Some("people-sync-1"));
    assert_eq!(
        contacts[0].etag.as_deref(),
        Some("%EgcBAgkLLjc9GgQBAgUHIgxzT0h6SFhXc2ZjYz0=")
    );
    assert_eq!(
        contacts[0].person["resourceName"],
        json!("people/c3141592653589793238")
    );
    let ada = &contacts[0].contact;
    assert_eq!(ada.uid, "people/c3141592653589793238");
    assert_eq!(ada.name, "Ada Lovelace");
    assert_eq!(ada.nickname.as_deref(), Some("Countess"));
    assert_eq!(ada.emails.len(), 2);
    assert!(ada.emails[0].preferred);
    assert_eq!(ada.phones[0].kind.as_deref(), Some("cell"));
    assert_eq!(ada.organization.as_deref(), Some("Analytical Engines Ltd"));
    assert_eq!(
        ada.note.as_deref(),
        Some("Wrote the first published program.")
    );
    assert!(ada.birthday.is_some());
    assert_eq!(
        ada.photo.as_deref(),
        Some("https://lh3.googleusercontent.com/contacts/AdaPhoto=s100")
    );
    assert_eq!(
        contacts[1].contact.photo, None,
        "default avatars are ignored"
    );

    let incremental = client
        .contacts_page(ContactSource::Connections, Some("people-sync-1"), None)
        .await
        .expect("incremental page");
    assert_eq!(
        incremental.deleted,
        vec!["people/c2718281828459045235".to_owned()]
    );
    assert!(incremental.contacts.is_empty());

    let err = client
        .contacts_page(ContactSource::Connections, Some("week-old"), None)
        .await
        .expect_err("expired");
    assert!(matches!(err, Error::SyncTokenExpired), "got {err:?}");
}

#[tokio::test]
async fn other_contacts_and_search_with_warm_up() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/otherContacts"))
        .and(query_param("readMask", "names,emailAddresses,metadata"))
        .and(query_param("requestSyncToken", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "otherContacts": [{
                "resourceName": "otherContacts/c5772156649015328606",
                "emailAddresses": [{ "metadata": { "primary": true }, "value": "katherine@example.com" }]
            }],
            "nextSyncToken": "other-sync-1",
            "totalSize": 1
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people:searchContacts"))
        .and(query_param("query", ""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people:searchContacts"))
        .and(query_param("query", "ada"))
        .and(query_param("pageSize", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "person": {
                "resourceName": "people/c3141592653589793238",
                "names": [{ "displayName": "Ada Lovelace" }],
                "emailAddresses": [{ "value": "ada@example.com" }]
            }}]
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/otherContacts:search"))
        .and(query_param("query", ""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/otherContacts:search"))
        .and(query_param("query", "kat"))
        .and(query_param("readMask", "names,emailAddresses,phoneNumbers"))
        .and(query_param("pageSize", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{ "person": {
                "resourceName": "otherContacts/c5772156649015328606",
                "emailAddresses": [{ "value": "katherine@example.com" }]
            }}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let page = client
        .contacts_page(ContactSource::OtherContacts, None, None)
        .await
        .expect("other contacts");
    assert_eq!(page.contacts[0].contact.name, "katherine@example.com");
    assert_eq!(page.next_sync_token.as_deref(), Some("other-sync-1"));
    assert_eq!(page.total_items, Some(1));

    for _ in 0..2 {
        let found = client.search_contacts("ada", 10).await.expect("search");
        assert_eq!(found[0].name, "Ada Lovelace");
    }
    let searches = requests_to(&server, "GET", "/v1/people:searchContacts").await;
    assert_eq!(
        query(&searches[0], "query").as_deref(),
        Some(""),
        "warm-up first"
    );

    let others = client
        .search_other_contacts("kat", 99)
        .await
        .expect("search other contacts");
    assert_eq!(others[0].emails[0].value, "katherine@example.com");
}

#[tokio::test]
async fn contact_writes() {
    let server = MockServer::start().await;
    let ada = json!({
        "resourceName": "people/c1",
        "etag": "%E1",
        "names": [{ "metadata": { "primary": true }, "displayName": "Ada Lovelace", "givenName": "Ada", "familyName": "Lovelace" }],
        "emailAddresses": [{ "metadata": { "primary": true }, "value": "ada@example.com", "type": "work", "formattedType": "Work" }]
    });
    Mock::given(method("POST"))
        .and(path("/v1/people:createContact"))
        .and(query_param("personFields", CONNECTION_PERSON_FIELDS))
        .and(body_partial_json(json!({
            "names": [{ "givenName": "Ada", "familyName": "Lovelace" }],
            "emailAddresses": [{ "value": "ada@example.com", "type": "work" }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ada.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut updated = ada.clone();
    updated["etag"] = json!("%E2");
    updated["phoneNumbers"] = json!([{ "value": "+44 20 1234", "type": "mobile" }]);
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContact"))
        .and(query_param("updatePersonFields", "phoneNumbers"))
        .and(body_partial_json(json!({
            "resourceName": "people/c1",
            "etag": "%E1",
            "phoneNumbers": [{ "value": "+44 20 1234", "type": "mobile" }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(updated.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContactPhoto"))
        .and(body_partial_json(json!({ "photoBytes": "AQID" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "person": {
            "resourceName": "people/c1",
            "etag": "%E3",
            "photos": [{ "url": "https://lh3.googleusercontent.com/contacts/new=s100" }]
        }})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/people/c1:deleteContactPhoto"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "person": {
            "resourceName": "people/c1",
            "etag": "%E4",
            "photos": [{ "url": "https://lh3.googleusercontent.com/cm/default=s100", "default": true }]
        }})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/people/c1:deleteContact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people/c1"))
        .and(query_param("personFields", CONNECTION_PERSON_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(updated))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let new = envelope_ical::Contact {
        name: "Ada Lovelace".into(),
        given_name: Some("Ada".into()),
        family_name: Some("Lovelace".into()),
        emails: vec![envelope_ical::ContactField {
            value: "ada@example.com".into(),
            kind: Some("work".into()),
            preferred: false,
        }],
        ..Default::default()
    };
    let created = client.create_contact(&new).await.expect("create");
    assert_eq!(created.contact.uid, "people/c1");
    assert_eq!(created.etag.as_deref(), Some("%E1"));

    // Nothing changed: no request.
    let same = client
        .update_contact(&created, &created.contact)
        .await
        .expect("no-op update");
    assert_eq!(same, created);

    let mut edited = created.contact.clone();
    edited.phones.push(envelope_ical::ContactField {
        value: "+44 20 1234".into(),
        kind: Some("cell".into()),
        preferred: false,
    });
    let saved = client
        .update_contact(&created, &edited)
        .await
        .expect("update");
    assert_eq!(saved.etag.as_deref(), Some("%E2"));
    assert_eq!(saved.contact.phones[0].kind.as_deref(), Some("cell"));
    let body: Value = requests_to(&server, "PATCH", "/v1/people/c1:updateContact").await[0]
        .body_json()
        .expect("json body");
    assert!(
        body.get("emailAddresses").is_none(),
        "unchanged fields are not sent"
    );

    let with_photo = client
        .update_contact_photo("people/c1", &[1, 2, 3])
        .await
        .expect("photo");
    assert_eq!(
        with_photo.contact.photo.as_deref(),
        Some("https://lh3.googleusercontent.com/contacts/new=s100")
    );
    let without = client
        .delete_contact_photo("people/c1")
        .await
        .expect("photo removal");
    assert_eq!(without.contact.photo, None);
    assert_eq!(without.etag.as_deref(), Some("%E4"));

    let fetched = client.get_contact("people/c1").await.expect("get");
    assert_eq!(fetched.etag.as_deref(), Some("%E2"));
    client.delete_contact("people/c1").await.expect("delete");
    assert!(client.delete_contact("otherContacts/c1").await.is_err());
}

#[tokio::test]
async fn stale_contact_etags_are_recognised() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContact"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": 400,
                "message": "Request person.etag is different than the current person.etag. Clear local cache and get the latest person.",
                "status": "FAILED_PRECONDITION"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/people/c2:deleteContact"))
        .respond_with(google_error(
            403,
            "insufficientPermissions",
            "Request had insufficient authentication scopes.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let base = envelope_google::GoogleContact {
        contact: envelope_ical::Contact {
            uid: "people/c1".into(),
            name: "A".into(),
            ..Default::default()
        },
        etag: Some("%old".into()),
        person: json!({ "resourceName": "people/c1" }),
    };
    let mut edited = base.contact.clone();
    edited.note = Some("changed".into());
    let err = client
        .update_contact(&base, &edited)
        .await
        .expect_err("stale");
    assert!(err.is_stale_etag(), "got {err:?}");
    assert!(!err.is_forbidden());
    let err = client
        .delete_contact("people/c2")
        .await
        .expect_err("read-only grant");
    assert!(err.is_forbidden(), "got {err:?}");
    assert!(!err.is_stale_etag());
}

#[test]
fn public_futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    let client = GoogleClient::new(Arc::new(StaticToken::new("t"))).expect("client");
    let options = EventSyncOptions::full();
    let event = GoogleEvent::new(Event::new(
        "uid",
        EventTime::Utc {
            at: local("2024-01-01T09:00:00").and_utc(),
        },
        EventTime::Utc {
            at: local("2024-01-01T10:00:00").and_utc(),
        },
    ));
    let write = WriteOptions::default();
    let patch = EventPatch::new();
    let mut sync = client.event_sync("primary", options.clone());
    let mut contacts = client.contacts_sync(ContactSource::Connections, None);
    assert_send(&client.colors());
    assert_send(&client.list_calendars());
    assert_send(&client.get_calendar("primary"));
    assert_send(&client.events_page("primary", &options, None));
    assert_send(&sync.next_page());
    assert_send(&client.get_event("primary", "id"));
    assert_send(&client.insert_event("primary", &event, &write));
    assert_send(&client.update_event("primary", &event, &write));
    assert_send(&client.patch_event("primary", "id", &patch, &write));
    assert_send(&client.delete_event("primary", "id", &write));
    assert_send(&client.move_event("primary", "id", "other", &write));
    assert_send(&client.respond_to_event("primary", "id", PartStat::Accepted, None, &write));
    assert_send(&client.quick_add("primary", "text", &write));
    assert_send(&client.list_instances("primary", "id", &Default::default()));
    assert_send(&client.find_instance("primary", "id", &event.event.start));
    assert_send(&client.free_busy(&FreeBusyRequest {
        time_min: Utc::now(),
        time_max: Utc::now(),
        calendar_ids: Vec::new(),
    }));
    assert_send(&client.contacts_page(ContactSource::OtherContacts, None, None));
    assert_send(&contacts.next_page());
    assert_send(&client.search_contacts("a", 5));
    assert_send(&client.search_other_contacts("a", 5));
    assert_send(&client.warm_up_contact_search());
    let contact = envelope_ical::Contact::default();
    let saved = envelope_google::GoogleContact::default();
    assert_send(&client.get_contact("people/c1"));
    assert_send(&client.create_contact(&contact));
    assert_send(&client.update_contact(&saved, &contact));
    assert_send(&client.delete_contact("people/c1"));
    assert_send(&client.update_contact_photo("people/c1", b"x"));
    assert_send(&client.delete_contact_photo("people/c1"));
    assert_send(&client.list_send_as());
    assert_send(&client.gmail_profile());
}

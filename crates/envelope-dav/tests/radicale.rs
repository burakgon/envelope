//! Integration tests against a live [Radicale](https://radicale.org) 3 server.
//!
//! They are skipped (and pass) unless `ENVELOPE_TEST_RADICALE_URL` is set.
//! The server must accept any credentials (`--auth-type none`); every test
//! works as its own freshly generated user, so tests are independent of each
//! other and of earlier runs. The easiest way to run them is
//! `crates/envelope-dav/scripts/radicale-tests.sh`, which starts a throwaway
//! server on 127.0.0.1:15232, runs this file and stops the server again.
//! Manually:
//!
//! ```text
//! radicale --config "" --server-hosts 127.0.0.1:15232 \
//!     --storage-filesystem-folder "$(mktemp -d)" --auth-type none &
//! ENVELOPE_TEST_RADICALE_URL=http://127.0.0.1:15232/ cargo test -p envelope-dav --test radicale
//! ```

use chrono::{TimeZone, Utc};
use envelope_dav::{
    AddressBookPatch, Auth, CalendarPatch, ComponentSet, DavClient, DavError, NewAddressBook,
    NewCalendar, ObjectKind, Precondition, ServiceType, object_href,
};

/// A client for a fresh user, or `None` when no server is configured.
fn session() -> Option<(DavClient, String)> {
    let base = std::env::var("ENVELOPE_TEST_RADICALE_URL").ok()?;
    if base.trim().is_empty() {
        return None;
    }
    let user = format!("envtest-{}", uuid::Uuid::new_v4().simple());
    let client = DavClient::new(&base, Auth::basic(&user, "any-password")).expect("client");
    Some((client, user))
}

macro_rules! require_radicale {
    () => {
        match session() {
            Some(session) => session,
            None => {
                eprintln!(
                    "skipping: set ENVELOPE_TEST_RADICALE_URL to run the Radicale integration tests"
                );
                return;
            }
        }
    };
}

fn event(uid: &str, summary: &str, start: &str, end: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Envelope//Integration tests//EN\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:{start}\r\nDTEND:{end}\r\n\
         SUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn vcard(uid: &str, name: &str, email: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nN:{name};;;;\r\n\
         EMAIL;TYPE=INTERNET:{email}\r\nEND:VCARD\r\n"
    )
}

async fn calendar_home(client: &DavClient) -> String {
    let discovery = client
        .discover(ServiceType::CalDav)
        .await
        .expect("CalDAV discovery");
    discovery.calendar_home_set[0].clone()
}

#[tokio::test]
async fn discovery_finds_principal_and_home_sets() {
    let (client, user) = require_radicale!();
    let base = client.base_url().to_string();
    let principal = format!("{base}{user}/");

    let caldav = client
        .discover(ServiceType::CalDav)
        .await
        .expect("CalDAV discovery");
    assert_eq!(caldav.service, ServiceType::CalDav);
    // `/.well-known/caldav` redirects to the root, which knows the principal.
    assert_eq!(caldav.context_url, base);
    assert_eq!(caldav.principal, principal);
    assert_eq!(caldav.calendar_home_set, [principal.as_str()]);
    assert_eq!(caldav.addressbook_home_set, [principal.as_str()]);
    // Radicale lists the principal path, not mailto: addresses.
    assert!(caldav.calendar_user_addresses.is_empty());

    let carddav = client
        .discover(ServiceType::CardDav)
        .await
        .expect("CardDAV discovery");
    assert_eq!(carddav.home_set(), [principal.as_str()]);

    // Starting from an explicit path works as well.
    let from_path = client
        .discover_from(&principal, ServiceType::CalDav)
        .await
        .expect("discovery from the principal URL");
    assert_eq!(from_path.principal, principal);
    assert_eq!(from_path.context_url, principal);

    // A fresh account has no collections yet.
    assert!(
        client
            .list_calendars(&principal)
            .await
            .expect("list")
            .is_empty()
    );
    assert!(
        client
            .list_addressbooks(&principal)
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn calendar_collections_can_be_created_renamed_and_deleted() {
    let (client, _) = require_radicale!();
    let home = calendar_home(&client).await;

    let href = client
        .create_calendar(
            &home,
            &NewCalendar::new("Work & Life")
                .with_color("#FF2968")
                .with_description("Integration test <calendar>"),
        )
        .await
        .expect("MKCALENDAR");
    assert!(href.starts_with(&home));

    let calendars = client.list_calendars(&home).await.expect("list");
    assert_eq!(calendars.len(), 1);
    let calendar = &calendars[0];
    assert_eq!(calendar.href, href);
    assert_eq!(calendar.display_name.as_deref(), Some("Work & Life"));
    assert_eq!(calendar.color.as_deref(), Some("#ff2968"));
    assert_eq!(
        calendar.description.as_deref(),
        Some("Integration test <calendar>")
    );
    assert_eq!(calendar.components, ComponentSet::EVENTS);
    assert!(!calendar.read_only);
    assert!(calendar.supports_sync);
    assert!(calendar.sync_token.is_some());
    assert!(calendar.ctag.is_some());

    client
        .update_calendar(
            &href,
            &CalendarPatch {
                display_name: Some("Renamed".into()),
                color: Some("#00aa55".into()),
                description: Some("Updated".into()),
                order: Some(5),
            },
        )
        .await
        .expect("PROPPATCH");
    let calendar = client.get_calendar(&href).await.expect("get calendar");
    assert_eq!(calendar.display_name.as_deref(), Some("Renamed"));
    assert_eq!(calendar.color.as_deref(), Some("#00aa55"));
    assert_eq!(calendar.description.as_deref(), Some("Updated"));
    assert_eq!(calendar.order, Some(5));

    let tasks = client
        .create_calendar(
            &home,
            &NewCalendar::new("Todo").with_components(ComponentSet::TASKS),
        )
        .await
        .expect("MKCALENDAR for tasks");
    assert_eq!(
        client.get_calendar(&tasks).await.expect("tasks").components,
        ComponentSet::TASKS
    );
    assert_eq!(client.list_calendars(&home).await.expect("list").len(), 2);

    client
        .delete_collection(&href)
        .await
        .expect("delete calendar");
    let remaining = client.list_calendars(&home).await.expect("list");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].href, tasks);
    assert!(matches!(
        client.get_calendar(&href).await,
        Err(DavError::NotFound { .. })
    ));
    assert!(matches!(
        client.delete_collection(&href).await,
        Err(DavError::NotFound { .. })
    ));
}

#[tokio::test]
async fn calendar_objects_sync_roundtrip() {
    let (client, _) = require_radicale!();
    let home = calendar_home(&client).await;
    let calendar = client
        .create_calendar(&home, &NewCalendar::new("Sync"))
        .await
        .expect("MKCALENDAR");

    let initial = client
        .sync_collection(&calendar, None)
        .await
        .expect("initial sync");
    assert!(initial.changed.is_empty());
    assert!(initial.removed.is_empty());
    let token0 = initial.sync_token.expect("sync token");

    // Create two events; the hrefs contain characters servers like to encode.
    let uid1 = "event-1@example.com";
    let href1 = object_href(&calendar, uid1, ObjectKind::Calendar).expect("href");
    assert!(href1.ends_with("/event-1@example.com.ics"));
    let etag1 = client
        .put(
            &href1,
            &event(uid1, "Kickoff", "20240105T100000Z", "20240105T110000Z"),
            Precondition::IfNoneMatch,
        )
        .await
        .expect("create event 1")
        .expect("etag for event 1");
    let uid2 = "event 2/with odd chars";
    let href2 = object_href(&calendar, uid2, ObjectKind::Calendar).expect("href");
    let etag2 = client
        .put(
            &href2,
            &event(uid2, "Retro", "20240310T090000Z", "20240310T100000Z"),
            Precondition::IfNoneMatch,
        )
        .await
        .expect("create event 2")
        .expect("etag for event 2");

    // Creating an existing resource is refused.
    let err = client
        .put(
            &href1,
            &event(uid1, "Duplicate", "20240105T100000Z", "20240105T110000Z"),
            Precondition::IfNoneMatch,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DavError::PreconditionFailed { .. }),
        "{err:?}"
    );

    // The delta reports both, with the same hrefs and tags we wrote.
    let delta = client
        .sync_collection(&calendar, Some(&token0))
        .await
        .expect("delta sync");
    let mut changed: Vec<(String, Option<String>)> = delta
        .changed
        .iter()
        .map(|o| (o.href.clone(), o.etag.clone()))
        .collect();
    changed.sort();
    let mut expected = vec![
        (href1.clone(), Some(etag1.clone())),
        (href2.clone(), Some(etag2.clone())),
    ];
    expected.sort();
    assert_eq!(changed, expected);
    assert!(delta.removed.is_empty());
    assert!(!delta.truncated);
    let token1 = delta.sync_token.expect("sync token");
    assert_ne!(token1, token0);

    // Fetch them in one multiget; unknown hrefs are reported as missing.
    let missing = object_href(&calendar, "does-not-exist", ObjectKind::Calendar).expect("href");
    let fetched = client
        .multiget(
            &calendar,
            &[href1.as_str(), href2.as_str(), missing.as_str()],
        )
        .await
        .expect("multiget");
    assert_eq!(fetched.objects.len(), 2);
    assert_eq!(fetched.missing, [missing.as_str()]);
    let first = fetched
        .objects
        .iter()
        .find(|o| o.href == href1)
        .expect("event 1 fetched");
    assert_eq!(first.etag.as_deref(), Some(etag1.as_str()));
    assert!(first.data.contains("UID:event-1@example.com"));
    assert!(first.data.contains("SUMMARY:Kickoff"));
    assert!(first.data.starts_with("BEGIN:VCALENDAR"));

    // Updates are conditional on the current tag.
    let updated = event(
        uid1,
        "Kickoff (moved)",
        "20240106T100000Z",
        "20240106T110000Z",
    );
    let etag1b = client
        .put(&href1, &updated, Precondition::IfMatch(etag1.clone()))
        .await
        .expect("update event 1")
        .expect("new etag");
    assert_ne!(etag1b, etag1);
    let err = client
        .put(&href1, &updated, Precondition::IfMatch(etag1.clone()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DavError::PreconditionFailed { .. }),
        "{err:?}"
    );

    // Time-range query: only the January event overlaps January.
    let january = client
        .query_time_range(
            &calendar,
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap(),
        )
        .await
        .expect("calendar-query");
    assert_eq!(january.len(), 1);
    assert_eq!(january[0].href, href1);
    assert!(january[0].data.contains("SUMMARY:Kickoff (moved)"));

    // Deletion is conditional too.
    let err = client.delete(&href2, Some(&etag1)).await.unwrap_err();
    assert!(
        matches!(err, DavError::PreconditionFailed { .. }),
        "{err:?}"
    );
    client
        .delete(&href2, Some(&etag2))
        .await
        .expect("delete event 2");
    assert!(matches!(
        client.delete(&href2, None).await,
        Err(DavError::NotFound { .. })
    ));

    // The next delta has the update and the deletion.
    let delta = client
        .sync_collection(&calendar, Some(&token1))
        .await
        .expect("delta sync");
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.changed[0].href, href1);
    assert_eq!(delta.changed[0].etag.as_deref(), Some(etag1b.as_str()));
    assert_eq!(delta.removed, [href2.as_str()]);
    let token2 = delta.sync_token.expect("sync token");

    // Nothing changed since.
    let idle = client
        .sync_collection(&calendar, Some(&token2))
        .await
        .expect("idle sync");
    assert!(idle.changed.is_empty() && idle.removed.is_empty());

    // Single-resource helpers and the PROPFIND fallback agree.
    let object = client.get(&href1).await.expect("GET");
    assert_eq!(object.etag.as_deref(), Some(etag1b.as_str()));
    assert!(object.data.contains("Kickoff (moved)"));
    assert_eq!(
        client.get_etag(&href1).await.expect("etag"),
        Some(etag1b.clone())
    );
    let listed = client
        .list_etags(&calendar)
        .await
        .expect("PROPFIND listing");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].href, href1);
    assert_eq!(listed[0].etag.as_deref(), Some(etag1b.as_str()));

    // A token the server never issued forces a full resync.
    let err = client
        .sync_collection(&calendar, Some("http://radicale.org/ns/sync/0000"))
        .await
        .unwrap_err();
    assert!(matches!(err, DavError::InvalidSyncToken), "{err:?}");
}

#[tokio::test]
async fn concurrent_writes_from_cloned_clients() {
    let (client, _) = require_radicale!();
    let home = calendar_home(&client).await;
    let calendar = client
        .create_calendar(&home, &NewCalendar::new("Concurrent"))
        .await
        .expect("MKCALENDAR");

    let mut tasks = Vec::new();
    for i in 0..8 {
        let client = client.clone();
        let calendar = calendar.clone();
        tasks.push(tokio::spawn(async move {
            let uid = format!("concurrent-{i}");
            let href = object_href(&calendar, &uid, ObjectKind::Calendar).expect("href");
            let body = event(
                &uid,
                &format!("Event {i}"),
                "20240201T080000Z",
                "20240201T090000Z",
            );
            client
                .put(&href, &body, Precondition::IfNoneMatch)
                .await
                .map(|etag| (href, etag))
        }));
    }
    let mut hrefs = Vec::new();
    for task in tasks {
        let (href, etag) = task.await.expect("task").expect("put");
        assert!(etag.is_some());
        hrefs.push(href);
    }
    let delta = client.sync_collection(&calendar, None).await.expect("sync");
    assert_eq!(delta.changed.len(), 8);
    let fetched = client.multiget(&calendar, &hrefs).await.expect("multiget");
    assert_eq!(fetched.objects.len(), 8);
    assert!(fetched.missing.is_empty());
}

#[tokio::test]
async fn addressbook_roundtrip() {
    let (client, _) = require_radicale!();
    let discovery = client
        .discover(ServiceType::CardDav)
        .await
        .expect("CardDAV discovery");
    let home = discovery.addressbook_home_set[0].clone();

    let book = client
        .create_addressbook(
            &home,
            &NewAddressBook::new("Friends").with_description("People & pets"),
        )
        .await
        .expect("extended MKCOL");
    // Calendars and address books share the Radicale home; each listing only
    // returns its own kind.
    client
        .create_calendar(&home, &NewCalendar::new("Not an address book"))
        .await
        .expect("MKCALENDAR");
    let books = client.list_addressbooks(&home).await.expect("list");
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].href, book);
    assert_eq!(books[0].display_name.as_deref(), Some("Friends"));
    assert_eq!(books[0].description.as_deref(), Some("People & pets"));
    assert!(books[0].sync_token.is_some());
    assert!(books[0].supports_sync);
    assert!(!books[0].read_only);
    assert_eq!(client.list_calendars(&home).await.expect("list").len(), 1);

    client
        .update_addressbook(
            &book,
            &AddressBookPatch {
                display_name: Some("Family".into()),
                description: None,
            },
        )
        .await
        .expect("PROPPATCH");
    let renamed = client
        .get_addressbook(&book)
        .await
        .expect("get address book");
    assert_eq!(renamed.display_name.as_deref(), Some("Family"));
    assert_eq!(renamed.description.as_deref(), Some("People & pets"));

    let token0 = client
        .sync_collection(&book, None)
        .await
        .expect("initial sync")
        .sync_token
        .expect("token");

    let href1 = object_href(&book, "contact-1", ObjectKind::Contact).expect("href");
    assert!(href1.ends_with("/contact-1.vcf"));
    let etag1 = client
        .put(
            &href1,
            &vcard("contact-1", "Ayşe Yılmaz", "ayse@example.com"),
            Precondition::IfNoneMatch,
        )
        .await
        .expect("create contact 1")
        .expect("etag");
    let href2 = object_href(&book, "contact-2", ObjectKind::Contact).expect("href");
    let etag2 = client
        .put(
            &href2,
            &vcard("contact-2", "Jane Doe", "jane@example.com"),
            Precondition::IfNoneMatch,
        )
        .await
        .expect("create contact 2")
        .expect("etag");

    let delta = client
        .sync_collection(&book, Some(&token0))
        .await
        .expect("delta");
    let mut changed: Vec<String> = delta.changed.iter().map(|o| o.href.clone()).collect();
    changed.sort();
    assert_eq!(changed, [href1.clone(), href2.clone()]);
    let token1 = delta.sync_token.expect("token");

    let fetched = client
        .addressbook_multiget(&book, &[href1.clone(), href2.clone()])
        .await
        .expect("addressbook-multiget");
    assert_eq!(fetched.objects.len(), 2);
    let ayse = fetched
        .objects
        .iter()
        .find(|o| o.href == href1)
        .expect("contact 1");
    assert_eq!(ayse.etag.as_deref(), Some(etag1.as_str()));
    assert!(ayse.data.contains("FN:Ayşe Yılmaz"));
    assert!(ayse.data.starts_with("BEGIN:VCARD"));

    let etag1b = client
        .put(
            &href1,
            &vcard("contact-1", "Ayşe Yılmaz-Demir", "ayse@example.com"),
            Precondition::IfMatch(etag1.clone()),
        )
        .await
        .expect("update contact 1")
        .expect("etag");
    assert_ne!(etag1b, etag1);
    let err = client
        .put(
            &href1,
            &vcard("contact-1", "Stale", "x@example.com"),
            Precondition::IfMatch(etag1),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DavError::PreconditionFailed { .. }),
        "{err:?}"
    );
    client
        .delete(&href2, Some(&etag2))
        .await
        .expect("delete contact 2");

    let delta = client
        .sync_collection(&book, Some(&token1))
        .await
        .expect("delta");
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.changed[0].href, href1);
    assert_eq!(delta.changed[0].etag.as_deref(), Some(etag1b.as_str()));
    assert_eq!(delta.removed, [href2]);

    client
        .delete_collection(&book)
        .await
        .expect("delete address book");
    assert!(
        client
            .list_addressbooks(&home)
            .await
            .expect("list")
            .is_empty()
    );
}

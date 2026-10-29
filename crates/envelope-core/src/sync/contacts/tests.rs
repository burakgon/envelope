//! Contacts: the engine API on a local address book, CardDAV storage quirks,
//! two-way sync with mock Google People and Microsoft Graph servers, and
//! contact groups going up to a mock CardDAV server and coming from a mock
//! Exchange server, all with a real (temporary) database.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use parking_lot::Mutex;
use serde_json::{Value, json};
use wiremock::matchers::{
    body_partial_json, header, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::contacts::{ContactInput, LabeledValue};
use crate::engine::{EventSink, Paths};
use crate::model::{AccountConfig, AccountKind};

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

impl Events {
    fn contacts_changed(&self) -> Vec<i64> {
        self.0
            .lock()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::ContactsChanged { book_ids } => Some(book_ids.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }
}

struct Fixture {
    engine: Engine,
    events: Events,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "envelope-contacts-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .expect("engine");
    Fixture {
        engine,
        events,
        dir,
    }
}

impl Fixture {
    /// An account the background schedulers leave alone (IMAP without DAV
    /// servers); the tests drive its contact sync themselves.
    fn account(&self, email: &str) -> AccountRow {
        let id = self
            .engine
            .db
            .write(|tx| {
                mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    email,
                    "Test",
                    "#0ea5e9",
                    &AccountConfig::default(),
                )
            })
            .expect("account");
        self.engine.account_row(id).expect("account row")
    }

    fn book(&self, account: &AccountRow, kind: &str, remote_id: &str) -> BookRow {
        let upsert = BookUpsert {
            remote_id: remote_id.to_owned(),
            name: "Book".into(),
            read_only: false,
            is_default: true,
            sort_order: 0,
        };
        self.engine
            .db
            .write(|tx| cdb::sync_books(tx, account.id, kind, std::slice::from_ref(&upsert)))
            .expect("book");
        self.books(account).remove(0)
    }

    fn books(&self, account: &AccountRow) -> Vec<BookRow> {
        self.engine
            .db
            .read(|c| cdb::account_books(c, account.id))
            .expect("books")
    }

    fn row(&self, id: i64) -> ContactRow {
        self.engine
            .db
            .read(|c| cdb::get_contact(c, id))
            .expect("row")
    }

    async fn list(&self, query: &str) -> Vec<(String, Option<String>)> {
        self.engine
            .contacts_list(query, None, 0, 100)
            .await
            .expect("list")
            .items
            .into_iter()
            .map(|c| (c.name, c.email))
            .collect()
    }

    async fn id_of(&self, name: &str) -> i64 {
        self.engine
            .contacts_list(name, None, 0, 10)
            .await
            .expect("list")
            .items
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no contact {name}"))
            .id
    }

    async fn photos(&self) {
        let http = reqwest::Client::new();
        fetch_photos(&self.engine, &http, |_| None).await;
        make_thumbnails(&self.engine).await;
    }
}

fn lv(value: &str, label: &str) -> LabeledValue {
    LabeledValue {
        value: value.into(),
        label: Some(label.into()),
    }
}

/// A small PNG photo as bytes and as a data URL.
fn png() -> (Vec<u8>, String) {
    let image = image::RgbImage::from_fn(40, 30, |x, y| image::Rgb([x as u8 * 6, y as u8 * 8, 90]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("png");
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    (bytes, url)
}

// ---------------------------------------------------------------------------
// Local address book

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_address_book_round_trip() {
    let fx = fixture();
    let books = fx.engine.contact_books().expect("books");
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].kind, "local");
    assert_eq!(books[0].name, crate::contacts::LOCAL_BOOK_NAME);
    assert!(!books[0].read_only);
    let book = books[0].id;
    let (_, photo) = png();

    let id = fx
        .engine
        .save_contact(ContactInput {
            book_id: book,
            name: "Ada Lovelace".into(),
            given_name: Some("Ada".into()),
            family_name: Some("Lovelace".into()),
            emails: vec![
                lv("ada@work.example", "work"),
                lv("ada@home.example", "home"),
            ],
            phones: vec![lv("+44 (20) 7946-0958", "mobile")],
            addresses: vec![lv("12 St James's Square\nLondon SW1Y 4JH", "home")],
            urls: vec!["ada.example".into()],
            organization: Some("Analytical Engines".into()),
            birthday: Some("--12-10".into()),
            notes: Some("First programmer".into()),
            photo: Some(photo.clone()),
            ..ContactInput::default()
        })
        .await
        .expect("create");
    for (name, org) in [
        ("Zeynep Öztürk", "Acme"),
        ("ömer Şahin", ""),
        ("Oscar", ""),
        ("+1 555 0100", ""),
    ] {
        fx.engine
            .save_contact(ContactInput {
                book_id: book,
                name: name.into(),
                organization: Some(org.into()),
                ..ContactInput::default()
            })
            .await
            .expect("create");
    }

    let view = fx.engine.contact(id).await.expect("contact");
    assert_eq!(view.name, "Ada Lovelace");
    assert_eq!(view.phones[0].label.as_deref(), Some("mobile"));
    assert_eq!(
        view.addresses[0].value,
        "12 St James's Square\nLondon SW1Y 4JH"
    );
    assert_eq!(view.urls, ["https://ada.example"]);
    assert_eq!(view.birthday.as_deref(), Some("--12-10"));
    assert_eq!(view.photo.as_deref(), Some(photo.as_str()));

    let names: Vec<String> = fx.list("").await.into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        [
            "Ada Lovelace",
            "Oscar",
            "ömer Şahin",
            "Zeynep Öztürk",
            "+1 555 0100"
        ],
        "sections in order, accents next to their base letter, # last"
    );
    let page = fx.engine.contacts_list("", None, 0, 2).await.expect("page");
    assert_eq!(page.total, 5);
    assert_eq!(page.items.len(), 2);
    assert!(
        page.items[0]
            .photo
            .as_deref()
            .is_some_and(|p| p.starts_with("data:image/jpeg;base64,")),
        "list thumbnail"
    );
    assert_eq!(page.items[0].email.as_deref(), Some("ada@work.example"));
    for (query, expected) in [
        ("LOVE", "Ada Lovelace"),
        ("ozturk", "Zeynep Öztürk"),
        ("acme", "Zeynep Öztürk"),
        ("ada@home", "Ada Lovelace"),
        ("7946 0958", "Ada Lovelace"),
        ("+44 20 79", "Ada Lovelace"),
        ("ada engines", "Ada Lovelace"),
    ] {
        assert_eq!(
            fx.list(query).await.first().map(|(n, _)| n.as_str()),
            Some(expected),
            "{query}"
        );
    }
    assert!(fx.list("nobody").await.is_empty());

    // Saving what the editor was given changes nothing.
    let version = fx.row(id).version;
    let unchanged = ContactInput {
        id: Some(id),
        book_id: book,
        name: "Ada Lovelace".into(),
        given_name: view.given_name.clone(),
        family_name: view.family_name.clone(),
        emails: view.emails.clone(),
        phones: view.phones.clone(),
        addresses: view.addresses.clone(),
        urls: view.urls.clone(),
        organization: view.organization.clone(),
        title: view.title.clone(),
        birthday: view.birthday.clone(),
        notes: view.notes.clone(),
        photo: view.photo.clone(),
    };
    fx.engine
        .save_contact(unchanged.clone())
        .await
        .expect("save");
    assert_eq!(fx.row(id).version, version);
    let mut edited = unchanged;
    edited.photo = None;
    edited.phones.clear();
    fx.engine.save_contact(edited).await.expect("save");
    let view = fx.engine.contact(id).await.expect("contact");
    assert!(view.phones.is_empty());
    assert_eq!(view.photo, None);
    assert!(fx.list("7946").await.is_empty());
    assert_eq!(
        fx.row(id).local_state,
        cdb::SYNCED,
        "local books need no upload"
    );

    fx.engine.delete_contact(id).await.expect("delete");
    assert!(fx.engine.contact(id).await.is_err());
    assert_eq!(fx.list("").await.len(), 4);
    assert!(
        fx.engine
            .save_contact(ContactInput {
                book_id: book,
                ..ContactInput::default()
            })
            .await
            .is_err(),
        "a contact needs a name or an address"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_book_makes_way_for_account_books() {
    let fx = fixture();
    assert_eq!(fx.engine.contact_books().expect("books").len(), 1);
    let account = fx.account("megan@contoso.com");
    let remote = fx.book(&account, "carddav", "https://dav.example.com/ab/");
    let books = fx.engine.contact_books().expect("books");
    assert_eq!(books.len(), 1, "the empty local book goes away");
    assert_eq!(books[0].id, remote.id);
    assert_eq!(books[0].account_id, Some(account.id));

    // A read-only remote book does not count, and refuses edits.
    fx.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE address_books SET read_only = 1 WHERE id = ?",
                [remote.id],
            )?;
            Ok(())
        })
        .expect("read-only");
    let books = fx.engine.contact_books().expect("books");
    assert_eq!(books.len(), 2);
    assert!(
        fx.engine
            .save_contact(ContactInput {
                book_id: remote.id,
                name: "X".into(),
                ..ContactInput::default()
            })
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autocomplete_ranks_address_book_contacts() {
    let fx = fixture();
    let book = fx.engine.contact_books().expect("books")[0].id;
    fx.engine
        .save_contact(ContactInput {
            book_id: book,
            name: "Ada Lovelace".into(),
            emails: vec![
                lv("countess@work.example", "work"),
                lv("ada@home.example", "home"),
            ],
            ..ContactInput::default()
        })
        .await
        .expect("contact");
    fx.engine
        .db
        .write(|tx| {
            crate::db::misc::record_addresses(
                tx,
                &[
                    crate::model::Address::new(Some("Adam".into()), "adam@x.example"),
                    crate::model::Address::new(None, "ada@home.example"),
                ],
                false,
                now_ms(),
            )
        })
        .expect("history");
    let emails = |q: &str| {
        fx.engine
            .suggest_contacts(q, 8)
            .expect("suggest")
            .into_iter()
            .map(|s| (s.email, s.name))
            .collect::<Vec<_>>()
    };
    let found = emails("ada");
    assert_eq!(found.len(), 3);
    assert!(
        found[..2]
            .iter()
            .all(|(_, name)| name.as_deref() == Some("Ada Lovelace")),
        "{found:?}"
    );
    assert_eq!(found[2].0, "adam@x.example");
    // Any word of the name finds every address of the contact.
    assert_eq!(emails("lovel").len(), 2);
    // An address match only suggests that address.
    assert_eq!(
        emails("countess"),
        [(
            "countess@work.example".to_owned(),
            Some("Ada Lovelace".to_owned())
        )]
    );
}

// ---------------------------------------------------------------------------
// CardDAV storage

fn object(href: &str, etag: &str, data: &str) -> DavObject {
    DavObject {
        href: href.into(),
        etag: Some(etag.into()),
        data: data.into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stores_vcards_from_quirky_servers() {
    let fx = fixture();
    let account = fx.account("alex@example.com");
    let book = fx.book(&account, "carddav", "https://dav.example.com/ab/");
    let (_, photo) = png();
    // vCard 4.0 with a folded data: URI photo.
    let mut v4 = String::from(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:1\r\nFN:Can Öztürk\r\nEMAIL;TYPE=work;PREF=1:can@work.example\r\n",
    );
    let photo_line = format!("PHOTO:{photo}");
    let mut rest = photo_line.as_str();
    let mut first = true;
    while !rest.is_empty() {
        let take = rest.len().min(if first { 75 } else { 74 });
        if !first {
            v4.push(' ');
        }
        v4.push_str(&rest[..take]);
        v4.push_str("\r\n");
        rest = &rest[take..];
        first = false;
    }
    v4.push_str("END:VCARD\r\n");
    let objects = vec![
        object("https://dav.example.com/ab/1.vcf", "\"1\"", &v4),
        // No UID, no FN: the name comes from N.
        object(
            "https://dav.example.com/ab/2.vcf",
            "\"2\"",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Kaya;Ahmet;;;\r\nTEL;TYPE=CELL:+90 532 123 45 67\r\nADR;TYPE=WORK:;;Atatürk Cd. 1;Ankara;;06000;Türkiye\r\nEND:VCARD\r\n",
        ),
        object(
            "https://dav.example.com/ab/group.vcf",
            "\"g\"",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nFN:Family\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nEND:VCARD\r\n",
        ),
        object(
            "https://dav.example.com/ab/bad.vcf",
            "\"b\"",
            "this is not a vCard",
        ),
        // The same href twice in one answer: the later copy wins.
        object(
            "https://dav.example.com/ab/2.vcf",
            "\"2b\"",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Kaya;Ahmet;;;\r\nTEL;TYPE=CELL:+90 532 123 45 67\r\nTEL;TYPE=WORK:+90 312 000 00 00\r\nEND:VCARD\r\n",
        ),
    ];
    store_vcards(&fx.engine, book.id, objects, Vec::new())
        .await
        .expect("store");
    let names = fx.list("").await;
    assert_eq!(
        names,
        [
            ("Ahmet Kaya".to_owned(), None),
            ("Can Öztürk".to_owned(), Some("can@work.example".to_owned()))
        ],
        "group cards and unreadable vCards are not contacts"
    );
    let index = fx
        .engine
        .db
        .read(|c| cdb::remote_index(c, book.id))
        .expect("index");
    assert_eq!(index.len(), 3, "the group card is remembered");
    assert_eq!(
        index["https://dav.example.com/ab/2.vcf"].0.as_deref(),
        Some("\"2b\"")
    );
    let ahmet = fx.id_of("Ahmet Kaya").await;
    assert_eq!(
        fx.engine.contact(ahmet).await.expect("view").phones.len(),
        2
    );

    fx.photos().await;
    let can = fx.id_of("Can Öztürk").await;
    let view = fx.engine.contact(can).await.expect("view");
    assert_eq!(view.photo.as_deref(), Some(photo.as_str()));
    let summary = fx
        .engine
        .contacts_list("can", None, 0, 1)
        .await
        .expect("list")
        .items
        .remove(0);
    assert!(
        summary
            .photo
            .is_some_and(|p| p.starts_with("data:image/jpeg"))
    );

    // A local edit is not overwritten by the next server copy, and survives
    // the server deleting the contact (pushing it puts it back).
    let input = ContactInput {
        id: Some(can),
        book_id: book.id,
        name: view.name.clone(),
        emails: view.emails.clone(),
        notes: Some("local note".into()),
        photo: view.photo.clone(),
        ..ContactInput::default()
    };
    fx.engine.save_contact(input.clone()).await.expect("edit");
    assert_eq!(fx.row(can).local_state, cdb::MODIFIED);
    store_vcards(
        &fx.engine,
        book.id,
        vec![object(
            "https://dav.example.com/ab/1.vcf",
            "\"9\"",
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:1\r\nFN:Server Name\r\nEND:VCARD\r\n",
        )],
        vec!["https://dav.example.com/ab/1.vcf".into()],
    )
    .await
    .expect("store");
    let row = fx.row(can);
    assert_eq!(row.contact.note.as_deref(), Some("local note"));
    assert_eq!(
        row.etag.as_deref(),
        Some("\"1\""),
        "still based on the copy it was edited from"
    );
    // Deleting it locally marks it for deletion on the server.
    fx.engine.delete_contact(can).await.expect("delete");
    assert_eq!(fx.row(can).local_state, cdb::DELETED);
    assert_eq!(fx.list("").await.len(), 1, "hidden right away");
}

// ---------------------------------------------------------------------------
// Google People

fn google(server: &MockServer) -> envelope_google::GoogleClient {
    envelope_google::GoogleClient::with_config(
        Arc::new(envelope_google::StaticToken::new("token")),
        envelope_google::ClientConfig {
            retry: envelope_google::RetryPolicy::none(),
            ..envelope_google::ClientConfig::with_api_root(&server.uri())
        },
    )
    .expect("client")
}

fn ada(server: &MockServer, etag: &str) -> Value {
    json!({
        "resourceName": "people/c1",
        "etag": etag,
        "names": [{ "metadata": { "primary": true }, "displayName": "Ada Lovelace", "givenName": "Ada", "familyName": "Lovelace" }],
        "emailAddresses": [
            { "metadata": { "primary": true }, "value": "ada@work.example", "type": "work", "formattedType": "Work" },
            { "value": "ada@home.example", "type": "home" }
        ],
        "phoneNumbers": [{ "value": "+44 20 1234", "type": "mobile" }],
        "photos": [{ "url": format!("{}/photos/ada=s100", server.uri()) }]
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_contacts_two_way_sync() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = fx.account("ada@gmail.example");
    let client = google(&server);
    let (photo_bytes, photo_url) = png();
    let connections = "/v1/people/me/connections";

    Mock::given(method("GET"))
        .and(path(connections))
        .and(query_param_is_missing("syncToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [
                ada(&server, "%A1"),
                {
                    "resourceName": "people/c2",
                    "etag": "%G1",
                    "names": [{ "displayName": "Grace Hopper" }],
                    "emailAddresses": [{ "value": "grace@navy.example" }]
                }
            ],
            "nextSyncToken": "t1"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/photos/ada=s100"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(photo_bytes.clone(), "image/png"))
        .mount(&server)
        .await;

    let changed = google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("first sync");
    let book = fx.books(&account).remove(0);
    assert!(changed.contains(&book.id));
    assert_eq!(book.kind, "google");
    assert_eq!(book.name, "ada@gmail.example");
    assert!(!book.read_only);
    assert_eq!(book.sync_state.sync_token.as_deref(), Some("t1"));
    assert_eq!(
        fx.list("").await,
        [
            ("Ada Lovelace".into(), Some("ada@work.example".into())),
            ("Grace Hopper".into(), Some("grace@navy.example".into()))
        ]
    );
    fx.photos().await;
    let ada_id = fx.id_of("Ada Lovelace").await;
    let view = fx.engine.contact(ada_id).await.expect("view");
    assert_eq!(view.photo.as_deref(), Some(photo_url.as_str()));
    assert_eq!(view.phones[0].label.as_deref(), Some("mobile"));

    // Local changes: a new phone for Ada, Grace deleted, a new contact.
    let mut input = ContactInput {
        id: Some(ada_id),
        book_id: book.id,
        name: view.name.clone(),
        given_name: view.given_name.clone(),
        family_name: view.family_name.clone(),
        emails: view.emails.clone(),
        phones: view.phones.clone(),
        photo: view.photo.clone(),
        ..ContactInput::default()
    };
    input.phones.push(lv("+44 20 9999", "work"));
    fx.engine.save_contact(input.clone()).await.expect("edit");
    let grace = fx.id_of("Grace Hopper").await;
    fx.engine.delete_contact(grace).await.expect("delete");
    let charles = fx
        .engine
        .save_contact(ContactInput {
            book_id: book.id,
            name: "Charles Babbage".into(),
            given_name: Some("Charles".into()),
            family_name: Some("Babbage".into()),
            emails: vec![lv("charles@engine.example", "work")],
            photo: Some(photo_url.clone()),
            ..ContactInput::default()
        })
        .await
        .expect("create");
    assert_eq!(fx.row(charles).local_state, cdb::CREATED);

    let mut ada_updated = ada(&server, "%A2");
    ada_updated["phoneNumbers"] = json!([
        { "value": "+44 20 1234", "type": "mobile" },
        { "value": "+44 20 9999", "type": "work" }
    ]);
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContact"))
        .and(query_param("updatePersonFields", "phoneNumbers"))
        .and(body_partial_json(json!({ "etag": "%A1" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ada_updated.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/people/c2:deleteContact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/people:createContact"))
        .and(body_partial_json(json!({
            "names": [{ "givenName": "Charles", "familyName": "Babbage" }],
            "emailAddresses": [{ "value": "charles@engine.example", "type": "work" }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resourceName": "people/c3",
            "etag": "%C1",
            "names": [{ "displayName": "Charles Babbage", "givenName": "Charles", "familyName": "Babbage" }],
            "emailAddresses": [{ "value": "charles@engine.example", "type": "work" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c3:updateContactPhoto"))
        .and(body_partial_json(json!({
            "photoBytes": base64::engine::general_purpose::STANDARD.encode(&photo_bytes)
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "person": {
            "resourceName": "people/c3",
            "etag": "%C2",
            "names": [{ "displayName": "Charles Babbage", "givenName": "Charles", "familyName": "Babbage" }],
            "emailAddresses": [{ "value": "charles@engine.example", "type": "work" }],
            "photos": [{ "url": format!("{}/photos/charles=s100", server.uri()) }]
        }})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(connections))
        .and(query_param("syncToken", "t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "nextSyncToken": "t2" })))
        .mount(&server)
        .await;

    google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("second sync");
    let row = fx.row(ada_id);
    assert_eq!(row.local_state, cdb::SYNCED);
    assert_eq!(row.etag.as_deref(), Some("%A2"));
    assert_eq!(row.contact.phones.len(), 2);
    assert!(fx.engine.contact(grace).await.is_err(), "deleted for good");
    let row = fx.row(charles);
    assert_eq!(row.local_state, cdb::SYNCED);
    assert_eq!(row.remote_id.as_deref(), Some("people/c3"));
    assert_eq!(row.etag.as_deref(), Some("%C2"));
    assert!(!row.photo_dirty);
    assert_eq!(
        row.photo_url, row.photo_src,
        "no need to download our own photo"
    );
    assert_eq!(
        fx.engine
            .db
            .read(|c| cdb::photos_to_fetch(c, 10))
            .expect("pending")
            .len(),
        0
    );

    // A conflict: Ada changed on Google while a local note was pending.
    input.notes = Some("Wrote the first program".into());
    fx.engine.save_contact(input).await.expect("edit");
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContact"))
        .and(body_partial_json(json!({ "etag": "%A2" })))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "code": 400, "status": "FAILED_PRECONDITION",
                       "message": "Request person.etag is different than the current person.etag." }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut ada_fresh = ada_updated.clone();
    ada_fresh["etag"] = json!("%A3");
    ada_fresh["organizations"] = json!([{ "name": "Analytical Engines", "title": "Countess" }]);
    Mock::given(method("GET"))
        .and(path("/v1/people/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ada_fresh.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut ada_merged = ada_fresh.clone();
    ada_merged["etag"] = json!("%A4");
    ada_merged["biographies"] =
        json!([{ "value": "Wrote the first program", "contentType": "TEXT_PLAIN" }]);
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContact"))
        .and(query_param("updatePersonFields", "biographies"))
        .and(body_partial_json(json!({ "etag": "%A3" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ada_merged.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(connections))
        .and(query_param("syncToken", "t2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "nextSyncToken": "t3" })))
        .mount(&server)
        .await;
    google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("conflict sync");
    let view = fx.engine.contact(ada_id).await.expect("view");
    assert_eq!(view.notes.as_deref(), Some("Wrote the first program"));
    assert_eq!(
        view.title.as_deref(),
        Some("Countess"),
        "the server's change is kept"
    );
    assert_eq!(fx.row(ada_id).etag.as_deref(), Some("%A4"));

    // An expired sync token: a full listing, and contacts it lacks go.
    Mock::given(method("GET"))
        .and(path(connections))
        .and(query_param("syncToken", "t3"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "code": 400, "status": "FAILED_PRECONDITION", "message": "Sync token is expired.",
                       "details": [{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "EXPIRED_SYNC_TOKEN" }] }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(connections))
        .and(query_param_is_missing("syncToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [ada_merged],
            "nextSyncToken": "t4"
        })))
        .mount(&server)
        .await;
    google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("resync");
    assert_eq!(
        fx.list("").await,
        [("Ada Lovelace".into(), Some("ada@work.example".into()))]
    );
    assert_eq!(
        fx.books(&account)[0].sync_state.sync_token.as_deref(),
        Some("t4")
    );
    assert!(!fx.events.contacts_changed().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_read_only_grants_keep_edits_local_until_refused() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = fx.account("ro@gmail.example");
    let client = google(&server);
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [ada(&server, "%A1")], "nextSyncToken": "t1"
        })))
        .mount(&server)
        .await;
    google_sync(&fx.engine, &account, &client, false, true)
        .await
        .expect("sync");
    let book = fx.books(&account).remove(0);
    assert!(book.read_only);
    let ada_id = fx.id_of("Ada Lovelace").await;
    assert!(fx.engine.delete_contact(ada_id).await.is_err(), "read-only");

    // A grant that turns out not to allow writing: the change is dropped and
    // the server's copy comes back.
    google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("sync");
    fx.engine.delete_contact(ada_id).await.expect("delete");
    Mock::given(method("DELETE"))
        .and(path("/v1/people/c1:deleteContact"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": { "code": 403, "status": "PERMISSION_DENIED",
                       "message": "Request had insufficient authentication scopes.",
                       "errors": [{ "reason": "insufficientPermissions" }] }
        })))
        .expect(1)
        .mount(&server)
        .await;
    google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("sync");
    assert_eq!(fx.list("").await.len(), 1, "restored from the server");
}

/// A Google account with Ada and Grace, and one label (with `members`)
/// besides Google's own groups and an empty label.
async fn google_with_label(server: &MockServer, name: &str, members: Value) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [
                ada(server, "%A1"),
                { "resourceName": "people/c2", "etag": "%G1", "names": [{ "displayName": "Grace Hopper" }],
                  "emailAddresses": [{ "value": "grace@navy.example" }] }
            ],
            "nextSyncToken": "t1"
        })))
        .mount(server)
        .await;
    let count = members.as_array().map_or(0, Vec::len);
    Mock::given(method("GET"))
        .and(path("/v1/contactGroups"))
        .and(query_param(
            "groupFields",
            "name,groupType,memberCount,metadata",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "contactGroups": [
                { "resourceName": "contactGroups/myContacts", "groupType": "SYSTEM_CONTACT_GROUP",
                  "name": "myContacts", "formattedName": "My Contacts", "memberCount": 2 },
                { "resourceName": "contactGroups/starred", "groupType": "SYSTEM_CONTACT_GROUP", "name": "starred" },
                { "resourceName": "contactGroups/1a2b", "etag": "e1", "groupType": "USER_CONTACT_GROUP",
                  "name": name, "formattedName": name, "memberCount": count },
                { "resourceName": "contactGroups/9z", "groupType": "USER_CONTACT_GROUP", "name": "Empty" }
            ]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/contactGroups:batchGet"))
        .and(query_param("resourceNames", "contactGroups/1a2b"))
        .and(query_param("maxMembers", count.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "responses": [{
                "requestedResourceName": "contactGroups/1a2b",
                "contactGroup": { "resourceName": "contactGroups/1a2b", "name": name, "memberResourceNames": members }
            }]
        })))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_labels_are_read_only_groups() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = fx.account("ada@gmail.example");
    let client = google(&server);
    google_with_label(&server, "Design team", json!(["people/c1", "people/c2"])).await;
    let changed = google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("sync");
    let book = fx.books(&account).remove(0);
    assert!(changed.contains(&book.id));
    let groups = fx.engine.contact_groups().await.expect("groups");
    assert_eq!(
        groups
            .iter()
            .map(|g| (
                g.name.as_str(),
                g.read_only,
                g.members
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
            ))
            .collect::<Vec<_>>(),
        [
            ("Design team", true, vec!["Ada Lovelace", "Grace Hopper"]),
            ("Empty", true, Vec::new()),
        ],
        "the user's labels, not Google's own groups"
    );
    let team = groups[0].id;
    assert!(fx.engine.rename_group(team, "Designers").await.is_err());
    assert_eq!(fx.list("").await.len(), 2, "labels are not contacts");

    // Renamed, and one person less, on Google. A full sync of the people
    // (the token expired) keeps the labels.
    google_with_label(&server, "Designers", json!(["people/c2"])).await;
    fx.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE address_books SET sync_state = '{}' WHERE id = ?",
                [book.id],
            )?;
            Ok(())
        })
        .expect("reset");
    google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("sync");
    let groups = fx.engine.contact_groups().await.expect("groups");
    assert_eq!(groups[0].id, team, "the same group, changed");
    assert_eq!(groups[0].name, "Designers");
    assert_eq!(
        groups[0]
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        ["Grace Hopper"]
    );

    // Labels deleted on Google go here too.
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [], "nextSyncToken": "t2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/contactGroups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "contactGroups": [] })))
        .mount(&server)
        .await;
    google_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("sync");
    assert!(fx.engine.contact_groups().await.expect("groups").is_empty());
    assert_eq!(fx.list("").await.len(), 2);
}

// ---------------------------------------------------------------------------
// Microsoft Graph

fn graph(server: &MockServer) -> envelope_graph::GraphClient {
    envelope_graph::GraphClient::builder(Arc::new(envelope_graph::StaticToken::new("token")))
        .base_url(format!("{}/v1.0", server.uri()))
        .retry_policy(envelope_graph::RetryPolicy::none())
        .build()
        .expect("client")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_contacts_two_way_sync() {
    let fx = fixture();
    let server = MockServer::start().await;
    let base = format!("{}/v1.0", server.uri());
    let account = fx.account("megan@contoso.example");
    let client = graph(&server);
    let (photo_bytes, photo_url) = png();

    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{ "id": "F2", "displayName": "Family", "parentFolderId": "F1" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders/F1/contacts/delta"))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.deltaLink": format!("{base}/me/contactFolders/F1/contacts/delta?$deltatoken=d1"),
            "value": [{
                "id": "P", "displayName": "Pavel Bansky", "givenName": "Pavel", "surname": "Bansky",
                "emailAddresses": [{ "name": "Pavel Bansky", "address": "pavelb@contoso.example" }],
                "businessPhones": ["+1 732 555 0102"],
                "homeAddress": { "street": "1 Main St", "city": "Redmond", "state": "WA", "postalCode": "98052" }
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders/F2/contacts/delta"))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.deltaLink": format!("{base}/me/contactFolders/F2/contacts/delta?$deltatoken=d1"),
            "value": [{ "id": "M", "displayName": "Mom", "homePhones": ["+1 555 0199"] }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts/P/photo/$value"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(photo_bytes.clone(), "image/png"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts/M/photo/$value"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "code": "ImageNotFound", "message": "The photo wasn't found." }
        })))
        .mount(&server)
        .await;

    graph_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("first sync");
    let books = fx.books(&account);
    assert_eq!(books.len(), 2);
    assert_eq!(books[0].remote_id, GRAPH_DEFAULT_BOOK);
    assert_eq!(books[0].name, "megan@contoso.example");
    assert_eq!(books[0].sync_state.folder_id.as_deref(), Some("F1"));
    assert_eq!(books[1].name, "Family");
    assert_eq!(
        fx.list("").await,
        [
            ("Mom".into(), None),
            ("Pavel Bansky".into(), Some("pavelb@contoso.example".into()))
        ]
    );
    let http = reqwest::Client::new();
    let injected = client.clone();
    fetch_photos(&fx.engine, &http, move |_| Some(injected.clone())).await;
    let pavel = fx.id_of("Pavel Bansky").await;
    let mom = fx.id_of("Mom").await;
    let view = fx.engine.contact(pavel).await.expect("view");
    assert_eq!(view.photo.as_deref(), Some(photo_url.as_str()));
    assert_eq!(view.addresses[0].value, "1 Main St\nRedmond, WA 98052");
    assert_eq!(fx.engine.contact(mom).await.expect("view").photo, None);
    assert!(
        fx.engine
            .db
            .read(|c| cdb::photos_to_fetch(c, 10))
            .expect("pending")
            .is_empty()
    );

    // Local changes.
    fx.engine
        .save_contact(ContactInput {
            id: Some(pavel),
            book_id: books[0].id,
            name: view.name.clone(),
            given_name: view.given_name.clone(),
            family_name: view.family_name.clone(),
            emails: view.emails.clone(),
            phones: view.phones.clone(),
            addresses: view.addresses.clone(),
            title: Some("Manager".into()),
            photo: view.photo.clone(),
            ..ContactInput::default()
        })
        .await
        .expect("edit");
    fx.engine.delete_contact(mom).await.expect("delete");
    let dad = fx
        .engine
        .save_contact(ContactInput {
            book_id: books[1].id,
            name: "Dad".into(),
            phones: vec![lv("+1 555 0198", "mobile")],
            photo: Some(photo_url.clone()),
            ..ContactInput::default()
        })
        .await
        .expect("create");

    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/contacts/P"))
        .and(body_partial_json(json!({ "jobTitle": "Manager", "displayName": "Pavel Bansky" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "P", "displayName": "Pavel Bansky", "givenName": "Pavel", "surname": "Bansky",
            "jobTitle": "Manager",
            "emailAddresses": [{ "name": "Pavel Bansky", "address": "pavelb@contoso.example" }],
            "businessPhones": ["+1 732 555 0102"],
            "homeAddress": { "street": "1 Main St", "city": "Redmond", "state": "WA", "postalCode": "98052" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/contacts/M"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/contactFolders/F2/contacts"))
        .and(body_partial_json(
            json!({ "displayName": "Dad", "mobilePhone": "+1 555 0198" }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "D", "displayName": "Dad", "mobilePhone": "+1 555 0198"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1.0/me/contacts/D/photo/$value"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    for folder in ["F1", "F2"] {
        Mock::given(method("GET"))
            .and(path(format!("/v1.0/me/contactFolders/{folder}/contacts/delta")))
            .and(query_param("$deltatoken", "d1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "@odata.deltaLink": format!("{base}/me/contactFolders/{folder}/contacts/delta?$deltatoken=d2"),
                "value": []
            })))
            .mount(&server)
            .await;
    }

    graph_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("second sync");
    let row = fx.row(pavel);
    assert_eq!(row.local_state, cdb::SYNCED);
    assert_eq!(row.contact.title.as_deref(), Some("Manager"));
    assert!(fx.engine.contact(mom).await.is_err());
    let row = fx.row(dad);
    assert_eq!(row.local_state, cdb::SYNCED);
    assert_eq!(row.remote_id.as_deref(), Some("D"));
    assert!(!row.photo_dirty);
    assert_eq!(row.photo_url, row.photo_src);
    let cursor = &fx.books(&account)[1].sync_state.cursor;
    assert!(
        cursor
            .as_ref()
            .and_then(|c| c["url"].as_str())
            .is_some_and(|u| u.ends_with("d2"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_resync_removes_contacts_the_server_lost() {
    let fx = fixture();
    let server = MockServer::start().await;
    let base = format!("{}/v1.0", server.uri());
    let account = fx.account("megan@contoso.example");
    let client = graph(&server);
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [] })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders"))
        .and(query_param("$select", "parentFolderId"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [] })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts"))
        .and(query_param("$select", "parentFolderId"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "value": [{ "parentFolderId": "F1" }] })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders/F1/contacts/delta"))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.deltaLink": format!("{base}/me/contactFolders/F1/contacts/delta?$deltatoken=d1"),
            "value": [{ "id": "A", "displayName": "Alpha" }, { "id": "B", "displayName": "Beta" }]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    graph_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("sync");
    assert_eq!(fx.list("").await.len(), 2);

    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders/F1/contacts/delta"))
        .and(query_param("$deltatoken", "d1"))
        .respond_with(ResponseTemplate::new(410).set_body_json(json!({
            "error": { "code": "SyncStateNotFound", "message": "The sync state is gone." }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders/F1/contacts/delta"))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.deltaLink": format!("{base}/me/contactFolders/F1/contacts/delta?$deltatoken=d9"),
            "value": [{ "id": "B", "displayName": "Beta" }]
        })))
        .mount(&server)
        .await;
    graph_sync(&fx.engine, &account, &client, true, true)
        .await
        .expect("resync");
    assert_eq!(fx.list("").await, [("Beta".into(), None)]);
}

#[test]
fn photo_urls_ask_for_a_useful_size() {
    assert_eq!(
        sized_photo_url("https://lh3.googleusercontent.com/contacts/AbC=s100"),
        "https://lh3.googleusercontent.com/contacts/AbC=s256"
    );
    assert_eq!(
        sized_photo_url("https://lh3.googleusercontent.com/-x/AAAA/s100/photo.jpg"),
        "https://lh3.googleusercontent.com/-x/AAAA/s100/photo.jpg?sz=256"
    );
    assert_eq!(
        sized_photo_url("https://cloud.example.com/photo.jpg"),
        "https://cloud.example.com/photo.jpg"
    );
}

#[test]
fn book_names_prefer_the_server_name() {
    let account = AccountRow {
        id: 1,
        kind: AccountKind::Imap,
        provider: "icloud".into(),
        email: "jane@icloud.example".into(),
        name: "Jane".into(),
        label: Some("iCloud".into()),
        color: String::new(),
        config: AccountConfig::default(),
        state: json!({}),
        sort_order: 0,
        enabled: true,
    };
    assert_eq!(book_name(Some("Family"), &account), "Family");
    assert_eq!(book_name(Some("Contacts"), &account), "iCloud");
    assert_eq!(book_name(None, &account), "iCloud");
    assert_eq!(book_name(Some(" card "), &account), "iCloud");
}

#[test]
fn maps_exchange_contacts() {
    let c = envelope_ews::Contact {
        id: Some(envelope_ews::ItemId {
            id: "AAMk1".into(),
            change_key: Some("ck".into()),
        }),
        parent_folder_id: None,
        is_group: false,
        display_name: None,
        file_as: Some("Okafor, Daniel".into()),
        given_name: Some("Daniel".into()),
        middle_name: None,
        surname: Some("Okafor".into()),
        nickname: None,
        company_name: Some("Lumen".into()),
        department: None,
        job_title: Some("Engineering Manager".into()),
        email_addresses: vec![envelope_ews::ContactEmail {
            key: "EmailAddress1".into(),
            address: "daniel@lumen.dev".into(),
            name: None,
        }],
        phone_numbers: vec![
            envelope_ews::ContactPhone {
                key: "MobilePhone".into(),
                number: "+44 7700 900 461".into(),
            },
            envelope_ews::ContactPhone {
                key: "BusinessPhone".into(),
                number: "+44 20 7946 0000".into(),
            },
        ],
        birthday: None,
        physical_addresses: vec![envelope_ews::PhysicalAddress {
            key: "Home".into(),
            street: Some("221B Baker Street".into()),
            city: Some("London".into()),
            postal_code: Some("NW1 6XE".into()),
            ..Default::default()
        }],
        business_home_page: Some("https://lumen.dev".into()),
        notes: Some("Prefers calls after ten.".into()),
        categories: vec![],
        source: Some("Store".into()),
        last_modified: None,
        members: Vec::new(),
    };
    let mapped = super::ews_contact(&c, "AAMk1");
    assert_eq!(mapped.name, "Daniel Okafor");
    assert_eq!(mapped.organization.as_deref(), Some("Lumen"));
    assert_eq!(mapped.emails[0].value, "daniel@lumen.dev");
    assert!(mapped.emails[0].preferred);
    let kinds: Vec<_> = mapped
        .phones
        .iter()
        .map(|p| p.kind.clone().unwrap_or_default())
        .collect();
    assert_eq!(kinds, vec!["cell", "work"]);
    assert_eq!(mapped.note.as_deref(), Some("Prefers calls after ten."));
    assert_eq!(mapped.urls[0].value, "https://lumen.dev");
    assert_eq!(mapped.addresses[0].kind.as_deref(), Some("home"));
    assert_eq!(
        mapped.addresses[0].formatted(),
        "221B Baker Street\nNW1 6XE London"
    );
}

#[test]
fn exchange_edits_touch_only_what_changed() {
    use envelope_ews::{ContactChange as C, ContactText as T};
    let field = |value: &str, kind: Option<&str>| envelope_ical::ContactField {
        value: value.into(),
        kind: kind.map(Into::into),
        preferred: false,
    };
    let server = envelope_ews::Contact {
        id: Some(envelope_ews::ItemId {
            id: "AAMk1".into(),
            change_key: Some("ck".into()),
        }),
        parent_folder_id: None,
        is_group: false,
        display_name: Some("Daniel Okafor".into()),
        file_as: None,
        given_name: Some("Daniel".into()),
        middle_name: Some("A.".into()),
        surname: Some("Okafor".into()),
        nickname: Some("Dan".into()),
        company_name: Some("Lumen".into()),
        department: None,
        job_title: None,
        email_addresses: vec![envelope_ews::ContactEmail {
            key: "EmailAddress1".into(),
            address: "daniel@lumen.dev".into(),
            name: Some("Daniel Okafor".into()),
        }],
        phone_numbers: vec![
            envelope_ews::ContactPhone {
                key: "CarPhone".into(),
                number: "+44 7700 900 461".into(),
            },
            envelope_ews::ContactPhone {
                key: "HomePhone".into(),
                number: "+44 20 7946 0001".into(),
            },
        ],
        birthday: None,
        physical_addresses: vec![envelope_ews::PhysicalAddress {
            key: "Home".into(),
            street: Some("221B Baker Street".into()),
            city: Some("London".into()),
            ..Default::default()
        }],
        business_home_page: None,
        notes: Some("Prefers calls after ten.".into()),
        categories: Vec::new(),
        source: None,
        last_modified: None,
        members: Vec::new(),
    };
    let mut desired = ews_contact(&server, "AAMk1");
    // Unchanged: nothing to send.
    assert_eq!(ews_changes(&server, &desired), []);

    desired.nickname = None;
    desired.title = Some("Engineering Manager".into());
    desired.emails.push(field("dan@home.example", None));
    // The car phone stays where it is (same digits, other formatting); the
    // home number goes; a new work number takes the first work key.
    desired.phones = vec![
        field("+447700900461", Some("cell")),
        field("+44 20 7946 0000", Some("work")),
    ];
    desired.birthday = chrono::NaiveDate::from_ymd_opt(1988, 4, 2);
    assert_eq!(
        ews_changes(&server, &desired),
        [
            C::Text(T::Nickname, None),
            C::Text(T::JobTitle, Some("Engineering Manager".into())),
            C::Email("EmailAddress2".into(), Some("dan@home.example".into())),
            C::Phone("HomePhone".into(), None),
            C::Phone("BusinessPhone".into(), Some("+44 20 7946 0000".into())),
            C::Birthday(chrono::NaiveDate::from_ymd_opt(1988, 4, 2)),
        ]
    );

    // A new contact: phones by kind, extra numbers of one kind spill over.
    let new = ews_new(&Contact {
        name: "Ada Lovelace".into(),
        given_name: Some("Ada".into()),
        emails: vec![field("ada@example.com", Some("work"))],
        phones: vec![
            field("1", Some("home")),
            field("2", Some("home")),
            field("3", Some("home")),
            field("4", None),
        ],
        ..Default::default()
    });
    assert_eq!(
        new.emails,
        [("EmailAddress1".to_string(), "ada@example.com".to_string())]
    );
    assert_eq!(
        new.phones
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        ["HomePhone", "HomePhone2", "OtherTelephone", "PrimaryPhone"]
    );
    assert!(new.text.contains(&(T::DisplayName, "Ada Lovelace".into())));
}

#[test]
fn exchange_edits_place_addresses_by_kind() {
    use envelope_ews::{AddressPart as P, ContactChange as C};
    let address = |kind: &str, street: &str, city: &str| envelope_ical::ContactAddress {
        kind: Some(kind.into()),
        street: street.into(),
        locality: city.into(),
        ..Default::default()
    };
    let server = envelope_ews::Contact {
        id: None,
        parent_folder_id: None,
        is_group: false,
        display_name: Some("Daniel Okafor".into()),
        file_as: None,
        given_name: None,
        middle_name: None,
        surname: None,
        nickname: None,
        company_name: None,
        department: None,
        job_title: None,
        email_addresses: Vec::new(),
        phone_numbers: Vec::new(),
        birthday: None,
        physical_addresses: vec![
            envelope_ews::PhysicalAddress {
                key: "Home".into(),
                street: Some("221B Baker Street".into()),
                city: Some("London".into()),
                ..Default::default()
            },
            envelope_ews::PhysicalAddress {
                key: "Other".into(),
                street: Some("1 Lake Road".into()),
                city: Some("Keswick".into()),
                ..Default::default()
            },
        ],
        business_home_page: Some("https://lumen.dev".into()),
        notes: Some("Prefers calls.".into()),
        categories: Vec::new(),
        source: None,
        last_modified: None,
        members: Vec::new(),
    };
    let mut desired = ews_contact(&server, "AAMk1");
    assert_eq!(ews_changes(&server, &desired), []);

    // The home address becomes a work address (it moves to Business), the
    // second one goes, and a new home address takes Home; the web site and
    // the notes change.
    desired.addresses = vec![
        address("work", "221B Baker Street", "London"),
        address("home", "4 Privet Drive", "Little Whinging"),
    ];
    desired.urls.clear();
    desired.note = Some("Prefers e-mail.".into());
    assert_eq!(
        ews_changes(&server, &desired),
        [
            C::Address(
                "Business".into(),
                P::Street,
                Some("221B Baker Street".into())
            ),
            C::Address("Business".into(), P::City, Some("London".into())),
            C::Address("Home".into(), P::Street, Some("4 Privet Drive".into())),
            C::Address("Home".into(), P::City, Some("Little Whinging".into())),
            C::Address("Other".into(), P::Street, None),
            C::Address("Other".into(), P::City, None),
            C::HomePage(None),
            C::Notes(Some("Prefers e-mail.".into())),
        ]
    );

    // Notes the server did not send are not touched.
    let unknown = envelope_ews::Contact {
        notes: None,
        ..server.clone()
    };
    let mut desired = ews_contact(&server, "AAMk1");
    desired.note = None;
    assert_eq!(ews_changes(&unknown, &desired), []);

    // A new contact: two home addresses take Home and then Other, a fourth
    // address finds no key left.
    let new = ews_new(&Contact {
        name: "Ada Lovelace".into(),
        addresses: vec![
            address("home", "12 St James's Square", "London"),
            address("home", "Ockham Park", "Surrey"),
            address("work", "1 Engine Lane", "London"),
            address("other", "Somewhere", "Else"),
        ],
        urls: vec![envelope_ical::ContactField {
            value: "https://ada.example".into(),
            kind: None,
            preferred: false,
        }],
        note: Some("Prefers letters.".into()),
        ..Default::default()
    });
    assert_eq!(
        new.addresses
            .iter()
            .map(|a| a.key.as_str())
            .collect::<Vec<_>>(),
        ["Home", "Other", "Business"]
    );
    assert_eq!(new.home_page.as_deref(), Some("https://ada.example"));
    assert_eq!(new.notes.as_deref(), Some("Prefers letters."));
}

/// An Exchange server with a contacts folder: FindItem lists `(id, change
/// key, name, notes)` without the notes, GetItem returns them in full.
#[derive(Clone, Default)]
struct ExchangeContacts {
    contacts: Arc<Mutex<Vec<(String, String, String, String)>>>,
    /// The ids each GetItem asked for.
    reads: Arc<Mutex<Vec<Vec<String>>>>,
}

impl wiremock::Respond for ExchangeContacts {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        const NS: &str = concat!(
            r#"xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" "#,
            r#"xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types""#
        );
        let body = String::from_utf8_lossy(&request.body).into_owned();
        let contacts = self.contacts.lock().clone();
        let (op, messages) = if body.contains("<m:FindItem") {
            let items: String = contacts
                .iter()
                .map(|(id, key, name, _)| {
                    format!(r#"<t:Contact><t:ItemId Id="{id}" ChangeKey="{key}"/><t:DisplayName>{name}</t:DisplayName></t:Contact>"#)
                })
                .collect();
            let found = format!(
                r#"<m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder IndexedPagingOffset="{n}" TotalItemsInView="{n}" IncludesLastItemInRange="true"><t:Items>{items}</t:Items></m:RootFolder></m:FindItemResponseMessage>"#,
                n = contacts.len()
            );
            ("FindItem", found)
        } else {
            let ids: Vec<String> = body
                .split(r#"<t:ItemId Id=""#)
                .skip(1)
                .filter_map(|rest| rest.split('"').next().map(str::to_owned))
                .collect();
            let messages = ids
                .iter()
                .map(|wanted| match contacts.iter().find(|(id, ..)| id == wanted) {
                    Some((id, key, name, notes)) => format!(
                        r#"<m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:Contact><t:ItemId Id="{id}" ChangeKey="{key}"/><t:Body BodyType="Text">{notes}</t:Body><t:DisplayName>{name}</t:DisplayName><t:PhysicalAddresses><t:Entry Key="Business"><t:Street>1 Market St</t:Street><t:City>San Francisco</t:City><t:State>CA</t:State><t:PostalCode>94105</t:PostalCode></t:Entry></t:PhysicalAddresses><t:BusinessHomePage>https://example.com/{id}</t:BusinessHomePage></t:Contact></m:Items></m:GetItemResponseMessage>"#
                    ),
                    None => r#"<m:GetItemResponseMessage ResponseClass="Error"><m:MessageText>gone</m:MessageText><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey></m:GetItemResponseMessage>"#.to_owned(),
                })
                .collect();
            self.reads.lock().push(ids);
            ("GetItem", messages)
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/xml; charset=utf-8")
            .set_body_string(format!(
                r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Header><h:ServerVersionInfo MajorVersion="15" MinorVersion="1" MajorBuildNumber="2507" MinorBuildNumber="6" Version="V2017_07_11" xmlns:h="http://schemas.microsoft.com/exchange/services/2006/types"/></s:Header><s:Body><m:{op}Response {NS}><m:ResponseMessages>{messages}</m:ResponseMessages></m:{op}Response></s:Body></s:Envelope>"#
            ))
    }
}

#[tokio::test]
async fn exchange_contacts_bring_notes_addresses_and_web_sites() {
    use crate::model::{AuthMethod, ExchangeAuth, ExchangeConfig};
    let fx = fixture();
    let server = MockServer::start().await;
    let fake = ExchangeContacts::default();
    *fake.contacts.lock() = vec![
        (
            "c1".into(),
            "k1".into(),
            "Ada Lovelace".into(),
            "Met at the Royal Society.".into(),
        ),
        (
            "c2".into(),
            "k1".into(),
            "Charles Babbage".into(),
            String::new(),
        ),
    ];
    Mock::given(method("POST"))
        .and(path(envelope_ews::EWS_PATH))
        .respond_with(fake.clone())
        .mount(&server)
        .await;
    let config = AccountConfig {
        auth: AuthMethod::Password,
        exchange: Some(ExchangeConfig {
            ews_url: format!("{}{}", server.uri(), envelope_ews::EWS_PATH),
            auth: ExchangeAuth::Basic,
            ..ExchangeConfig::default()
        }),
        ..AccountConfig::default()
    };
    let id = fx
        .engine
        .db
        .write(|tx| {
            let id = mail::insert_account(
                tx,
                AccountKind::Exchange,
                "exchange",
                "jane@example.com",
                "Jane Doe",
                "#0ea5e9",
                &config,
            )?;
            // The tests drive the sync; the account's worker stays away.
            tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [id])?;
            Ok(id)
        })
        .expect("account");
    fx.engine
        .secrets
        .preload(&crate::secrets::password_key(id), "secret");
    let account = fx.engine.account_row(id).expect("account row");
    let stored = |name: &str| {
        let book = fx.books(&account).remove(0);
        let rows = fx
            .engine
            .db
            .read(|c| {
                let mut st = c.prepare("SELECT id FROM contacts WHERE book_id = ? AND name = ?")?;
                let ids = st
                    .query_map(rusqlite::params![book.id, name], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(ids)
            })
            .expect("contacts");
        fx.row(rows[0]).contact
    };

    // The first sync reads every contact in full.
    ews_sync(&fx.engine, &account, true).await.expect("sync");
    assert_eq!(
        fake.reads.lock().clone(),
        [vec!["c1".to_string(), "c2".to_string()]]
    );
    let ada = stored("Ada Lovelace");
    assert_eq!(ada.note.as_deref(), Some("Met at the Royal Society."));
    assert_eq!(ada.urls[0].value, "https://example.com/c1");
    assert_eq!(ada.addresses.len(), 1);
    assert_eq!(ada.addresses[0].kind.as_deref(), Some("work"));
    assert_eq!(
        ada.addresses[0].formatted(),
        "1 Market St\nSan Francisco, CA 94105"
    );
    assert_eq!(stored("Charles Babbage").note, None);

    // Unchanged contacts are not read again; a changed one is.
    ews_sync(&fx.engine, &account, true).await.expect("sync");
    assert_eq!(fake.reads.lock().len(), 1);
    fake.contacts.lock()[1] = (
        "c2".into(),
        "k2".into(),
        "Charles Babbage".into(),
        "Difference engine.".into(),
    );
    ews_sync(&fx.engine, &account, true).await.expect("sync");
    assert_eq!(
        fake.reads.lock().last().cloned(),
        Some(vec!["c2".to_string()])
    );
    assert_eq!(
        stored("Charles Babbage").note.as_deref(),
        Some("Difference engine.")
    );
    assert_eq!(
        stored("Ada Lovelace").note.as_deref(),
        Some("Met at the Royal Society.")
    );
}

#[tokio::test]
async fn csv_exports_are_imported_and_address_books_exported_as_vcards() {
    let fx = fixture();
    let book = fx.engine.contact_books().expect("books")[0].id;
    let (_, photo) = png();
    fx.engine
        .save_contact(ContactInput {
            book_id: book,
            name: "Ada Lovelace".into(),
            emails: vec![lv("ada@example.com", "work")],
            photo: Some(photo),
            ..ContactInput::default()
        })
        .await
        .expect("save");

    // A Gmail export: Ada is there already, Charles is new.
    let csv = fx.dir.join("contacts.csv");
    std::fs::create_dir_all(&fx.dir).expect("dir");
    std::fs::write(
        &csv,
        "First Name,Last Name,E-mail 1 - Label,E-mail 1 - Value,Phone 1 - Label,Phone 1 - Value\n\
         Ada,Lovelace,* Work,ada@example.com,,\n\
         Charles,Babbage,Home,charles@example.com,Mobile,+44 20 7946 0001\n",
    )
    .expect("csv");
    let opened = fx.engine.open_file(&csv).await.expect("open");
    let crate::files::OpenedFile::Contacts(file) = opened else {
        panic!("not contacts: {opened:?}");
    };
    let listed: Vec<(&str, bool)> = file
        .contacts
        .iter()
        .map(|c| (c.name.as_str(), c.exists))
        .collect();
    assert_eq!(listed, [("Ada Lovelace", true), ("Charles Babbage", false)]);
    let result = fx
        .engine
        .import_contacts_file(&csv, book, None)
        .await
        .expect("import");
    assert_eq!((result.added, result.skipped), (1, 1));
    let charles = fx.id_of("Charles Babbage").await;
    let row = fx.row(charles);
    assert_eq!(row.contact.phones[0].value, "+44 20 7946 0001");
    assert_eq!(row.contact.phones[0].kind.as_deref(), Some("cell"));

    // Exported: both, Ada with her photo.
    let out = fx.dir.join("export.vcf");
    let written = fx
        .engine
        .export_contacts(Some(book), &out)
        .await
        .expect("export");
    assert_eq!(written, 2);
    let cards = envelope_ical::vcard::parse_vcards(&std::fs::read_to_string(&out).expect("vcf"));
    let names: Vec<&str> = cards.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Ada Lovelace", "Charles Babbage"]);
    assert!(
        cards[0]
            .photo
            .as_deref()
            .is_some_and(|p| p.starts_with("data:image/"))
    );
    assert_eq!(cards[1].emails[0].value, "charles@example.com");
}

#[tokio::test]
async fn duplicates_are_found_and_merged() {
    let fx = fixture();
    let book = fx.engine.contact_books().expect("books")[0].id;
    let save = |input: ContactInput| {
        let engine = fx.engine.clone();
        async move { engine.save_contact(input).await.expect("save") }
    };
    let ada = save(ContactInput {
        book_id: book,
        name: "Ada Lovelace".into(),
        emails: vec![lv("ada@example.com", "work")],
        organization: Some("Analytical Engines".into()),
        ..ContactInput::default()
    })
    .await;
    let ada_again = save(ContactInput {
        book_id: book,
        name: "Ada King".into(),
        emails: vec![
            lv("ADA@example.com", "home"),
            lv("ada@home.example", "home"),
        ],
        phones: vec![lv("+44 20 7946 0958", "mobile")],
        notes: Some("Countess of Lovelace".into()),
        ..ContactInput::default()
    })
    .await;
    // The same number written two ways.
    let charles = save(ContactInput {
        book_id: book,
        name: "Charles Babbage".into(),
        phones: vec![lv("+44 20 7946 0001", "work")],
        ..ContactInput::default()
    })
    .await;
    let charles_again = save(ContactInput {
        book_id: book,
        name: "C. Babbage".into(),
        phones: vec![lv("020 7946 0001", "work")],
        ..ContactInput::default()
    })
    .await;
    // One word is not enough to call two people the same.
    for _ in 0..2 {
        save(ContactInput {
            book_id: book,
            name: "Mom".into(),
            ..ContactInput::default()
        })
        .await;
    }

    let groups = fx.engine.contact_duplicates().await.expect("duplicates");
    let ids: Vec<Vec<i64>> = groups
        .iter()
        .map(|g| g.iter().map(|c| c.id).collect())
        .collect();
    // The fuller contact leads each group.
    assert_eq!(ids, [vec![ada_again, ada], vec![charles, charles_again]]);

    let merged = fx
        .engine
        .merge_contacts(vec![ada, ada_again])
        .await
        .expect("merge");
    assert_eq!(merged, 1);
    let kept = fx.row(ada).contact;
    assert_eq!(kept.name, "Ada Lovelace");
    assert_eq!(kept.organization.as_deref(), Some("Analytical Engines"));
    assert_eq!(kept.note.as_deref(), Some("Countess of Lovelace"));
    let emails: Vec<&str> = kept.emails.iter().map(|e| e.value.as_str()).collect();
    assert_eq!(emails, ["ada@example.com", "ada@home.example"]);
    assert_eq!(kept.phones.len(), 1);
    assert!(
        fx.engine.contact(ada_again).await.is_err(),
        "the merged contact is gone"
    );
    let groups = fx.engine.contact_duplicates().await.expect("duplicates");
    assert_eq!(groups.len(), 1);
}

// ---------------------------------------------------------------------------
// Contact groups

/// The PUT bodies a mock server received for `href`, in order.
async fn uploads(server: &MockServer, href: &str) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| {
            r.method.as_str() == "PUT"
                && envelope_dav::Url::parse(href).is_ok_and(|u| u.path() == r.url.path())
        })
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn carddav_groups_go_up_in_the_books_style_and_merge_server_edits() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = fx.account("alex@example.com");
    let collection = format!("{}/ab/", server.uri());
    let book = fx.book(&account, "carddav", &collection);
    let client = DavClient::new(&server.uri(), envelope_dav::Auth::None).expect("client");
    store_vcards(
        &fx.engine,
        book.id,
        vec![
            object(
                &format!("{collection}ada.vcf"),
                "\"1\"",
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada-1\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.com\r\nEND:VCARD\r\n",
            ),
            object(
                &format!("{collection}grace.vcf"),
                "\"1\"",
                "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:grace-1\r\nFN:Grace Hopper\r\nEND:VCARD\r\n",
            ),
        ],
        Vec::new(),
    )
    .await
    .expect("store");
    let ada = fx.id_of("Ada Lovelace").await;
    let grace = fx.id_of("Grace Hopper").await;
    let push = async |id: i64| {
        let row = fx.row(id);
        let book = fx.books(&account).remove(0);
        let style = fx
            .engine
            .db
            .read(|c| crate::contacts::groups::book_style(c, &book))
            .expect("style");
        let outcome = carddav_push(&client, &book, &row, None, style).await;
        settle(&fx.engine, &book, &row, outcome).expect("settle")
    };

    // No Apple groups there: a new group is a vCard 4.0 group card.
    let team = fx
        .engine
        .create_group(book.id, "Team", vec![ada])
        .await
        .expect("create");
    let href = fx.row(team).remote_id.expect("href");
    let at = href.trim_start_matches(&server.uri()).to_owned();
    Mock::given(method("PUT"))
        .and(path(&at))
        .and(header("If-None-Match", "*"))
        .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"g1\""))
        .expect(1)
        .mount(&server)
        .await;
    assert!(push(team).await);
    let sent = uploads(&server, &href).await.remove(0);
    for line in [
        "VERSION:4.0\r\n",
        "KIND:group\r\n",
        "FN:Team\r\n",
        "MEMBER:urn:uuid:ada-1\r\n",
    ] {
        assert!(sent.contains(line), "{line} missing in\n{sent}");
    }
    let row = fx.row(team);
    assert_eq!(row.local_state, cdb::SYNCED);
    assert_eq!(row.etag.as_deref(), Some("\"g1\""));
    assert!(row.is_group);

    // Someone else adds an address on the server while Grace is added here:
    // the upload finds the newer card and keeps both changes.
    let theirs = sent.replace(
        "MEMBER:urn:uuid:ada-1\r\n",
        "MEMBER:urn:uuid:ada-1\r\nMEMBER:mailto:boss@example.com\r\n",
    );
    Mock::given(method("PUT"))
        .and(path(&at))
        .and(header("If-Match", "\"g1\""))
        .respond_with(ResponseTemplate::new(412))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(&at))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"g2\"")
                .insert_header("Content-Type", "text/vcard")
                .set_body_string(theirs),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(&at))
        .and(header("If-Match", "\"g2\""))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"g3\""))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        fx.engine
            .add_to_group(team, vec![grace], Vec::new())
            .await
            .expect("add"),
        1
    );
    assert_eq!(fx.row(team).local_state, cdb::MODIFIED);
    assert!(push(team).await);
    let merged = uploads(&server, &href).await.pop().expect("upload");
    for line in [
        "MEMBER:urn:uuid:ada-1\r\n",
        "MEMBER:mailto:boss@example.com\r\n",
        "MEMBER:urn:uuid:grace-1\r\n",
    ] {
        assert!(merged.contains(line), "{line} missing in\n{merged}");
    }
    assert_eq!(fx.row(team).etag.as_deref(), Some("\"g3\""));
    let members: Vec<String> = fx
        .engine
        .contact_groups()
        .await
        .expect("groups")
        .remove(0)
        .members
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(
        members,
        ["Ada Lovelace", "boss@example.com", "Grace Hopper"]
    );

    // Once the address book has an Apple group, new ones are written so.
    store_vcards(
        &fx.engine,
        book.id,
        vec![object(
            &format!("{collection}family.vcf"),
            "\"f\"",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Family\r\nFN:Family\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nUID:family\r\nEND:VCARD\r\n",
        )],
        Vec::new(),
    )
    .await
    .expect("store");
    let friends = fx
        .engine
        .create_group(book.id, "Friends", vec![grace])
        .await
        .expect("create");
    let href = fx.row(friends).remote_id.expect("href");
    Mock::given(method("PUT"))
        .and(path(href.trim_start_matches(&server.uri())))
        .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"a1\""))
        .expect(1)
        .mount(&server)
        .await;
    assert!(push(friends).await);
    let sent = uploads(&server, &href).await.remove(0);
    for line in [
        "VERSION:3.0\r\n",
        "N:Friends;;;;\r\n",
        "X-ADDRESSBOOKSERVER-KIND:group\r\n",
        "X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:grace-1\r\n",
    ] {
        assert!(sent.contains(line), "{line} missing in\n{sent}");
    }
    assert!(!sent.contains("\r\nKIND:"), "{sent}");

    // Deleted here, deleted there.
    Mock::given(method("DELETE"))
        .and(path(&at))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    fx.engine.delete_group(team).await.expect("delete");
    assert!(push(team).await);
    assert!(
        fx.engine.db.read(|c| cdb::get_contact(c, team)).is_err(),
        "the row goes once the server deleted it"
    );
}

/// An Exchange contacts folder with a person and a contact group.
struct ExchangeGroup;

impl wiremock::Respond for ExchangeGroup {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        const NS: &str = concat!(
            r#"xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" "#,
            r#"xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types""#
        );
        const SOFIA: &str = r#"<t:Contact><t:ItemId Id="c1" ChangeKey="k1"/><t:DisplayName>Sofia Martínez</t:DisplayName><t:EmailAddresses><t:Entry Key="EmailAddress1">sofia@lumen.dev</t:Entry></t:EmailAddresses></t:Contact>"#;
        const TEAM: &str = r#"<t:DistributionList><t:ItemId Id="g1" ChangeKey="k1"/><t:DisplayName>Design team</t:DisplayName><t:Members><t:Member Key="1"><t:Mailbox><t:Name>Sofia</t:Name><t:EmailAddress>sofia@lumen.dev</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Contact</t:MailboxType><t:ItemId Id="c1"/></t:Mailbox></t:Member><t:Member Key="2"><t:Mailbox><t:Name>Maya Chen</t:Name><t:EmailAddress>maya@northwind.design</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>OneOff</t:MailboxType></t:Mailbox></t:Member><t:Member Key="3"><t:Mailbox><t:Name>Old list</t:Name><t:MailboxType>PrivateDL</t:MailboxType></t:Mailbox></t:Member></t:Members></t:DistributionList>"#;
        let body = String::from_utf8_lossy(&request.body).into_owned();
        let (op, messages) = if body.contains("<m:FindItem") {
            (
                "FindItem",
                format!(
                    r#"<m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder IndexedPagingOffset="2" TotalItemsInView="2" IncludesLastItemInRange="true"><t:Items>{SOFIA}<t:DistributionList><t:ItemId Id="g1" ChangeKey="k1"/><t:DisplayName>Design team</t:DisplayName></t:DistributionList></t:Items></m:RootFolder></m:FindItemResponseMessage>"#
                ),
            )
        } else {
            let items: String = body
                .split(r#"<t:ItemId Id=""#)
                .skip(1)
                .filter_map(|rest| rest.split('"').next())
                .map(|id| {
                    let item = if id == "c1" { SOFIA } else { TEAM };
                    format!(
                        r#"<m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{item}</m:Items></m:GetItemResponseMessage>"#
                    )
                })
                .collect();
            ("GetItem", items)
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/xml; charset=utf-8")
            .set_body_string(format!(
                r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Header><h:ServerVersionInfo MajorVersion="15" MinorVersion="1" MajorBuildNumber="2507" MinorBuildNumber="6" Version="V2017_07_11" xmlns:h="http://schemas.microsoft.com/exchange/services/2006/types"/></s:Header><s:Body><m:{op}Response {NS}><m:ResponseMessages>{messages}</m:ResponseMessages></m:{op}Response></s:Body></s:Envelope>"#
            ))
    }
}

#[tokio::test]
async fn exchange_contact_groups_are_read_only_lists() {
    use crate::model::{Address, AuthMethod, ExchangeAuth, ExchangeConfig};
    let fx = fixture();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(envelope_ews::EWS_PATH))
        .respond_with(ExchangeGroup)
        .mount(&server)
        .await;
    let config = AccountConfig {
        auth: AuthMethod::Password,
        exchange: Some(ExchangeConfig {
            ews_url: format!("{}{}", server.uri(), envelope_ews::EWS_PATH),
            auth: ExchangeAuth::Basic,
            ..ExchangeConfig::default()
        }),
        ..AccountConfig::default()
    };
    let id = fx
        .engine
        .db
        .write(|tx| {
            mail::insert_account(
                tx,
                AccountKind::Exchange,
                "exchange",
                "jane@example.com",
                "Jane Doe",
                "#0ea5e9",
                &config,
            )
        })
        .expect("account");
    fx.engine
        .secrets
        .preload(&crate::secrets::password_key(id), "secret");
    let account = fx.engine.account_row(id).expect("account row");
    // The second sync reads nothing again, and keeps the group.
    for _ in 0..2 {
        ews_sync(&fx.engine, &account, true).await.expect("sync");
        let groups = fx.engine.contact_groups().await.expect("groups");
        assert_eq!(groups.len(), 1);
        let team = &groups[0];
        assert_eq!(team.name, "Design team");
        assert!(team.read_only);
        assert_eq!(
            team.members
                .iter()
                .map(|m| (m.contact_id.is_some(), m.name.as_str()))
                .collect::<Vec<_>>(),
            [(false, "Maya Chen"), (true, "Sofia Martínez")],
            "the contact in it is the contact; another address keeps Exchange's name"
        );
    }
    let team = fx.engine.contact_groups().await.expect("groups")[0].id;
    assert!(fx.engine.rename_group(team, "Designers").await.is_err());
    assert!(fx.engine.delete_group(team).await.is_err());
    let suggested = fx.engine.suggest_groups("team", 5).expect("suggest");
    assert_eq!(suggested.len(), 1);
    assert_eq!(
        suggested[0].addresses,
        [
            Address::new(Some("Maya Chen".into()), "maya@northwind.design"),
            Address::new(Some("Sofia Martínez".into()), "sofia@lumen.dev"),
        ]
    );
    let names: Vec<String> = fx.list("").await.into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["Sofia Martínez"], "the group is not a contact");
}

//! CardDAV end-to-end test against the development server (Radicale):
//! discovery, the initial sync of server cards (vCard 3.0 and 4.0, labels,
//! photos, groups), creating, editing (including a conflicting server edit)
//! and deleting contacts, and picking up changes made on the server. A
//! second test does the same with contact groups, in address books of their
//! own (of sam@envelope.test, which it leaves as it found them).
//!
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_contacts -- --nocapture

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use envelope_core::contacts::{ContactInput, ContactView, LabeledValue};
use envelope_core::db::contacts as cdb;
use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};
use envelope_dav::{DavClient, NewAddressBook, ObjectKind, Precondition};
use parking_lot::Mutex;

const DAV: &str = "http://127.0.0.1:5232/";
const HOME: &str = "http://127.0.0.1:5232/alex@envelope.test/";

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);
impl EventSink for Events {
    fn emit(&self, e: EngineEvent) {
        self.0.lock().push(e);
    }
}

fn account() -> NewAccount {
    account_of("alex@envelope.test", "Alex Morgan")
}

fn account_of(email: &str, name: &str) -> NewAccount {
    NewAccount {
        email: email.into(),
        name: name.into(),
        provider: "generic".into(),
        password: "password".into(),
        config: AccountConfig {
            imap: Some(ServerConfig {
                host: "127.0.0.1".into(),
                port: 1993,
                security: Security::Tls,
                username: String::new(),
            }),
            smtp: Some(ServerConfig {
                host: "127.0.0.1".into(),
                port: 1587,
                security: Security::None,
                username: String::new(),
            }),
            carddav_url: Some(DAV.into()),
            accept_invalid_certs: true,
            ..Default::default()
        },
    }
}

/// Polls `check` until it yields a value (or fails after 20 seconds).
async fn until<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(value) = check().await {
            println!("{what}: {:?}", start.elapsed());
            return value;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn names(engine: &Engine) -> Vec<String> {
    engine
        .contacts_list("", None, 0, 100)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|c| c.name)
        .collect()
}

async fn find(engine: &Engine, name: &str) -> Option<ContactView> {
    let page = engine.contacts_list(name, None, 0, 10).await.ok()?;
    let id = page.items.iter().find(|c| c.name == name)?.id;
    engine.contact(id).await.ok()
}

fn state(engine: &Engine, id: i64) -> i64 {
    engine
        .db
        .read(|c| cdb::get_contact(c, id))
        .map(|r| r.local_state)
        .unwrap_or(-1)
}

fn input(view: &ContactView) -> ContactInput {
    ContactInput {
        id: Some(view.id),
        book_id: view.book_id,
        name: view.name.clone(),
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
    }
}

fn lv(value: &str, label: &str) -> LabeledValue {
    LabeledValue {
        value: value.into(),
        label: Some(label.into()),
    }
}

fn png_base64() -> String {
    let image = image_bytes();
    base64::engine::general_purpose::STANDARD.encode(image)
}

/// A small PNG.
fn image_bytes() -> Vec<u8> {
    let image =
        image::RgbImage::from_fn(16, 12, |x, y| image::Rgb([x as u8 * 15, y as u8 * 20, 120]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

/// Folds a content line the way servers do (75 octets, continuation lines
/// start with a space).
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    let mut width = 75;
    while rest.len() > width {
        out.push_str(&rest[..width]);
        out.push_str("\r\n ");
        rest = &rest[width..];
        width = 74;
    }
    out.push_str(rest);
    out.push_str("\r\n");
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn carddav_round_trip() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,envelope_core=debug,hyper=info,reqwest=info,rustls=info,h2=info")
        .try_init();
    let reset =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/devserver/reset.sh");
    assert!(
        std::process::Command::new(reset)
            .status()
            .unwrap()
            .success()
    );

    // Server-side address book with cards written by other clients.
    let dav = DavClient::new(
        DAV,
        envelope_dav::Auth::basic("alex@envelope.test", "password"),
    )
    .unwrap();
    let book_href = dav
        .create_addressbook(HOME, &NewAddressBook::new("Friends"))
        .await
        .unwrap();
    let put = |uid: &str, card: String| {
        let href = envelope_dav::object_href(&book_href, uid, ObjectKind::Contact).unwrap();
        let dav = dav.clone();
        async move {
            dav.put(&href, &card, Precondition::IfNoneMatch)
                .await
                .unwrap();
            href
        }
    };
    let zeynep = put(
        "zeynep-1",
        format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iPhone OS 17.4//EN\r\nUID:zeynep-1\r\n\
N:Yılmaz;Zeynep;;;\r\nFN:Zeynep Yılmaz\r\nORG:Acme Teknoloji A.Ş.;Ürün\r\n\
item1.EMAIL;type=INTERNET;type=pref:zeynep@icloud.example\r\nitem1.X-ABLabel:_$!<Home>!$_\r\n\
item2.EMAIL;type=INTERNET:zeynep@acme.example\r\nitem2.X-ABLabel:_$!<Work>!$_\r\n\
TEL;type=CELL;type=VOICE;type=pref:+90 532 111 22 33\r\n\
item3.ADR;type=HOME;type=pref:;;Bağdat Cd. 123;Kadıköy;İstanbul;34710;Türkiye\r\nitem3.X-ABADR:tr\r\n\
X-SOCIALPROFILE;type=twitter:https://twitter.com/zeyno\r\n{}END:VCARD\r\n",
            fold(&format!("PHOTO;ENCODING=b;TYPE=PNG:{}", png_base64()))
        ),
    )
    .await;
    let can = put(
        "can-1",
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:can-1\r\nFN:Can Öztürk\r\nN:Öztürk;Can;;;\r\n\
EMAIL;TYPE=work;PREF=1:can@work.example\r\nTEL;VALUE=uri;TYPE=\"cell,voice\":tel:+90-532-000-0000\r\n\
BDAY:--0415\r\nKIND:individual\r\nEND:VCARD\r\n"
            .into(),
    )
    .await;
    put(
        "family",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:family\r\nFN:Family\r\nX-ADDRESSBOOKSERVER-KIND:group\r\n\
X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:zeynep-1\r\nEND:VCARD\r\n"
            .into(),
    )
    .await;

    let dir = std::env::temp_dir().join(format!("envelope-contacts-{}", uuid::Uuid::new_v4()));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        events.clone(),
    )
    .unwrap();
    let local = engine.contact_books().unwrap();
    assert_eq!(local.len(), 1, "a local address book before any account");
    assert_eq!(local[0].kind, "local");
    engine.add_password_account(account()).await.unwrap();

    // Initial sync.
    let list = until("initial sync", || async {
        let n = names(&engine).await;
        (n.len() >= 2).then_some(n)
    })
    .await;
    assert_eq!(
        list,
        ["Can Öztürk", "Zeynep Yılmaz"],
        "the group card is not a contact"
    );
    let books = engine.contact_books().unwrap();
    assert_eq!(books.len(), 1, "the empty local book is gone: {books:?}");
    assert_eq!(books[0].kind, "carddav");
    assert_eq!(books[0].name, "Friends");
    assert!(!books[0].read_only);
    let book = books[0].id;
    assert!(events.0.lock().iter().any(
        |e| matches!(e, EngineEvent::ContactsChanged { book_ids } if book_ids.contains(&book))
    ));

    let z = find(&engine, "Zeynep Yılmaz").await.unwrap();
    assert_eq!(z.organization.as_deref(), Some("Acme Teknoloji A.Ş."));
    assert_eq!(
        z.emails,
        [
            lv("zeynep@icloud.example", "home"),
            lv("zeynep@acme.example", "work")
        ]
    );
    assert_eq!(z.phones, [lv("+90 532 111 22 33", "mobile")]);
    assert_eq!(
        z.addresses,
        [lv(
            "Bağdat Cd. 123\nKadıköy, İstanbul 34710\nTürkiye",
            "home"
        )]
    );
    assert_eq!(
        z.photo.as_deref(),
        Some(format!("data:image/png;base64,{}", png_base64()).as_str())
    );
    let c = find(&engine, "Can Öztürk").await.unwrap();
    assert_eq!(c.birthday.as_deref(), Some("--04-15"));
    assert_eq!(c.phones, [lv("+90-532-000-0000", "mobile")]);
    // List thumbnails are made in the background.
    until("thumbnail", || async {
        let page = engine.contacts_list("zeynep", None, 0, 1).await.ok()?;
        page.items.first()?.photo.clone()
    })
    .await;
    // Search: diacritics, phone digits, organization.
    for (query, name) in [
        ("yilmaz", "Zeynep Yılmaz"),
        ("ozturk", "Can Öztürk"),
        ("532 111", "Zeynep Yılmaz"),
        ("acme", "Zeynep Yılmaz"),
    ] {
        let page = engine.contacts_list(query, None, 0, 10).await.unwrap();
        assert_eq!(page.items.len(), 1, "{query}");
        assert_eq!(page.items[0].name, name, "{query}");
    }

    // Create a contact here; it is uploaded in the background.
    let photo = format!("data:image/png;base64,{}", png_base64());
    let new_id = engine
        .save_contact(ContactInput {
            id: None,
            book_id: book,
            name: "Ada Lovelace".into(),
            given_name: Some("Ada".into()),
            family_name: Some("Lovelace".into()),
            emails: vec![
                lv("ada@example.com", "work"),
                lv("ada@home.example", "Personal"),
            ],
            phones: vec![lv("+44 20 7946 0958", "mobile")],
            addresses: vec![lv("12 St James's Square\nLondon SW1Y 4JH", "home")],
            urls: vec!["https://ada.example".into()],
            organization: Some("Analytical Engines".into()),
            title: None,
            birthday: Some("1815-12-10".into()),
            notes: Some("First programmer".into()),
            photo: Some(photo.clone()),
        })
        .await
        .unwrap();
    until("upload", || async {
        (state(&engine, new_id) == cdb::SYNCED).then_some(())
    })
    .await;
    let href = engine
        .db
        .read(|c| cdb::get_contact(c, new_id))
        .unwrap()
        .remote_id
        .unwrap();
    let card = dav.get(&href).await.unwrap().data;
    let unfolded = card.replace("\r\n ", "");
    for expected in [
        "FN:Ada Lovelace",
        "N:Lovelace;Ada;;;",
        "EMAIL;TYPE=INTERNET,WORK:ada@example.com",
        "X-ABLABEL:Personal",
        "TEL;TYPE=CELL:+44 20 7946 0958",
        "ADR;TYPE=HOME:;;12 St James's Square\\nLondon SW1Y 4JH;;;;",
        "URL:https://ada.example",
        "BDAY:1815-12-10",
        "PHOTO;ENCODING=b;TYPE=PNG:",
    ] {
        assert!(unfolded.contains(expected), "missing {expected} in\n{card}");
    }

    // Edit it: only what changed is rewritten.
    let ada = engine.contact(new_id).await.unwrap();
    assert_eq!(ada.emails[1].label.as_deref(), Some("personal"));
    let mut edit = input(&ada);
    edit.phones.push(lv("+44 20 7946 0000", "work"));
    edit.title = Some("Countess".into());
    engine.save_contact(edit).await.unwrap();
    until("edit upload", || async {
        (state(&engine, new_id) == cdb::SYNCED).then_some(())
    })
    .await;
    let card = dav.get(&href).await.unwrap().data;
    assert!(card.contains("TITLE:Countess"), "{card}");
    assert!(card.contains("TEL;TYPE=WORK:+44 20 7946 0000"), "{card}");
    assert!(
        card.contains("item1.X-ABLABEL:Personal"),
        "unchanged entries keep their form: {card}"
    );

    // A conflict: Zeynep is changed on the server while edited here.
    let server = dav.get(&zeynep).await.unwrap();
    let mut theirs = envelope_core::envelope_ical::parse_vcard(&server.data).unwrap();
    theirs.title = Some("Kıdemli Mühendis".into());
    let changed = envelope_core::envelope_ical::upsert_contact(&server.data, &theirs).unwrap();
    dav.put(
        &zeynep,
        &changed,
        Precondition::IfMatch(server.etag.clone().unwrap()),
    )
    .await
    .unwrap();
    let mut edit = input(&z);
    edit.notes = Some("Kahve sever.".into());
    engine.save_contact(edit).await.unwrap();
    until("conflict upload", || async {
        (state(&engine, z.id) == cdb::SYNCED).then_some(())
    })
    .await;
    let merged = dav.get(&zeynep).await.unwrap().data;
    assert!(
        merged.contains("TITLE:Kıdemli Mühendis"),
        "server change kept: {merged}"
    );
    assert!(
        merged.contains("NOTE:Kahve sever."),
        "local change kept: {merged}"
    );
    assert!(
        merged.contains("X-SOCIALPROFILE;TYPE=twitter:https://twitter.com/zeyno")
            || merged.contains("X-SOCIALPROFILE;type=twitter:https://twitter.com/zeyno"),
        "unknown properties survive: {merged}"
    );
    let z = until("merged copy", || async {
        let z = find(&engine, "Zeynep Yılmaz").await?;
        (z.title.as_deref() == Some("Kıdemli Mühendis")).then_some(z)
    })
    .await;
    assert_eq!(z.notes.as_deref(), Some("Kahve sever."));

    // Delete one here; the server copy goes too.
    engine.delete_contact(c.id).await.unwrap();
    assert!(!names(&engine).await.contains(&"Can Öztürk".to_owned()));
    until("delete upload", || async {
        dav.get(&can)
            .await
            .err()
            .filter(|e| matches!(e, envelope_dav::DavError::NotFound { .. }))
    })
    .await;

    // Changes made on the server show up after a sync.
    put(
        "grace-1",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:grace-1\r\nFN:Grace Hopper\r\nEMAIL:grace@navy.example\r\nEND:VCARD\r\n"
            .into(),
    )
    .await;
    dav.delete(&href, None).await.unwrap();
    engine.sync_now(None);
    let list = until("server changes", || async {
        let n = names(&engine).await;
        (n.contains(&"Grace Hopper".to_owned()) && !n.contains(&"Ada Lovelace".to_owned()))
            .then_some(n)
    })
    .await;
    assert_eq!(list, ["Grace Hopper", "Zeynep Yılmaz"]);
    let suggestions = engine.suggest_contacts("grace", 5).unwrap();
    assert_eq!(suggestions[0].email, "grace@navy.example");
    assert_eq!(suggestions[0].name.as_deref(), Some("Grace Hopper"));

    // A fresh database sees the same server state.
    engine.shutdown();
    let engine2 = Engine::start(
        Paths {
            data: dir.join("d2"),
            cache: dir.join("c2"),
        },
        Events::default(),
    )
    .unwrap();
    engine2.add_password_account(account()).await.unwrap();
    let list = until("second engine", || async {
        let n = names(&engine2).await;
        (n.len() >= 2).then_some(n)
    })
    .await;
    assert_eq!(list, ["Grace Hopper", "Zeynep Yılmaz"]);
    let z2 = find(&engine2, "Zeynep Yılmaz").await.unwrap();
    assert_eq!(z2.notes.as_deref(), Some("Kahve sever."));
    assert_eq!(z2.title.as_deref(), Some("Kıdemli Mühendis"));
    engine2.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}

/// The groups of one address book: each group's name and its members' names.
async fn groups_of(engine: &Engine, book: i64) -> Vec<(String, Vec<String>)> {
    engine
        .contact_groups()
        .await
        .unwrap()
        .into_iter()
        .filter(|g| g.book_id == book)
        .map(|g| (g.name, g.members.into_iter().map(|m| m.name).collect()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn carddav_groups() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,envelope_core=debug,hyper=info,reqwest=info,rustls=info,h2=info")
        .try_init();
    const SAM: &str = "http://127.0.0.1:5232/sam@envelope.test/";

    // Two address books of sam's: one with an Apple group and the people in
    // it, one without groups.
    let dav = DavClient::new(
        DAV,
        envelope_dav::Auth::basic("sam@envelope.test", "password"),
    )
    .unwrap();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let family_book = format!("Family {}", &run[..8]);
    let work_book = format!("Work {}", &run[..8]);
    let family_href = dav
        .create_addressbook(SAM, &NewAddressBook::new(&family_book))
        .await
        .unwrap();
    let work_href = dav
        .create_addressbook(SAM, &NewAddressBook::new(&work_book))
        .await
        .unwrap();
    let put = |book: &str, uid: &str, card: String| {
        let href = envelope_dav::object_href(book, uid, ObjectKind::Contact).unwrap();
        let dav = dav.clone();
        async move {
            dav.put(&href, &card, Precondition::IfNoneMatch)
                .await
                .unwrap();
            href
        }
    };
    put(
        &family_href,
        "ada-1",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada-1\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nEMAIL:ada@example.com\r\nEND:VCARD\r\n".into(),
    )
    .await;
    put(
        &family_href,
        "grace-1",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:grace-1\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nEMAIL:grace@navy.example\r\nEND:VCARD\r\n".into(),
    )
    .await;
    let family = put(
        &family_href,
        "family-1",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//macOS 15.0//EN\r\nN:Family\r\nFN:Family\r\n\
X-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:ada-1\r\n\
X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:someone-elsewhere\r\nUID:family-1\r\nEND:VCARD\r\n"
            .into(),
    )
    .await;
    put(
        &work_href,
        "linus-1",
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:linus-1\r\nFN:Linus Torvalds\r\nEMAIL:linus@kernel.example\r\nEND:VCARD\r\n".into(),
    )
    .await;

    let dir = std::env::temp_dir().join(format!("envelope-groups-{run}"));
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Events::default(),
    )
    .unwrap();
    engine
        .add_password_account(account_of("sam@envelope.test", "Sam Lee"))
        .await
        .unwrap();
    let book_id = |name: &str| {
        engine
            .contact_books()
            .unwrap()
            .into_iter()
            .find(|b| b.name == name)
            .map(|b| b.id)
    };
    let (family_id, work_id) = until("initial sync", || async {
        let ids = (book_id(&family_book)?, book_id(&work_book)?);
        (groups_of(&engine, ids.0).await.len() == 1 && names(&engine).await.len() >= 3)
            .then_some(ids)
    })
    .await;
    assert_eq!(
        groups_of(&engine, family_id).await,
        [("Family".to_owned(), vec!["Ada Lovelace".to_owned()])],
        "the member that isn't here is not shown"
    );
    let group_id = |book: i64, name: &str| {
        let engine = engine.clone();
        let name = name.to_owned();
        async move {
            engine
                .contact_groups()
                .await
                .unwrap()
                .into_iter()
                .find(|g| g.book_id == book && g.name == name)
                .map(|g| g.id)
                .unwrap()
        }
    };
    let contact_id = |book: i64, name: &str| {
        let engine = engine.clone();
        let name = name.to_owned();
        async move {
            let page = engine
                .contacts_list(&name, Some(book), 0, 10)
                .await
                .unwrap();
            page.items.iter().find(|c| c.name == name).unwrap().id
        }
    };
    let synced = |id: i64| {
        let engine = engine.clone();
        async move { (state(&engine, id) == cdb::SYNCED).then_some(()) }
    };
    let href_of = |id: i64| {
        engine
            .db
            .read(|c| cdb::get_contact(c, id))
            .unwrap()
            .remote_id
            .unwrap()
    };
    let ada = contact_id(family_id, "Ada Lovelace").await;
    let grace = contact_id(family_id, "Grace Hopper").await;
    let linus = contact_id(work_id, "Linus Torvalds").await;

    // New groups go up the way their address book writes groups.
    let team = engine
        .create_group(family_id, "Team", vec![ada, grace])
        .await
        .unwrap();
    let kernel = engine
        .create_group(work_id, "Kernel", vec![linus])
        .await
        .unwrap();
    until("new groups uploaded", || async {
        synced(team).await?;
        synced(kernel).await
    })
    .await;
    let card = dav.get(&href_of(team)).await.unwrap().data;
    for line in [
        "VERSION:3.0",
        "FN:Team",
        "X-ADDRESSBOOKSERVER-KIND:group",
        "X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:ada-1",
        "X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:grace-1",
    ] {
        assert!(card.contains(line), "missing {line} in\n{card}");
    }
    let card = dav.get(&href_of(kernel)).await.unwrap().data;
    for line in [
        "VERSION:4.0",
        "KIND:group",
        "FN:Kernel",
        "MEMBER:urn:uuid:linus-1",
    ] {
        assert!(card.contains(line), "missing {line} in\n{card}");
    }

    // Renamed, and one member out.
    engine.rename_group(team, "Core team").await.unwrap();
    engine
        .remove_from_group(team, vec![grace], Vec::new())
        .await
        .unwrap();
    until("edit uploaded", || synced(team)).await;
    let card = dav.get(&href_of(team)).await.unwrap().data;
    assert!(card.contains("FN:Core team"), "{card}");
    assert!(card.contains("N:Core team;;;;"), "{card}");
    assert!(!card.contains("grace-1"), "{card}");

    // Family changes on the server while Grace joins here: both stay, and
    // so does the member Envelope doesn't know.
    let server = dav.get(&family).await.unwrap();
    let mut theirs = envelope_core::envelope_ical::parse_vcard(&server.data).unwrap();
    theirs.members.push("mailto:aunt@example.com".into());
    let changed = envelope_core::envelope_ical::upsert_contact(&server.data, &theirs).unwrap();
    dav.put(
        &family,
        &changed,
        Precondition::IfMatch(server.etag.clone().unwrap()),
    )
    .await
    .unwrap();
    let family_group = group_id(family_id, "Family").await;
    engine
        .add_to_group(family_group, vec![grace], Vec::new())
        .await
        .unwrap();
    until("conflict uploaded", || synced(family_group)).await;
    let card = dav.get(&family).await.unwrap().data;
    for member in [
        "urn:uuid:ada-1",
        "urn:uuid:someone-elsewhere",
        "mailto:aunt@example.com",
        "urn:uuid:grace-1",
    ] {
        assert!(
            card.contains(&format!("X-ADDRESSBOOKSERVER-MEMBER:{member}")),
            "missing {member} in\n{card}"
        );
    }
    let members = until("merged copy", || async {
        let groups = groups_of(&engine, family_id).await;
        let (_, members) = groups.into_iter().find(|(n, _)| n == "Family")?;
        (members.len() == 3).then_some(members)
    })
    .await;
    assert_eq!(
        members,
        ["Ada Lovelace", "aunt@example.com", "Grace Hopper"]
    );

    // Deleted here, gone there; the people stay.
    let kernel_href = href_of(kernel);
    engine.delete_group(kernel).await.unwrap();
    until("delete uploaded", || async {
        dav.get(&kernel_href)
            .await
            .err()
            .filter(|e| matches!(e, envelope_dav::DavError::NotFound { .. }))
    })
    .await;
    assert!(engine.contact(linus).await.is_ok());

    // Groups made on the server show up after a sync.
    put(
        &work_href,
        "ops-1",
        "BEGIN:VCARD\r\nVERSION:4.0\r\nKIND:group\r\nFN:Ops\r\nMEMBER:urn:uuid:linus-1\r\n\
MEMBER:mailto:oncall@kernel.example\r\nUID:ops-1\r\nEND:VCARD\r\n"
            .into(),
    )
    .await;
    engine.sync_now(None);
    let ops = until("server group", || async {
        let groups = groups_of(&engine, work_id).await;
        groups.into_iter().find(|(n, _)| n == "Ops").map(|(_, m)| m)
    })
    .await;
    assert_eq!(ops, ["Linus Torvalds", "oncall@kernel.example"]);
    let suggested = engine.suggest_groups("ops", 4).unwrap();
    assert_eq!(
        suggested[0]
            .addresses
            .iter()
            .map(|a| a.email.as_str())
            .collect::<Vec<_>>(),
        ["linus@kernel.example", "oncall@kernel.example"]
    );

    engine.shutdown();
    for href in [&family_href, &work_href] {
        dav.delete(href, None).await.unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}

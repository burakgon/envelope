//! Contact groups in one engine with a real (temporary) database: groups of
//! an address book on this computer made, renamed, filled, emptied and
//! deleted; groups synced from a CardDAV server with members of every kind;
//! and the groups the composer offers.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use super::*;
use crate::contacts::{ContactInput, LabeledValue};
use crate::db::contacts::{BookUpsert, SyncedContact};
use crate::db::mail;
use crate::engine::{EventSink, Paths};
use crate::model::{AccountConfig, AccountKind};

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
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
    let dir =
        std::env::temp_dir().join(format!("envelope-groups-{}", uuid::Uuid::new_v4().simple()));
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
    async fn person(&self, book: i64, name: &str, emails: &[&str]) -> i64 {
        self.engine
            .save_contact(ContactInput {
                book_id: book,
                name: name.into(),
                emails: emails
                    .iter()
                    .map(|e| LabeledValue {
                        value: (*e).into(),
                        label: None,
                    })
                    .collect(),
                ..ContactInput::default()
            })
            .await
            .expect("contact")
    }

    async fn groups(&self) -> Vec<ContactGroup> {
        self.engine.contact_groups().await.expect("groups")
    }

    /// Each group's name and its members' names.
    async fn names(&self) -> Vec<(String, Vec<String>)> {
        self.groups()
            .await
            .into_iter()
            .map(|g| (g.name, g.members.into_iter().map(|m| m.name).collect()))
            .collect()
    }

    fn row(&self, id: i64) -> ContactRow {
        self.engine
            .db
            .read(|c| cdb::get_contact(c, id))
            .expect("row")
    }

    fn changed_books(&self) -> Vec<i64> {
        self.events
            .0
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

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groups_of_a_local_address_book() {
    let fx = fixture();
    let book = fx.engine.contact_books().expect("books")[0].id;
    let ada = fx
        .person(
            book,
            "Ada Lovelace",
            &["ada@work.example", "ada@home.example"],
        )
        .await;
    let grace = fx
        .person(book, "Grace Hopper", &["grace@navy.example"])
        .await;
    let linus = fx.person(book, "Linus", &[]).await;
    let other_book = fx
        .engine
        .db
        .write(|tx| cdb::create_local_book(tx, "Other"))
        .expect("book");
    let stranger = fx
        .person(other_book, "Stranger", &["stranger@x.example"])
        .await;

    let id = fx
        .engine
        .create_group(book, " Design team ", vec![grace, linus, ada, stranger])
        .await
        .expect("create");
    let groups = fx.groups().await;
    assert_eq!(groups.len(), 1);
    let group = &groups[0];
    assert_eq!((group.id, group.book_id), (id, book));
    assert_eq!(group.name, "Design team");
    assert!(!group.read_only);
    assert_eq!(
        group
            .members
            .iter()
            .map(|m| (m.contact_id, m.name.as_str(), m.email.as_deref()))
            .collect::<Vec<_>>(),
        [
            (Some(ada), "Ada Lovelace", Some("ada@work.example")),
            (Some(grace), "Grace Hopper", Some("grace@navy.example")),
            (Some(linus), "Linus", None),
        ],
        "by name, without the contact of another address book"
    );
    let row = fx.row(id);
    assert!(row.is_group);
    assert_eq!(row.local_state, cdb::SYNCED, "nothing to upload");
    assert_eq!(row.contact.members.len(), 3);
    assert!(
        row.contact
            .members
            .iter()
            .all(|m| m.starts_with("urn:uuid:"))
    );
    assert!(fx.changed_books().contains(&book));

    // A group is no contact: not listed, not counted, not suggested as one.
    let page = fx
        .engine
        .contacts_list("", None, 0, 100)
        .await
        .expect("list");
    assert_eq!(page.total, 4);
    assert!(page.items.iter().all(|c| c.name != "Design team"));
    assert!(
        fx.engine
            .suggest_contacts("design", 8)
            .expect("suggest")
            .is_empty()
    );

    // Names: none, or one another group of the address book has, are refused.
    for name in ["  ", "design TEAM"] {
        assert!(
            fx.engine
                .create_group(book, name, Vec::new())
                .await
                .is_err(),
            "{name:?}"
        );
    }
    fx.engine
        .create_group(other_book, "Design team", Vec::new())
        .await
        .expect("the same name in another address book");
    fx.engine
        .rename_group(id, "Designers")
        .await
        .expect("rename");
    assert_eq!(fx.groups().await[0].name, "Designers");
    fx.engine
        .rename_group(id, "designers")
        .await
        .expect("its own name, in other letters");

    // People join once, and only from the group's address book.
    assert_eq!(
        fx.engine
            .add_to_group(id, vec![ada, stranger], Vec::new())
            .await
            .expect("add"),
        0
    );
    let zoe = fx
        .person(book, "Zoe Garcia", &["zoe@umbrella.example"])
        .await;
    assert_eq!(
        fx.engine
            .add_to_group(id, vec![zoe], Vec::new())
            .await
            .expect("add"),
        1
    );
    assert_eq!(
        fx.engine
            .remove_from_group(id, vec![grace], Vec::new())
            .await
            .expect("remove"),
        1
    );
    // An address joins as the contact who has it, or as itself (and leaves
    // again, as Undo would have it).
    assert_eq!(
        fx.engine
            .add_to_group(
                id,
                Vec::new(),
                strings(&["ADA@home.example", "x@y.example", "not an address"])
            )
            .await
            .expect("add"),
        1
    );
    let group = fx.groups().await.remove(0);
    let x = group
        .members
        .iter()
        .find(|m| m.contact_id.is_none())
        .expect("the address");
    assert_eq!(
        (x.name.as_str(), x.email.as_deref()),
        ("x@y.example", Some("x@y.example"))
    );
    assert_eq!(
        fx.engine
            .remove_from_group(id, Vec::new(), strings(&["X@Y.example"]))
            .await
            .expect("remove"),
        1
    );
    assert_eq!(
        fx.names().await,
        [
            (
                "designers".to_owned(),
                strings(&["Ada Lovelace", "Linus", "Zoe Garcia"])
            ),
            ("Design team".to_owned(), Vec::new()),
        ],
        "in the address books' order"
    );

    // The composer: members' primary addresses, without the one who has none.
    let suggested = fx.engine.suggest_groups("DESIG", 5).expect("suggest");
    let designers = suggested
        .iter()
        .find(|s| s.id == id)
        .expect("the group is suggested");
    assert_eq!(designers.count, 3);
    assert_eq!(
        designers.addresses,
        [
            Address::new(Some("Ada Lovelace".into()), "ada@work.example"),
            Address::new(Some("Zoe Garcia".into()), "zoe@umbrella.example"),
        ]
    );
    assert!(
        suggested.iter().all(|s| s.name != "Design team"),
        "a group without addresses is not offered"
    );
    assert!(
        fx.engine
            .suggest_groups("signers", 5)
            .expect("suggest")
            .is_empty()
    );

    // Deleting a contact leaves the group; deleting the group leaves the people.
    fx.engine.delete_contact(zoe).await.expect("delete");
    let designers = fx
        .groups()
        .await
        .into_iter()
        .find(|g| g.id == id)
        .expect("group");
    assert_eq!(designers.members.len(), 2);
    fx.engine.delete_group(id).await.expect("delete");
    assert!(fx.groups().await.iter().all(|g| g.id != id));
    assert!(fx.engine.contact(ada).await.is_ok());
    assert!(fx.engine.rename_group(id, "Again").await.is_err());
    assert!(
        fx.engine.delete_group(ada).await.is_err(),
        "a contact is not a group"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merging_duplicates_keeps_them_in_their_groups() {
    let fx = fixture();
    let book = fx.engine.contact_books().expect("books")[0].id;
    let daniel = fx
        .person(book, "Daniel Okafor", &["daniel@lumen.example"])
        .await;
    let dan = fx.person(book, "Dan Okafor", &["dan@okafor.example"]).await;
    let team = fx
        .engine
        .create_group(book, "Team", vec![dan])
        .await
        .expect("group");
    assert_eq!(
        fx.engine
            .merge_contacts(vec![daniel, dan])
            .await
            .expect("merge"),
        1
    );
    let groups = fx.groups().await;
    let team = groups.iter().find(|g| g.id == team).expect("group");
    assert_eq!(
        team.members
            .iter()
            .map(|m| m.contact_id)
            .collect::<Vec<_>>(),
        [Some(daniel)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groups_from_a_carddav_server() {
    let fx = fixture();
    let account = fx
        .engine
        .db
        .write(|tx| {
            mail::insert_account(
                tx,
                AccountKind::Imap,
                "generic",
                "alex@example.com",
                "Alex",
                "#0ea5e9",
                &AccountConfig::default(),
            )
        })
        .expect("account");
    fx.engine
        .db
        .write(|tx| {
            cdb::sync_books(
                tx,
                account,
                "carddav",
                &[BookUpsert {
                    remote_id: "https://dav.example.com/ab/".into(),
                    name: "Friends".into(),
                    read_only: false,
                    is_default: true,
                    sort_order: 0,
                }],
            )
        })
        .expect("book");
    let book = fx
        .engine
        .db
        .read(|c| cdb::account_books(c, account))
        .expect("books")
        .remove(0);
    let card = |href: &str, data: &str| {
        let (contact, kind) = vcard::parse_vcard_with_kind(data).expect("card");
        SyncedContact {
            remote_id: format!("https://dav.example.com/ab/{href}"),
            etag: Some("\"1\"".into()),
            uid: contact.uid.clone(),
            contact,
            base: Some(data.to_owned()),
            photo: cdb::PhotoChange::Keep,
            is_group: kind.as_deref() == Some("group"),
        }
    };
    let family = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Family\r\nFN:Family\r\nX-ADDRESSBOOKSERVER-KIND:group\r\n\
X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:zeynep-1\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:CAN-1\r\n\
X-ADDRESSBOOKSERVER-MEMBER:mailto:ahmet@kaya.example\r\nX-ADDRESSBOOKSERVER-MEMBER:mailto:aunt@x.example\r\n\
X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:not-here\r\nUID:family\r\nEND:VCARD\r\n";
    let ids = fx
        .engine
        .db
        .write(|tx| {
            let mut ids = Vec::new();
            for item in [
                card(
                    "zeynep.vcf",
                    "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:zeynep-1\r\nFN:Zeynep Yılmaz\r\nEMAIL:zeynep@x.example\r\nEND:VCARD\r\n",
                ),
                card(
                    "can.vcf",
                    "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:can-1\r\nFN:Can Öztürk\r\nEND:VCARD\r\n",
                ),
                // No UID: found by its address.
                card(
                    "ahmet.vcf",
                    "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ahmet Kaya\r\nEMAIL:Ahmet@Kaya.example\r\nEND:VCARD\r\n",
                ),
                card("family.vcf", family),
            ] {
                ids.push(cdb::upsert_synced(tx, book.id, &item)?.expect("stored"));
            }
            Ok(ids)
        })
        .expect("store");
    let family_id = ids[3];
    assert_eq!(
        fx.engine.db.read(|c| book_style(c, &book)).expect("style"),
        GroupStyle::Apple,
        "new groups are written like the ones there"
    );

    let check = |groups: Vec<ContactGroup>| {
        assert_eq!(groups.len(), 1);
        let family = &groups[0];
        assert_eq!(family.name, "Family");
        assert_eq!(
            family
                .members
                .iter()
                .map(|m| (m.contact_id.is_some(), m.name.as_str()))
                .collect::<Vec<_>>(),
            [
                (true, "Ahmet Kaya"),
                (false, "aunt@x.example"),
                (true, "Can Öztürk"),
                (true, "Zeynep Yılmaz"),
            ],
            "members by UID in any letters, by address, an address alone; not the card that isn't here"
        );
    };
    check(fx.groups().await);
    // Groups stored before Envelope read members find them in the server's copy.
    fx.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE contacts SET data = ? WHERE id = ?",
                rusqlite::params![
                    serde_json::to_string(&Contact {
                        uid: "family".into(),
                        name: "Family".into(),
                        family_name: Some("Family".into()),
                        ..Contact::default()
                    })?,
                    family_id
                ],
            )?;
            Ok(())
        })
        .expect("old row");
    check(fx.groups().await);

    // An edit keeps the member Envelope doesn't know, and waits for upload.
    assert_eq!(
        fx.engine
            .remove_from_group(family_id, vec![ids[1]], strings(&["AUNT@x.example"]))
            .await
            .expect("remove"),
        2
    );
    fx.engine
        .rename_group(family_id, "Kaya family")
        .await
        .expect("rename");
    let row = fx.row(family_id);
    assert_eq!(row.local_state, cdb::MODIFIED);
    assert_eq!(
        row.contact.members,
        [
            "urn:uuid:zeynep-1",
            "mailto:ahmet@kaya.example",
            "urn:uuid:not-here"
        ]
    );
    assert_eq!(row.contact.name, "Kaya family");
    assert_eq!(
        row.contact.family_name.as_deref(),
        Some("Kaya family"),
        "Apple's N follows the name"
    );
    let card =
        vcard::upsert_contact(row.base.as_deref().unwrap_or_default(), &row.contact).expect("card");
    assert!(card.contains("N:Kaya family;;;;\r\n"), "{card}");
    assert!(card.contains("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:not-here\r\n"));

    // A new group in this address book goes up as a new card.
    let new = fx
        .engine
        .create_group(book.id, "Neighbours", vec![ids[0]])
        .await
        .expect("create");
    let row = fx.row(new);
    assert_eq!(row.local_state, cdb::CREATED);
    assert!(row.is_group);
    assert!(
        row.remote_id
            .as_deref()
            .is_some_and(|href| href.starts_with("https://dav.example.com/ab/"))
    );
    assert_eq!(row.contact.members, ["urn:uuid:zeynep-1"]);
    // Deleting it before it went up still has to ask the server (an upload
    // whose answer got lost may have made it).
    fx.engine.delete_group(new).await.expect("delete");
    assert_eq!(fx.row(new).local_state, cdb::DELETED);
    assert!(fx.groups().await.iter().all(|g| g.id != new));

    // Read-only address books keep their groups as they are.
    fx.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE address_books SET read_only = 1 WHERE id = ?",
                [book.id],
            )?;
            Ok(())
        })
        .expect("read-only");
    assert!(fx.groups().await[0].read_only);
    assert!(fx.engine.rename_group(family_id, "X").await.is_err());
    assert!(
        fx.engine
            .create_group(book.id, "Y", Vec::new())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn icloud_address_books_get_apple_groups() {
    let fx = fixture();
    let book = |remote_id: &str| BookRow {
        id: 999,
        account_id: Some(1),
        kind: "carddav".into(),
        remote_id: remote_id.into(),
        name: "Book".into(),
        read_only: false,
        is_default: true,
        sync_state: cdb::BookSyncState::default(),
    };
    let style = |remote_id: &str| {
        fx.engine
            .db
            .read(|c| book_style(c, &book(remote_id)))
            .expect("style")
    };
    assert_eq!(
        style("https://p42-contacts.icloud.com/123/carddavhome/card/"),
        GroupStyle::Apple
    );
    assert_eq!(
        style("https://dav.example.com/alex/contacts/"),
        GroupStyle::Standard
    );
    assert_eq!(
        style("https://icloud.com.example.net/ab/"),
        GroupStyle::Standard
    );
}

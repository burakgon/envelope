//! Birthdays read from contacts saved in a real (temporary) database.

use std::path::PathBuf;

use crate::contacts::ContactInput;
use crate::engine::{Engine, EventSink, Paths};
use crate::model::EngineEvent;

use super::Birthday;

struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _: EngineEvent) {}
}

struct Fixture {
    engine: Engine,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn birthdays_come_from_contacts_once_each() {
    let dir = std::env::temp_dir().join(format!("envelope-birthdays-{}", uuid::Uuid::new_v4()));
    let f = Fixture {
        engine: Engine::start(
            Paths {
                data: dir.join("data"),
                cache: dir.join("cache"),
            },
            NoEvents,
        )
        .unwrap(),
        dir,
    };
    f.engine.ensure_local_book().unwrap();
    let book = f.engine.contact_books().unwrap()[0].id;
    let save = |name: &str, birthday: Option<&str>| {
        f.engine.save_contact(ContactInput {
            book_id: book,
            name: name.into(),
            birthday: birthday.map(str::to_string),
            ..ContactInput::default()
        })
    };
    let maya = save("Maya Chen", Some("1992-04-15")).await.unwrap();
    let kenji = save("Kenji Tanaka", Some("--02-29")).await.unwrap();
    save("Daniel Okafor", None).await.unwrap();
    // The same person twice, as after an import.
    save("Maya Chen", Some("1992-04-15")).await.unwrap();

    assert_eq!(
        f.engine.birthdays().unwrap(),
        // In the address book's order.
        [
            Birthday {
                contact_id: kenji,
                name: "Kenji Tanaka".into(),
                date: "--02-29".into(),
            },
            Birthday {
                contact_id: maya,
                name: "Maya Chen".into(),
                date: "1992-04-15".into(),
            },
        ]
    );
}

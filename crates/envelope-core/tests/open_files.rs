//! Opening calendar and message files: events added to the local calendar
//! once, guests dropped, and messages read with their attachments.

use envelope_core::files::OpenedFile;
use envelope_core::model::EngineEvent;
use envelope_core::{Engine, EventSink, Paths};

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team\r\n\
BEGIN:VEVENT\r\nUID:a@test\r\nDTSTART:20261110T090000Z\r\nDTEND:20261110T100000Z\r\nSUMMARY:Planning\r\n\
ORGANIZER:mailto:boss@example.com\r\nATTENDEE:mailto:someone@example.com\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:b@test\r\nDTSTART;VALUE=DATE:20261111\r\nDTEND;VALUE=DATE:20261112\r\nSUMMARY:Offsite\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

#[tokio::test]
async fn calendar_files_are_added_once() {
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let dir = std::env::temp_dir().join(format!("envelope-open-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Quiet,
    )
    .unwrap();
    engine.ensure_local_calendar().unwrap();
    let local = engine
        .calendars()
        .unwrap()
        .into_iter()
        .find(|c| c.kind == "local")
        .unwrap();

    // Recognized by content, whatever the name.
    let path = dir.join("team-invite.txt");
    std::fs::write(&path, ICS).unwrap();
    let OpenedFile::Calendar(file) = engine.open_file(&path).await.unwrap() else {
        panic!("not a calendar");
    };
    assert_eq!(file.name.as_deref(), Some("Team"));
    assert_eq!(file.events.len(), 2);
    assert!(file.events.iter().all(|e| !e.exists));
    assert!(file.events[1].all_day);

    let result = engine
        .import_calendar_file(&path, local.id, None)
        .await
        .unwrap();
    assert_eq!((result.added, result.skipped), (2, 0));
    let OpenedFile::Calendar(again) = engine.open_file(&path).await.unwrap() else {
        panic!("not a calendar");
    };
    assert!(again.events.iter().all(|e| e.exists));
    let result = engine
        .import_calendar_file(&path, local.id, None)
        .await
        .unwrap();
    assert_eq!((result.added, result.skipped), (0, 2));

    // The copy carries no guests, so nothing would ever be sent to them.
    let occurrences = engine
        .occurrences(1_793_000_000_000, 1_796_000_000_000)
        .unwrap();
    let planning = occurrences.iter().find(|o| o.title == "Planning").unwrap();
    assert!(!planning.has_attendees);

    // A message file with an attachment.
    let eml = dir.join("note.eml");
    std::fs::write(
        &eml,
        "From: Sam <sam@example.com>\r\nTo: alex@example.com\r\nSubject: Notes\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n\
--x\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=\"a.csv\"\r\n\r\n1,2\r\n--x--\r\n",
    )
    .unwrap();
    let OpenedFile::Message(message) = engine.open_file(&eml).await.unwrap() else {
        panic!("not a message");
    };
    assert_eq!(message.subject, "Notes");
    assert_eq!(message.attachments.len(), 1);
    let saved = engine
        .file_part(&eml, &message.attachments[0].part_id)
        .await
        .unwrap();
    assert_eq!(saved.file_name().unwrap(), "a.csv");
    assert_eq!(std::fs::read_to_string(saved).unwrap().trim(), "1,2");

    // Anything else is refused with an explanation.
    let other = dir.join("notes.txt");
    std::fs::write(&other, "just words").unwrap();
    assert!(engine.open_file(&other).await.is_err());

    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn contact_cards_are_added_once() {
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let dir = std::env::temp_dir().join(format!("envelope-open-vcf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Quiet,
    )
    .unwrap();
    engine.ensure_local_book().unwrap();
    let book = engine.contact_books().unwrap()[0].id;

    let path = dir.join("team.vcf");
    std::fs::write(
        &path,
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Maya Chen\r\nEMAIL:maya@northwind.design\r\nORG:Northwind\r\nEND:VCARD\r\n\
         BEGIN:VCARD\r\nVERSION:4.0\r\nKIND:group\r\nFN:Design team\r\nEND:VCARD\r\n\
         BEGIN:VCARD\r\nVERSION:3.0\r\nN:Okafor;Daniel;;;\r\nEMAIL:daniel@lumen.dev\r\nTEL;TYPE=CELL:+44 7700 900461\r\nEND:VCARD\r\n",
    )
    .unwrap();
    let OpenedFile::Contacts(file) = engine.open_file(&path).await.unwrap() else {
        panic!("not contacts");
    };
    // The group card is left out; names come from N when FN is missing.
    assert_eq!(
        file.contacts
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["Maya Chen", "Daniel Okafor"]
    );
    assert!(file.contacts.iter().all(|c| !c.exists));

    // Only Maya at first, then the whole file: she is not added twice.
    let first = engine
        .import_contacts_file(&path, book, Some(vec![0]))
        .await
        .unwrap();
    assert_eq!((first.added, first.skipped), (1, 0));
    let again = engine
        .import_contacts_file(&path, book, None)
        .await
        .unwrap();
    assert_eq!((again.added, again.skipped), (1, 1));
    let OpenedFile::Contacts(file) = engine.open_file(&path).await.unwrap() else {
        panic!("not contacts");
    };
    assert!(file.contacts.iter().all(|c| c.exists));
    let page = engine.contacts_list("", None, 0, 10).await.unwrap();
    assert_eq!(page.total, 2);

    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

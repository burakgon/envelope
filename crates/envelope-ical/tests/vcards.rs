//! vCard parsing, writing and in-place editing.

mod common;

use common::{date, text};
use envelope_ical::vcard::{NO_YEAR, parse_vcards_bytes};
use envelope_ical::{
    Contact, ContactField, Error, contact_to_vcard, parse_components, parse_vcard, parse_vcards,
    upsert_contact,
};

fn field(value: &str, kind: Option<&str>, preferred: bool) -> ContactField {
    ContactField {
        value: value.into(),
        kind: kind.map(str::to_owned),
        preferred,
    }
}

const JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAALCAABAAEBAREA/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAIAQEAAD8AVN//2Q==";

#[test]
fn icloud_card() {
    let contact = parse_vcard(text(include_bytes!("fixtures/icloud.vcf"))).expect("card");
    assert_eq!(contact.uid, "4C1D3E2F-1A2B-4C5D-8E9F-0A1B2C3D4E5F");
    assert_eq!(contact.name, "Zeynep Yılmaz");
    assert_eq!(contact.given_name.as_deref(), Some("Zeynep"));
    assert_eq!(contact.family_name.as_deref(), Some("Yılmaz"));
    assert_eq!(contact.nickname.as_deref(), Some("Zeyno"));
    assert_eq!(contact.organization.as_deref(), Some("Acme Teknoloji A.Ş."));
    assert_eq!(contact.title.as_deref(), Some("Kıdemli Mühendis"));
    assert_eq!(
        contact.emails,
        [
            field("zeynep@icloud.com", Some("home"), true),
            field("zeynep.yilmaz@acme.com.tr", Some("work"), false),
            field("zeynep.blog@example.org", Some("blog"), false),
        ]
    );
    assert_eq!(
        contact.phones,
        [
            field("+90 532 111 22 33", Some("cell"), true),
            field("+90 555 444 33 22", Some("cell"), false),
            field("+90 212 999 88 77", Some("work"), false),
        ]
    );
    assert_eq!(contact.birthday, Some(date("1604-08-17")));
    assert_eq!(
        contact.note.as_deref(),
        Some("Kahve sever.\nTercihen Türk kahvesi.")
    );
    assert_eq!(
        contact.photo.as_deref(),
        Some(format!("data:image/jpeg;base64,{JPEG}").as_str())
    );
}

#[test]
fn google_export_with_several_cards() {
    let contacts = parse_vcards(text(include_bytes!("fixtures/google.vcf")));
    assert_eq!(contacts.len(), 3);
    let ahmet = &contacts[0];
    assert_eq!(ahmet.name, "Ahmet Kaya");
    assert_eq!(
        ahmet.emails[0],
        field("ahmet.kaya@gmail.com", Some("home"), false)
    );
    assert_eq!(
        ahmet.emails[1],
        field("ahmet@kaya-hukuk.com.tr", Some("work"), false)
    );
    assert_eq!(
        ahmet.phones,
        [
            field("+905321234567", Some("cell"), false),
            field("+902161234567", Some("work"), false)
        ]
    );
    assert_eq!(ahmet.birthday, Some(date("1984-11-03")));
    assert_eq!(ahmet.organization.as_deref(), Some("Kaya Hukuk Bürosu"));
    assert_eq!(
        ahmet.photo.as_deref(),
        Some("https://lh3.googleusercontent.com/contacts/AG6tpzE5mFx2kQ=s100")
    );
    let john = &contacts[1];
    assert_eq!(
        john.phones[1],
        field("+1 415 555 0199", Some("grandma's house"), false)
    );
    assert_eq!(john.emails[0].kind, None, "INTERNET alone is not a kind");
    let nameless = &contacts[2];
    assert_eq!(
        nameless.name, "noname@example.com",
        "display name falls back to the e-mail"
    );
    assert_eq!(nameless.emails[0].kind.as_deref(), Some("other"));
}

#[test]
fn outlook_vcard_21_with_quoted_printable() {
    let contacts = parse_vcards_bytes(include_bytes!("fixtures/outlook21.vcf"));
    assert_eq!(contacts.len(), 2);
    let jurgen = &contacts[0];
    assert_eq!(jurgen.name, "Jürgen Müller");
    assert_eq!(jurgen.family_name.as_deref(), Some("Müller"));
    assert_eq!(jurgen.given_name.as_deref(), Some("Jürgen"));
    assert_eq!(jurgen.organization.as_deref(), Some("Contoso GmbH"));
    assert_eq!(
        jurgen.phones,
        [
            field("+49 30 1234567", Some("work"), false),
            field("+49 170 1234567", Some("cell"), false),
            field("+49 30 1234568", Some("fax"), false),
        ]
    );
    assert_eq!(
        jurgen.emails,
        [
            field("juergen.mueller@contoso.de", None, true),
            field("jm@privat.example", None, false)
        ]
    );
    assert_eq!(jurgen.birthday, Some(date("1975-03-12")));
    assert_eq!(
        jurgen.note.as_deref(),
        Some("Bevorzugt Anrufe vormittags – nicht nach 18 Uhr.")
    );
    assert_eq!(
        jurgen.photo.as_deref(),
        Some(format!("data:image/jpeg;base64,{JPEG}").as_str())
    );

    let ismail = &contacts[1];
    assert_eq!(ismail.family_name.as_deref(), Some("Şık"));
    assert_eq!(ismail.given_name.as_deref(), Some("İsmail"));
    assert_eq!(
        ismail.name, "İsmail Şık",
        "FN is derived from N when missing"
    );
    assert_eq!(
        ismail.phones,
        [
            field("+905551112233", Some("cell"), false),
            field("+902125556677", Some("home"), true)
        ]
    );
}

#[test]
fn nextcloud_cards_in_versions_3_and_4() {
    let contacts = parse_vcards(text(include_bytes!("fixtures/nextcloud.vcf")));
    assert_eq!(contacts.len(), 2);
    let elif = &contacts[0];
    assert_eq!(
        elif.phones[0],
        field("+90 216 111 22 33", Some("home"), false),
        "quoted TYPE lists"
    );
    assert_eq!(
        elif.note.as_deref(),
        Some("Nextcloud contact, with commas; and semicolons")
    );
    assert!(
        elif.photo
            .as_deref()
            .is_some_and(|p| p.starts_with("data:image/png;base64,iVBORw0KGgo"))
    );
    let can = &contacts[1];
    assert_eq!(can.uid, "urn:uuid:f81d4fae-7dec-11d0-a765-00a0c91e6bf6");
    assert_eq!(
        can.emails,
        [
            field("can@work.example", Some("work"), true),
            field("can@home.example", Some("home"), false)
        ]
    );
    assert_eq!(can.phones, [field("+90-532-000-0000", Some("cell"), true)]);
    assert_eq!(
        can.birthday,
        chrono::NaiveDate::from_ymd_opt(NO_YEAR, 4, 15)
    );
    assert_eq!(
        can.photo.as_deref(),
        Some(format!("data:image/jpeg;base64,{JPEG}").as_str())
    );
}

#[test]
fn written_cards_round_trip() {
    for (name, bytes) in common::VCF_FIXTURES {
        for contact in parse_vcards_bytes(bytes) {
            let written = contact_to_vcard(&contact);
            assert!(
                written.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n"),
                "{name}"
            );
            assert!(written.lines().all(|l| l.len() <= 76), "{name}");
            let reparsed = parse_vcard(&written).expect("card");
            assert_eq!(reparsed, contact, "{name}");
        }
    }
}

#[test]
fn upsert_preserves_unknown_properties() {
    let original = text(include_bytes!("fixtures/icloud.vcf"));
    let mut contact = parse_vcard(original).expect("card");
    contact.phones[2].value = "+90 212 999 00 00".into();
    contact.note = Some("Çay da olur.".into());
    contact.emails.remove(2);
    let updated = upsert_contact(original, &contact).expect("upsert");
    for kept in [
        "X-SOCIALPROFILE;TYPE=twitter:https://twitter.com/zeyno",
        "item5.ADR;TYPE=HOME;TYPE=pref:;;Bağdat Cd. 123;Kadıköy;İstanbul;34710;Türkiye",
        "item5.X-ABADR:tr",
        "X-ADDRESSBOOKSERVER-KIND:individual",
        "item1.EMAIL;TYPE=INTERNET;TYPE=pref:zeynep@icloud.com",
        "item2.X-ABLABEL:_$!<Work>!$_",
        "PRODID:-//Apple Inc.//iPhone OS 17.4//EN",
    ] {
        assert!(updated.replace("\r\n ", "").contains(kept), "lost {kept}");
    }
    assert!(!updated.contains("zeynep.blog@example.org"));
    assert!(
        !updated.contains("item3.X-ABLABEL"),
        "the removed e-mail's label goes too"
    );
    assert!(!updated.contains("+90 212 999 88 77"));
    assert_eq!(parse_vcard(&updated).expect("card"), contact);

    // Unchanged contacts produce the same card (modulo folding/canonical case).
    let unchanged =
        upsert_contact(original, &parse_vcard(original).expect("card")).expect("upsert");
    assert_eq!(parse_components(&unchanged), parse_components(original));
}

#[test]
fn upsert_upgrades_21_and_keeps_40() {
    let original = text(include_bytes!("fixtures/outlook21.vcf"));
    let mut contact = parse_vcard(original).expect("card");
    contact.title = Some("Geschäftsführer".into());
    let updated = upsert_contact(original, &contact).expect("upsert");
    let cards = parse_components(&updated);
    assert_eq!(cards.len(), 2, "other cards are kept");
    let card = &cards[0];
    assert_eq!(card.value("VERSION"), Some("3.0"));
    assert_eq!(card.value("N"), Some("Müller;Jürgen"));
    assert_eq!(
        card.text("LABEL").as_deref(),
        Some("Friedrichstraße 1\nBerlin 10117\nDeutschland")
    );
    assert_eq!(
        card.value("ADR"),
        Some(";;Friedrichstraße 1;Berlin;;10117;Deutschland")
    );
    let tel = card.property("TEL").expect("tel");
    assert_eq!(tel.param_values("TYPE"), ["WORK", "VOICE"]);
    assert_eq!(
        card.property("PHOTO").and_then(|p| p.param("ENCODING")),
        Some("b")
    );
    assert!(card.property("X-MS-OL-DESIGN").is_some());
    assert!(
        card.properties
            .iter()
            .all(|p| !p.has_param("ENCODING") || p.name == "PHOTO"),
        "QP is decoded"
    );
    assert_eq!(parse_vcards(&updated)[0], contact);
    assert_eq!(
        cards[1].value("VERSION"),
        Some("2.1"),
        "untouched cards keep their version"
    );

    let original = text(include_bytes!("fixtures/nextcloud.vcf"));
    let mut can = parse_vcards(original).remove(1);
    can.emails
        .push(field("can@new.example", Some("other"), false));
    let updated = upsert_contact(original, &can).expect("upsert");
    let card = &parse_components(&updated)[1];
    assert_eq!(card.value("VERSION"), Some("4.0"));
    assert_eq!(
        card.value("BDAY"),
        Some("--0415"),
        "unchanged values are not rewritten"
    );
    let added = card.properties_named("EMAIL").last().expect("email");
    assert_eq!(added.value, "can@new.example");
    assert_eq!(added.param_values("TYPE"), ["other"]);
    assert_eq!(parse_vcards(&updated)[1], can);

    // A 4.0 birthday without year and a preferred phone use 4.0 syntax.
    can.birthday = chrono::NaiveDate::from_ymd_opt(NO_YEAR, 5, 1);
    can.phones[0].value = "+90 532 999 99 99".into();
    let updated = upsert_contact(original, &can).expect("upsert");
    let card = &parse_components(&updated)[1];
    assert_eq!(card.value("BDAY"), Some("--0501"));
    let tel = card.property("TEL").expect("tel");
    assert_eq!(tel.param("PREF"), Some("1"));
}

#[test]
fn errors_and_empty_input() {
    assert_eq!(
        parse_vcard("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap_err(),
        Error::NoVCard
    );
    assert!(matches!(
        upsert_contact("garbage", &Contact::default()),
        Err(Error::NoVCard)
    ));
    let created = upsert_contact(
        "",
        &Contact {
            name: "New".into(),
            ..Contact::default()
        },
    )
    .expect("card");
    assert_eq!(parse_vcard(&created).expect("card").name, "New");
    // Latin-1 bytes without CHARSET still decode.
    let contacts = parse_vcards_bytes(
        b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ren\xe9 Fran\xe7ois\r\nEND:VCARD\r\n",
    );
    assert_eq!(contacts[0].name, "René François");
}

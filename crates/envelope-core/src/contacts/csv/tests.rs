use super::*;

fn kinds(fields: &[ContactField]) -> Vec<(&str, Option<&str>)> {
    fields
        .iter()
        .map(|f| (f.value.as_str(), f.kind.as_deref()))
        .collect()
}

#[test]
fn reads_gmail_exports() {
    let text = "First Name,Middle Name,Last Name,Phonetic First Name,Nickname,Organization Name,Organization Title,Birthday,Notes,Labels,E-mail 1 - Label,E-mail 1 - Value,E-mail 2 - Label,E-mail 2 - Value,Phone 1 - Label,Phone 1 - Value,Address 1 - Label,Address 1 - Formatted,Address 1 - Street,Address 1 - City,Address 1 - Postal Code,Website 1 - Label,Website 1 - Value\r\n\
Ada,,Lovelace,,,Analytical Engines,Analyst,--12-10,\"Met at the \"\"Royal\"\" Society\",* myContacts,* Home,ada@home.example ::: ada@other.example,Work,ada@work.example,Mobile,+44 20 7946 0000,Home,\"12 St James's Square\nLondon SW1Y 4JH\",,,,Profile,https://ada.example\r\n\
,,,,,,,,,* myContacts,,,,,,,,,,,,,\r\n\
Charles,,Babbage,,Charlie,,,1791-12-26,,,,,,,Work,+44 20 7946 0001,Work,,1 Engine Lane,London,EC1A 1AA,,\r\n";
    assert!(looks_like_contacts(text));
    let people = parse(text);
    assert_eq!(people.len(), 2, "{people:#?}");

    let ada = &people[0];
    assert_eq!(ada.name, "Ada Lovelace");
    assert_eq!(ada.organization.as_deref(), Some("Analytical Engines"));
    assert_eq!(ada.title.as_deref(), Some("Analyst"));
    assert_eq!(ada.note.as_deref(), Some("Met at the \"Royal\" Society"));
    assert_eq!(ada.birthday, NaiveDate::from_ymd_opt(NO_YEAR, 12, 10));
    assert_eq!(
        kinds(&ada.emails),
        [
            ("ada@home.example", Some("home")),
            ("ada@other.example", Some("home")),
            ("ada@work.example", Some("work")),
        ]
    );
    assert!(ada.emails[0].preferred);
    assert_eq!(kinds(&ada.phones), [("+44 20 7946 0000", Some("cell"))]);
    assert_eq!(kinds(&ada.urls), [("https://ada.example", Some("other"))]);
    assert_eq!(ada.addresses.len(), 1);
    assert_eq!(ada.addresses[0].kind.as_deref(), Some("home"));
    assert_eq!(
        ada.addresses[0].formatted(),
        "12 St James's Square\nLondon SW1Y 4JH"
    );

    let charles = &people[1];
    assert_eq!(charles.nickname.as_deref(), Some("Charlie"));
    assert_eq!(charles.birthday, NaiveDate::from_ymd_opt(1791, 12, 26));
    assert_eq!(kinds(&charles.phones), [("+44 20 7946 0001", Some("work"))]);
    // The address's parts win over no formatted text.
    assert_eq!(charles.addresses[0].street, "1 Engine Lane");
    assert_eq!(charles.addresses[0].locality, "London");
    assert_eq!(charles.addresses[0].kind.as_deref(), Some("work"));
}

#[test]
fn reads_outlook_exports_in_windows_1252() {
    let mut bytes = b"First Name,Middle Name,Last Name,Title,E-mail Address,E-mail Display Name,E-mail 2 Address,Business Phone,Mobile Phone,Home Fax,Job Title,Company,Business Street,Business City,Business Postal Code,Business Country/Region,Web Page,Birthday,Notes\r\n".to_vec();
    bytes.extend_from_slice(b"Ren\xe9e,,M\xfcller,Ms.,renee@firma.example,Ren\xe9e M\xfcller (renee@firma.example),,+49 30 1234567,+49 170 1234567,+49 30 7654321,Leiterin,Firma GmbH,\"Hauptstra\xdfe 1\r\nAufgang B\",Berlin,10115,Deutschland,www.firma.example,5/17/1990,\x93Wichtig\x94\r\n");
    bytes.extend_from_slice(b"Max,,Mustermann,,max@firma.example,,,,,,,,,,,,,0/0/00,\r\n");
    let text = decode(&bytes);
    let people = parse(&text);
    assert_eq!(people.len(), 2);
    let renee = &people[0];
    assert_eq!(renee.name, "Ren\u{e9}e M\u{fc}ller");
    // "Title" is Outlook's honorific; the job title is "Job Title".
    assert_eq!(renee.title.as_deref(), Some("Leiterin"));
    assert_eq!(renee.organization.as_deref(), Some("Firma GmbH"));
    assert_eq!(kinds(&renee.emails), [("renee@firma.example", None)]);
    assert_eq!(
        kinds(&renee.phones),
        [
            ("+49 30 1234567", Some("work")),
            ("+49 170 1234567", Some("cell")),
            ("+49 30 7654321", Some("fax")),
        ]
    );
    assert_eq!(renee.birthday, NaiveDate::from_ymd_opt(1990, 5, 17));
    assert_eq!(renee.note.as_deref(), Some("\u{201c}Wichtig\u{201d}"));
    let address = &renee.addresses[0];
    assert_eq!(address.kind.as_deref(), Some("work"));
    assert_eq!(address.street, "Hauptstra\u{df}e 1\nAufgang B");
    assert_eq!(address.locality, "Berlin");
    assert_eq!(address.postal_code, "10115");
    assert_eq!(address.country, "Deutschland");
    assert_eq!(kinds(&renee.urls), [("www.firma.example", None)]);
    // Outlook's empty date.
    assert_eq!(people[1].birthday, None);
}

#[test]
fn reads_thunderbird_exports_with_semicolons() {
    let text = "First Name;Last Name;Display Name;Nickname;Primary Email;Secondary Email;Screen Name;Work Phone;Home Phone;Fax Number;Pager Number;Mobile Number;Home Address;Home Address 2;Home City;Home State;Home ZipCode;Home Country;Work Address;Work Address 2;Work City;Work State;Work ZipCode;Work Country;Job Title;Department;Organization;Web Page 1;Web Page 2;Birth Year;Birth Month;Birth Day;Custom 1;Custom 2;Custom 3;Custom 4;Notes\n\
Grace;Hopper;Grace Hopper;Amazing Grace;grace@navy.example;grace@home.example;;;;;;+1 202 555 0100;;;;;;;;;;;;;Rear Admiral;;US Navy;;;1906;12;9;;;;;Wrote the first compiler.\n";
    let people = parse(text);
    assert_eq!(people.len(), 1);
    let grace = &people[0];
    assert_eq!(grace.name, "Grace Hopper");
    assert_eq!(grace.nickname.as_deref(), Some("Amazing Grace"));
    assert_eq!(
        kinds(&grace.emails),
        [("grace@navy.example", None), ("grace@home.example", None)]
    );
    assert_eq!(kinds(&grace.phones), [("+1 202 555 0100", Some("cell"))]);
    assert_eq!(grace.birthday, NaiveDate::from_ymd_opt(1906, 12, 9));
    assert_eq!(grace.title.as_deref(), Some("Rear Admiral"));
    assert_eq!(grace.organization.as_deref(), Some("US Navy"));
    assert_eq!(grace.note.as_deref(), Some("Wrote the first compiler."));
}

#[test]
fn decodes_utf16_with_a_byte_order_mark() {
    let text = "Name\tE-mail Address\nZo\u{eb} \u{5c71}\u{7530}\tzoe@example.com\n";
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let people = parse(&decode(&bytes));
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].name, "Zo\u{eb} \u{5c71}\u{7530}");
    assert_eq!(people[0].emails[0].value, "zoe@example.com");
}

#[test]
fn other_csv_files_are_not_contacts() {
    assert!(!looks_like_contacts(
        "date,amount,payee\n2026-10-01,12.50,Bakery\n"
    ));
    assert!(!looks_like_contacts("just one line of text"));
}

//! Contacts paging, contact delta and people search against a mock Graph
//! server.

mod common;

use common::{fixture, json_body, prefer_contains, setup};
use envelope_graph::DeltaCursor;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn pages_through_contacts() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts"))
        .and(query_param("$top", "500"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "contacts_page1.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts"))
        .and(query_param("$skip", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "contacts_page2.json")),
        )
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let contacts = client.all_contacts().await.expect("contacts");
    let names: Vec<&str> = contacts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Pavel Bansky", "Isaiah Langer", "Lidia Holloway"]
    );
    let pavel = &contacts[0];
    assert_eq!(pavel.uid, "AAMkADh6v5AAAvgTCEAAA=");
    assert_eq!(pavel.given_name.as_deref(), Some("Pavel"));
    assert_eq!(pavel.family_name.as_deref(), Some("Bansky"));
    assert_eq!(pavel.emails[0].value, "pavelb@contoso.com");
    assert!(pavel.emails[0].preferred);
    assert_eq!(pavel.phones[0].value, "+1 732 555 0102");
    assert_eq!(pavel.phones[0].kind.as_deref(), Some("work"));
    assert_eq!(pavel.organization.as_deref(), Some("Contoso"));
    assert_eq!(pavel.note, None, "empty notes are dropped");
}

#[tokio::test]
async fn pages_contacts_explicitly() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts"))
        .and(query_param("$top", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "contacts_page1.json")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts"))
        .and(query_param("$skip", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "contacts_page2.json")),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    let first = client.list_contacts(2, None).await.expect("first");
    assert_eq!(first.items.len(), 2);
    let second = client
        .list_contacts(2, first.next_link.as_deref())
        .await
        .expect("second");
    assert_eq!(second.items.len(), 1);
    assert!(second.next_link.is_none());
}

#[tokio::test]
async fn syncs_a_contact_folder() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contactFolders"))
        .and(query_param("$select", "parentFolderId"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts"))
        .and(query_param("$select", "parentFolderId"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": "AAMkADh6v5AAAvgTCEAAA=", "parentFolderId": "AAMkADContactsRoot="}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/v1.0/me/contactFolders/AAMkADContactsRoot=/contacts/delta",
        ))
        .and(prefer_contains("odata.maxpagesize=100"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "contacts_delta.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let folder = client
        .default_contact_folder_id()
        .await
        .expect("lookup")
        .expect("folder id");
    assert_eq!(folder, "AAMkADContactsRoot=");
    let page = client
        .contacts_delta(&folder, None, Some(100))
        .await
        .expect("delta");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "Pavel Bansky");
    assert_eq!(page.removed_ids, vec!["AAMkADh6v5AAAvgTCHAAA=".to_owned()]);
    assert!(matches!(page.next, DeltaCursor::Done(_)));
}

#[tokio::test]
async fn searches_people_for_autocomplete() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/people"))
        .and(query_param("$search", "\"j\""))
        .and(query_param("$top", "10"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "people_search.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let people = client.search_people("j", 10).await.expect("people");
    assert!(people.len() >= 2);
    let jan = &people[0];
    assert_eq!(jan.name, "Jan Travis");
    assert_eq!(jan.emails[0].value, "JanT@contoso.com");
    assert_eq!(jan.title.as_deref(), Some("VP Sales"));
    assert_eq!(jan.phones[0].kind.as_deref(), Some("work"));
    let jacob = &people[1];
    assert_eq!(jacob.name, "Jacob Cazares (TAILSPIN)");
    assert_eq!(jacob.given_name, None);
    assert_eq!(jacob.emails[0].value, "JacobC@tailspintoys.com");

    // Blank queries never reach the server.
    assert!(
        client
            .search_people("  \"", 10)
            .await
            .expect("blank")
            .is_empty()
    );
}

#[tokio::test]
async fn writes_contacts_and_photos() {
    let (server, client, _) = setup().await;
    let created = json!({
        "id": "AAMkNew=",
        "displayName": "Ada Lovelace",
        "givenName": "Ada",
        "surname": "Lovelace",
        "emailAddresses": [{"name": "Ada Lovelace", "address": "ada@example.com"}],
        "mobilePhone": "+44 20 1234",
        "homeAddress": {"street": "12 St James's Square", "city": "London"}
    });
    Mock::given(method("POST"))
        .and(path("/v1.0/me/contactFolders/AAMkFolder=/contacts"))
        .and(json_body(json!({
            "displayName": "Ada Lovelace",
            "givenName": "Ada",
            "surname": "Lovelace",
            "emailAddresses": [{"address": "ada@example.com", "name": "Ada Lovelace"}],
            "mobilePhone": "+44 20 1234",
            "homeAddress": {"street": "12 St James's Square", "city": "London"}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(created.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut updated = created.clone();
    updated["jobTitle"] = json!("Programmer");
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/contacts/AAMkNew="))
        .and(json_body(
            json!({"jobTitle": "Programmer", "displayName": "Ada Lovelace"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(updated))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1.0/me/contacts/AAMkNew=/photo/$value"))
        .and(header("content-type", "image/png"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts/AAMkNew=/photo/$value"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(vec![0x89, b'P', b'N', b'G'], "image/png"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts/AAMkNone=/photo/$value"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": "ImageNotFound", "message": "The photo wasn't found."}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/contacts/AAMkNew="))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let ada = envelope_ical::Contact {
        name: "Ada Lovelace".into(),
        given_name: Some("Ada".into()),
        family_name: Some("Lovelace".into()),
        emails: vec![envelope_ical::ContactField {
            value: "ada@example.com".into(),
            kind: None,
            preferred: false,
        }],
        phones: vec![envelope_ical::ContactField {
            value: "+44 20 1234".into(),
            kind: Some("cell".into()),
            preferred: false,
        }],
        addresses: vec![envelope_ical::ContactAddress {
            kind: Some("home".into()),
            street: "12 St James's Square".into(),
            locality: "London".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let saved = client
        .create_contact(Some("AAMkFolder="), &ada)
        .await
        .expect("create");
    assert_eq!(saved.uid, "AAMkNew=");
    assert_eq!(saved.phones[0].kind.as_deref(), Some("cell"));

    // Unchanged: no request at all.
    let same = client
        .update_contact("AAMkNew=", &saved, &saved)
        .await
        .expect("no-op");
    assert_eq!(same, saved);
    let mut edited = saved.clone();
    edited.title = Some("Programmer".into());
    let stored = client
        .update_contact("AAMkNew=", &saved, &edited)
        .await
        .expect("update");
    assert_eq!(stored.title.as_deref(), Some("Programmer"));

    client
        .set_contact_photo("AAMkNew=", vec![1u8, 2, 3], "image/png")
        .await
        .expect("photo upload");
    let (bytes, mime) = client
        .contact_photo("AAMkNew=")
        .await
        .expect("photo")
        .expect("has a photo");
    assert_eq!(mime, "image/png");
    assert_eq!(bytes.len(), 4);
    assert!(
        client
            .contact_photo("AAMkNone=")
            .await
            .expect("no photo")
            .is_none()
    );
    client.delete_contact("AAMkNew=").await.expect("delete");
}

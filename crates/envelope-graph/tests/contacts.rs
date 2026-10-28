//! Contacts paging, contact delta and people search against a mock Graph
//! server.

mod common;

use common::{fixture, prefer_contains, setup};
use envelope_graph::DeltaCursor;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
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

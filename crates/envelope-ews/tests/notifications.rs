//! Streaming notifications against a mock server.

mod common;

use common::{bodies, fixture, on, setup, soap_ok};
use envelope_ews::{DistinguishedFolder, NotificationEventType, StreamingMessage};
use wiremock::ResponseTemplate;

#[tokio::test]
async fn subscribes_and_reads_a_notification_stream() {
    let (server, client) = setup().await;
    on(&server, "<m:Subscribe>", soap_ok("subscribe.xml")).await;
    on(
        &server,
        "<m:GetStreamingEvents>",
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/xml; charset=utf-8")
            .set_body_string(fixture("streaming_events.xml")),
    )
    .await;
    on(
        &server,
        "<m:Unsubscribe>",
        soap_ok("delete_item.xml")
            .set_body_string(fixture("delete_item.xml").replace("DeleteItem", "Unsubscribe")),
    )
    .await;

    let subscription = client
        .subscribe_streaming(
            &[
                DistinguishedFolder::Inbox.into(),
                DistinguishedFolder::Calendar.into(),
            ],
            &[
                NotificationEventType::NewMail,
                NotificationEventType::Modified,
            ],
        )
        .await
        .unwrap();
    assert!(subscription.starts_with("JgBk"));

    let mut stream = client
        .get_streaming_events(std::slice::from_ref(&subscription), 30)
        .await
        .unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap(),
        StreamingMessage::KeepAlive
    );
    let StreamingMessage::Notifications(batch) = stream.next().await.unwrap().unwrap() else {
        panic!("expected notifications");
    };
    assert_eq!(batch[0].subscription_id, "sub-1");
    assert_eq!(batch[0].events.len(), 2);
    assert_eq!(
        batch[0].events[0].event_type,
        NotificationEventType::NewMail
    );
    assert_eq!(batch[0].events[1].unread_count, Some(6));
    let folders: Vec<&str> = batch[0].events[0]
        .affected_folders()
        .iter()
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(folders, ["inbox-id"]);
    assert_eq!(
        stream.next().await.unwrap().unwrap(),
        StreamingMessage::Closed
    );
    assert!(stream.next().await.is_none());
    drop(stream);

    client.unsubscribe(&subscription).await.unwrap();

    let bodies = bodies(&server).await;
    assert!(bodies[0].contains(
        "<t:EventType>NewMailEvent</t:EventType><t:EventType>ModifiedEvent</t:EventType>"
    ));
    assert!(bodies[1].contains("<m:ConnectionTimeout>30</m:ConnectionTimeout>"));
    assert!(bodies[2].contains(&format!(
        "<m:SubscriptionId>{subscription}</m:SubscriptionId>"
    )));
}

#[tokio::test]
async fn streaming_needs_exchange_2010_sp1() {
    let (_server, client) = setup().await;
    client.set_version(envelope_ews::ExchangeVersion::Exchange2010);
    assert!(
        client
            .subscribe_streaming(
                &[DistinguishedFolder::Inbox.into()],
                NotificationEventType::ALL
            )
            .await
            .is_err()
    );
    assert!(client.get_streaming_events(&[], 5).await.is_err());
}

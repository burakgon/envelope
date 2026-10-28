//! Change notifications through streaming subscriptions (Exchange 2010 SP1
//! and later).
//!
//! Notifications only say *that* something changed. The intended use is
//! to keep [`EwsClient::sync_folder_items`] as the source of truth and to
//! run it when an event for a folder arrives, falling back to polling at a
//! fixed interval when subscriptions are unavailable or keep failing:
//!
//! ```no_run
//! # async fn run(client: envelope_ews::EwsClient) -> Result<(), envelope_ews::EwsError> {
//! use envelope_ews::{DistinguishedFolder, NotificationEventType, StreamingMessage};
//!
//! let subscription = client
//!     .subscribe_streaming(&[DistinguishedFolder::Inbox.into()], NotificationEventType::ALL)
//!     .await?;
//! let mut events = client.get_streaming_events(&[subscription.clone()], 30).await?;
//! while let Some(message) = events.next().await {
//!     match message? {
//!         StreamingMessage::Notifications(batch) => { /* sync the folders named in `batch` */ }
//!         StreamingMessage::Closed => break, // reconnect with the same subscription
//!         StreamingMessage::KeepAlive => {}
//!         StreamingMessage::SubscriptionErrors(_) => break, // subscribe again
//!     }
//! }
//! # Ok(()) }
//! ```

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseError};
use crate::soap::{self, Parsed};
use crate::transport::LiveResponse;
use crate::types::{FolderId, FolderRef, ItemId, child_datetime, wire_enum};
use crate::xml::{Element, XmlWriter};

/// Largest envelope accepted from a notification stream.
const MAX_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;

wire_enum! {
    /// Kinds of change a subscription reports.
    pub enum NotificationEventType {
        /// An item or folder was copied.
        Copied = "CopiedEvent",
        /// An item or folder was created.
        Created = "CreatedEvent",
        /// An item or folder was deleted.
        Deleted = "DeletedEvent",
        /// An item or folder changed.
        Modified = "ModifiedEvent",
        /// An item or folder was moved.
        Moved = "MovedEvent",
        /// New mail arrived.
        NewMail = "NewMailEvent",
        /// Free/busy information changed (Exchange 2010 SP1 and later).
        FreeBusyChanged = "FreeBusyChangedEvent",
    }
}

/// One event of a notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEvent {
    /// What happened.
    pub event_type: NotificationEventType,
    /// When.
    pub timestamp: Option<DateTime<Utc>>,
    /// The item concerned (item events).
    pub item_id: Option<ItemId>,
    /// The folder concerned (folder events).
    pub folder_id: Option<FolderId>,
    /// The folder holding the item or folder.
    pub parent_folder_id: Option<FolderId>,
    /// Previous id of a moved or copied item.
    pub old_item_id: Option<ItemId>,
    /// Previous id of a moved or copied folder.
    pub old_folder_id: Option<FolderId>,
    /// Previous parent of a moved or copied item or folder.
    pub old_parent_folder_id: Option<FolderId>,
    /// New unread count of a modified folder.
    pub unread_count: Option<u32>,
}

impl NotificationEvent {
    fn from_element(element: &Element) -> Option<Self> {
        let event_type = NotificationEventType::parse(&element.name)?;
        Some(NotificationEvent {
            event_type,
            timestamp: child_datetime(element, "TimeStamp"),
            item_id: element.child("ItemId").and_then(ItemId::from_element),
            folder_id: element.child("FolderId").and_then(FolderId::from_element),
            parent_folder_id: element
                .child("ParentFolderId")
                .and_then(FolderId::from_element),
            old_item_id: element.child("OldItemId").and_then(ItemId::from_element),
            old_folder_id: element
                .child("OldFolderId")
                .and_then(FolderId::from_element),
            old_parent_folder_id: element
                .child("OldParentFolderId")
                .and_then(FolderId::from_element),
            unread_count: element.child_u32("UnreadCount"),
        })
    }

    /// The folders whose contents may have changed.
    pub fn affected_folders(&self) -> Vec<&FolderId> {
        [
            &self.parent_folder_id,
            &self.old_parent_folder_id,
            &self.folder_id,
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// The events of one subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    /// The subscription.
    pub subscription_id: String,
    /// Its events, in order.
    pub events: Vec<NotificationEvent>,
}

/// One message of a notification stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamingMessage {
    /// Events arrived.
    Notifications(Vec<Notification>),
    /// The server is still there; nothing happened.
    KeepAlive,
    /// The server closed the stream (its connection timeout elapsed);
    /// open a new one with the same subscriptions.
    Closed,
    /// Subscriptions that are gone (expired, unknown); subscribe again.
    SubscriptionErrors(Vec<(String, ResponseError)>),
}

pub(crate) fn subscribe_request(
    folders: &[FolderRef],
    events: &[NotificationEventType],
    all_folders: bool,
) -> String {
    let mut w = XmlWriter::new();
    w.start("m:Subscribe", &[]);
    let attrs: &[(&str, &str)] = if all_folders {
        &[("SubscribeToAllFolders", "true")]
    } else {
        &[]
    };
    w.start("m:StreamingSubscriptionRequest", attrs);
    if !all_folders {
        w.start("t:FolderIds", &[]);
        for folder in folders {
            folder.write(&mut w);
        }
        w.end("t:FolderIds");
    }
    w.start("t:EventTypes", &[]);
    for event in events {
        w.text_element("t:EventType", event.as_str());
    }
    w.end("t:EventTypes")
        .end("m:StreamingSubscriptionRequest")
        .end("m:Subscribe");
    w.into_string()
}

pub(crate) fn get_streaming_events_request(
    subscriptions: &[String],
    timeout_minutes: u32,
) -> String {
    let mut w = XmlWriter::new();
    w.start("m:GetStreamingEvents", &[])
        .start("m:SubscriptionIds", &[]);
    for id in subscriptions {
        w.text_element("t:SubscriptionId", id);
    }
    w.end("m:SubscriptionIds")
        .text_element(
            "m:ConnectionTimeout",
            &timeout_minutes.clamp(1, 30).to_string(),
        )
        .end("m:GetStreamingEvents");
    w.into_string()
}

pub(crate) fn unsubscribe_request(subscription: &str) -> String {
    let mut w = XmlWriter::new();
    w.start("m:Unsubscribe", &[])
        .text_element("m:SubscriptionId", subscription)
        .end("m:Unsubscribe");
    w.into_string()
}

/// Interprets one envelope of a notification stream.
pub(crate) fn parse_streaming_envelope(text: &str) -> Result<StreamingMessage, EwsError> {
    let response = match soap::parse_envelope(200, text)? {
        Parsed::Response(response) => response,
        Parsed::Fault(fault) => return Err(fault.into()),
    };
    let mut notifications = Vec::new();
    let mut errors = Vec::new();
    let mut closed = false;
    for message in soap::response_messages(&response.body)? {
        match soap::check(message) {
            Ok(message) => {
                if message
                    .child_text("ConnectionStatus")
                    .is_some_and(|s| s.eq_ignore_ascii_case("Closed"))
                {
                    closed = true;
                }
                if let Some(list) = message.child("Notifications") {
                    for notification in list.children_named("Notification") {
                        notifications.push(Notification {
                            subscription_id: notification
                                .child_text("SubscriptionId")
                                .unwrap_or_default(),
                            events: notification
                                .elements()
                                .filter_map(NotificationEvent::from_element)
                                .collect(),
                        });
                    }
                }
            }
            Err(error) => {
                let ids: Vec<String> = message
                    .child("ErrorSubscriptionIds")
                    .map(|ids| ids.elements().filter_map(Element::trimmed_text).collect())
                    .unwrap_or_default();
                if ids.is_empty() {
                    return Err(error.into());
                }
                errors.extend(ids.into_iter().map(|id| (id, error.clone())));
            }
        }
    }
    Ok(if !errors.is_empty() {
        StreamingMessage::SubscriptionErrors(errors)
    } else if !notifications.is_empty() {
        StreamingMessage::Notifications(notifications)
    } else if closed {
        StreamingMessage::Closed
    } else {
        StreamingMessage::KeepAlive
    })
}

/// Finds the end (exclusive) of the first complete SOAP envelope in
/// `buffer`: the end of its closing `</…Envelope>` tag.
pub(crate) fn envelope_end(buffer: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"Envelope>";
    let mut from = 0;
    while let Some(found) = find(&buffer[from..], NEEDLE) {
        let at = from + found;
        // Walk back over an optional prefix to the `</`.
        let mut start = at;
        while start > 0
            && (buffer[start - 1].is_ascii_alphanumeric()
                || matches!(buffer[start - 1], b'_' | b'-' | b'.' | b':'))
        {
            start -= 1;
        }
        if start >= 2 && &buffer[start - 2..start] == b"</" {
            return Some(at + NEEDLE.len());
        }
        from = at + NEEDLE.len();
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A live notification stream; see [`EwsClient::get_streaming_events`].
pub struct StreamingEvents {
    live: Option<LiveResponse>,
    buffer: BytesMut,
}

impl std::fmt::Debug for StreamingEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingEvents")
            .field("open", &self.live.is_some())
            .field("buffered", &self.buffer.len())
            .finish()
    }
}

impl StreamingEvents {
    /// The next message; `None` once the server ended the stream.
    pub async fn next(&mut self) -> Option<Result<StreamingMessage, EwsError>> {
        loop {
            if let Some(end) = envelope_end(&self.buffer) {
                let envelope: Bytes = self.buffer.split_to(end).freeze();
                let text = String::from_utf8_lossy(&envelope);
                return Some(parse_streaming_envelope(text.trim_start()));
            }
            if self.buffer.len() > MAX_ENVELOPE_BYTES {
                self.live = None;
                return Some(Err(EwsError::InvalidResponse(
                    "notification envelope too large".into(),
                )));
            }
            let live = self.live.as_mut()?;
            match live.response.chunk().await {
                Ok(Some(chunk)) => self.buffer.extend_from_slice(&chunk),
                Ok(None) => {
                    self.live = None;
                    let rest = String::from_utf8_lossy(&self.buffer).trim().to_owned();
                    self.buffer.clear();
                    if rest.is_empty() {
                        return None;
                    }
                    return Some(parse_streaming_envelope(&rest));
                }
                Err(err) => {
                    self.live = None;
                    return Some(Err(EwsError::from_reqwest(err)));
                }
            }
        }
    }
}

impl EwsClient {
    /// Creates a streaming subscription for the given folders and returns
    /// its id. Subscriptions expire some minutes after their last stream
    /// closes.
    pub async fn subscribe_streaming(
        &self,
        folders: &[FolderRef],
        events: &[NotificationEventType],
    ) -> Result<String, EwsError> {
        if folders.is_empty() {
            return Err(EwsError::InvalidInput("no folders to subscribe to".into()));
        }
        self.subscribe(subscribe_request(
            folders,
            &self.supported_events(events)?,
            false,
        ))
        .await
    }

    /// Creates a streaming subscription for every folder of the mailbox.
    pub async fn subscribe_streaming_all_folders(
        &self,
        events: &[NotificationEventType],
    ) -> Result<String, EwsError> {
        self.subscribe(subscribe_request(
            &[],
            &self.supported_events(events)?,
            true,
        ))
        .await
    }

    fn supported_events(
        &self,
        events: &[NotificationEventType],
    ) -> Result<Vec<NotificationEventType>, EwsError> {
        if !self.version().has_2010_sp1_features() {
            return Err(EwsError::InvalidInput(
                "streaming notifications require Exchange 2010 SP1 or later".into(),
            ));
        }
        if events.is_empty() {
            return Err(EwsError::InvalidInput(
                "no event types to subscribe to".into(),
            ));
        }
        Ok(events.to_vec())
    }

    async fn subscribe(&self, body: String) -> Result<String, EwsError> {
        let response = self.call(&SoapRequest::write("Subscribe", body)).await?;
        soap::single(&response)?
            .child_text("SubscriptionId")
            .ok_or_else(|| {
                EwsError::InvalidResponse("Subscribe returned no subscription id".into())
            })
    }

    /// Ends a subscription.
    pub async fn unsubscribe(&self, subscription_id: &str) -> Result<(), EwsError> {
        let body = unsubscribe_request(subscription_id);
        let response = self.call(&SoapRequest::write("Unsubscribe", body)).await?;
        soap::single(&response).map(drop)
    }

    /// Opens a notification stream for up to `timeout_minutes` (1–30)
    /// minutes. With NTLM the stream occupies one of the client's
    /// connections until it is dropped.
    pub async fn get_streaming_events(
        &self,
        subscription_ids: &[String],
        timeout_minutes: u32,
    ) -> Result<StreamingEvents, EwsError> {
        if subscription_ids.is_empty() {
            return Err(EwsError::InvalidInput("no subscriptions to stream".into()));
        }
        let minutes = timeout_minutes.clamp(1, 30);
        let body = get_streaming_events_request(subscription_ids, minutes);
        let request = SoapRequest::read("GetStreamingEvents", body);
        let live = self
            .call_streaming(&request, Duration::from_secs(u64::from(minutes) * 60 + 120))
            .await?;
        Ok(StreamingEvents {
            live: Some(live),
            buffer: BytesMut::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DistinguishedFolder;

    #[test]
    fn requests() {
        assert_eq!(
            subscribe_request(
                &[DistinguishedFolder::Inbox.into()],
                &[
                    NotificationEventType::NewMail,
                    NotificationEventType::Modified
                ],
                false
            ),
            r#"<m:Subscribe><m:StreamingSubscriptionRequest><t:FolderIds><t:DistinguishedFolderId Id="inbox"/></t:FolderIds><t:EventTypes><t:EventType>NewMailEvent</t:EventType><t:EventType>ModifiedEvent</t:EventType></t:EventTypes></m:StreamingSubscriptionRequest></m:Subscribe>"#
        );
        assert!(subscribe_request(&[], &[NotificationEventType::Created], true)
            .starts_with(r#"<m:Subscribe><m:StreamingSubscriptionRequest SubscribeToAllFolders="true"><t:EventTypes>"#));
        assert_eq!(
            get_streaming_events_request(&["sub-1".into()], 99),
            "<m:GetStreamingEvents><m:SubscriptionIds><t:SubscriptionId>sub-1</t:SubscriptionId></m:SubscriptionIds><m:ConnectionTimeout>30</m:ConnectionTimeout></m:GetStreamingEvents>"
        );
        assert_eq!(
            unsubscribe_request("sub-1"),
            "<m:Unsubscribe><m:SubscriptionId>sub-1</m:SubscriptionId></m:Unsubscribe>"
        );
    }

    const EVENTS: &str = r#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/"><soap11:Header xmlns:soap11="http://schemas.xmlsoap.org/soap/envelope/"><ServerVersionInfo xmlns="http://schemas.microsoft.com/exchange/services/2006/types" MajorVersion="15" MinorVersion="2" MajorBuildNumber="1544" MinorBuildNumber="4"/></soap11:Header><soap11:Body xmlns:soap11="http://schemas.xmlsoap.org/soap/envelope/"><m:GetStreamingEventsResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:GetStreamingEventsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Notifications><m:Notification><t:SubscriptionId>sub-1</t:SubscriptionId><t:NewMailEvent><t:TimeStamp>2024-05-01T08:00:00Z</t:TimeStamp><t:ItemId Id="item-1" ChangeKey="ck"/><t:ParentFolderId Id="inbox-id" ChangeKey="f"/></t:NewMailEvent><t:ModifiedEvent><t:TimeStamp>2024-05-01T08:00:01Z</t:TimeStamp><t:FolderId Id="inbox-id"/><t:ParentFolderId Id="root"/><t:UnreadCount>4</t:UnreadCount></t:ModifiedEvent><t:MovedEvent><t:TimeStamp>2024-05-01T08:00:02Z</t:TimeStamp><t:ItemId Id="item-2"/><t:ParentFolderId Id="archive"/><t:OldItemId Id="item-2-old"/><t:OldParentFolderId Id="inbox-id"/></t:MovedEvent></m:Notification></m:Notifications></m:GetStreamingEventsResponseMessage></m:ResponseMessages></m:GetStreamingEventsResponse></soap11:Body></Envelope>"#;

    const CLOSED: &str = r#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/"><soap11:Body xmlns:soap11="http://schemas.xmlsoap.org/soap/envelope/"><m:GetStreamingEventsResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages><m:GetStreamingEventsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:ConnectionStatus>Closed</m:ConnectionStatus></m:GetStreamingEventsResponseMessage></m:ResponseMessages></m:GetStreamingEventsResponse></soap11:Body></Envelope>"#;

    #[test]
    fn parses_notifications() {
        let StreamingMessage::Notifications(batch) = parse_streaming_envelope(EVENTS).unwrap()
        else {
            panic!("expected notifications");
        };
        assert_eq!(batch.len(), 1);
        let events = &batch[0].events;
        assert_eq!(batch[0].subscription_id, "sub-1");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, NotificationEventType::NewMail);
        assert_eq!(
            events[0].item_id.as_ref().map(|i| i.id.as_str()),
            Some("item-1")
        );
        assert_eq!(events[1].unread_count, Some(4));
        assert_eq!(
            events[1].folder_id.as_ref().map(|f| f.id.as_str()),
            Some("inbox-id")
        );
        let moved = &events[2];
        assert_eq!(
            moved.old_item_id.as_ref().map(|i| i.id.as_str()),
            Some("item-2-old")
        );
        let affected: Vec<&str> = moved
            .affected_folders()
            .iter()
            .map(|f| f.id.as_str())
            .collect();
        assert_eq!(affected, ["archive", "inbox-id"]);
        assert_eq!(
            parse_streaming_envelope(CLOSED).unwrap(),
            StreamingMessage::Closed
        );
    }

    #[test]
    fn parses_subscription_errors() {
        let text = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:GetStreamingEventsResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:GetStreamingEventsResponseMessage ResponseClass="Error"><m:MessageText>The subscription is invalid.</m:MessageText><m:ResponseCode>ErrorInvalidSubscription</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey><m:ErrorSubscriptionIds><m:SubscriptionId>sub-9</m:SubscriptionId></m:ErrorSubscriptionIds></m:GetStreamingEventsResponseMessage></m:ResponseMessages></m:GetStreamingEventsResponse></s:Body></s:Envelope>"#;
        let StreamingMessage::SubscriptionErrors(errors) = parse_streaming_envelope(text).unwrap()
        else {
            panic!("expected subscription errors");
        };
        assert_eq!(errors[0].0, "sub-9");
        assert!(errors[0].1.code.is_subscription_gone());
    }

    #[test]
    fn finds_envelope_boundaries() {
        let stream = format!("{EVENTS}{CLOSED}");
        let first = envelope_end(stream.as_bytes()).unwrap();
        assert_eq!(first, EVENTS.len());
        assert_eq!(
            envelope_end(&stream.as_bytes()[first..]),
            Some(CLOSED.len())
        );
        assert_eq!(envelope_end(b"<s:Envelope xmlns:s=\"x\"><s:Body>"), None);
        assert_eq!(envelope_end(b"<Envelope></Envelope>"), Some(21));
        assert_eq!(envelope_end(b"<a:Envelope></a:Envelope>  "), Some(25));
    }
}

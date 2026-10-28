//! Mail: listing and searching (`FindItem`), fetching with MIME content
//! (`GetItem`), read state / flags / categories (`UpdateItem`), sending and
//! saving MIME messages (`CreateItem`, `SendItem`), attachments and
//! `MarkAllItemsAsRead`.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseError};
use crate::items::{
    FieldUpdate, FindItemQuery, GET_ITEM_BATCH_SIZE, GET_ITEM_MIME_BATCH_SIZE, ItemPage, ItemView,
    UpdateAttributes, first_item_id, restriction_since, value_xml,
};
use crate::soap;
use crate::types::{
    Body, BodyType, DistinguishedFolder, FolderId, FolderRef, Importance, ItemId, ItemKind,
    ItemRef, ItemShape, Mailbox, PropertyPath, child_datetime, extended_tag_value, string_list,
    wire_enum,
};
use crate::version::ExchangeVersion;
use crate::xml::{Element, XmlWriter};

/// `PR_FLAG_STATUS` (`0` none, `1` complete, `2` flagged).
const PR_FLAG_STATUS: u16 = 0x1090;
/// `PR_FOLLOWUP_ICON` (`6` = red flag).
const PR_FOLLOWUP_ICON: u16 = 0x1095;
/// `PR_MESSAGE_FLAGS`; `MSGFLAG_READ | MSGFLAG_UNSENT` marks a draft.
const PR_MESSAGE_FLAGS: u16 = 0x0E07;
const MSGFLAG_READ: u32 = 0x1;
const MSGFLAG_UNSENT: u32 = 0x8;

/// Largest page `FindItem` serves by default (`EWSFindCountLimit`).
pub const MAX_FIND_PAGE_SIZE: u32 = 1000;

wire_enum! {
    /// Follow-up flag state.
    pub enum FlagStatus {
        /// Not flagged.
        NotFlagged = "NotFlagged",
        /// Flagged for follow-up.
        Flagged = "Flagged",
        /// Follow-up completed.
        Complete = "Complete",
    }
}

/// A follow-up flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flag {
    /// The state.
    pub status: FlagStatus,
    /// Start of the follow-up period.
    pub start_date: Option<DateTime<Utc>>,
    /// Due date.
    pub due_date: Option<DateTime<Utc>>,
    /// When the follow-up was completed.
    pub complete_date: Option<DateTime<Utc>>,
}

impl Flag {
    fn from_item(item: &Element) -> Option<Flag> {
        if let Some(flag) = item.child("Flag") {
            let status = flag
                .child_text("FlagStatus")
                .and_then(|s| FlagStatus::parse(&s))
                .unwrap_or(FlagStatus::NotFlagged);
            return Some(Flag {
                status,
                start_date: child_datetime(flag, "StartDate"),
                due_date: child_datetime(flag, "DueDate"),
                complete_date: child_datetime(flag, "CompleteDate"),
            });
        }
        let raw = extended_tag_value(item, PR_FLAG_STATUS)?;
        let status = match raw.trim() {
            "2" => FlagStatus::Flagged,
            "1" => FlagStatus::Complete,
            _ => FlagStatus::NotFlagged,
        };
        Some(Flag {
            status,
            start_date: None,
            due_date: None,
            complete_date: None,
        })
    }
}

wire_enum! {
    /// Kind of attachment.
    pub enum AttachmentKind {
        /// A file.
        File = "FileAttachment",
        /// An embedded item (an attached message, event, ...).
        Item = "ItemAttachment",
        /// A link to a cloud file (Exchange Online).
        Reference = "ReferenceAttachment",
    }
}

/// Metadata of an attachment, as listed on its item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInfo {
    /// Attachment id for [`EwsClient::get_attachment`].
    pub id: String,
    /// What it is.
    pub kind: AttachmentKind,
    /// File name.
    pub name: Option<String>,
    /// MIME type.
    pub content_type: Option<String>,
    /// `Content-ID` of inline parts.
    pub content_id: Option<String>,
    /// Size in bytes.
    pub size: Option<u64>,
    /// Whether the attachment is shown inline in the body.
    pub is_inline: bool,
    /// Last modification.
    pub last_modified: Option<DateTime<Utc>>,
}

impl AttachmentInfo {
    pub(crate) fn from_element(element: &Element) -> Option<Self> {
        let kind = AttachmentKind::parse(&element.name)?;
        let id = element.child("AttachmentId")?.attr("Id")?.to_owned();
        Some(AttachmentInfo {
            id,
            kind,
            name: element.child_text("Name"),
            content_type: element.child_text("ContentType"),
            content_id: element.child_text("ContentId"),
            size: element.child_u64("Size"),
            is_inline: element.child_bool("IsInline").unwrap_or(false),
            last_modified: child_datetime(element, "LastModifiedTime"),
        })
    }
}

/// An attachment with its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// Metadata.
    pub info: AttachmentInfo,
    /// The content of a file attachment.
    pub content: Option<Vec<u8>>,
    /// The MIME content of an item attachment (an attached message as
    /// `message/rfc822`, an attached event as iCalendar).
    pub mime_content: Option<Vec<u8>>,
}

/// A mail item (also used for the base properties of any item reported
/// by [`EwsClient::sync_folder_items`]).
///
/// Every property is optional because what is filled depends on the
/// requested shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// Item id and change key.
    pub id: ItemId,
    /// The item type.
    pub kind: ItemKind,
    /// The folder holding the item.
    pub parent_folder_id: Option<FolderId>,
    /// `IPM.Note`, `IPM.Schedule.Meeting.Request`, `REPORT.IPM.Note.NDR`, ...
    pub item_class: Option<String>,
    /// Subject.
    pub subject: Option<String>,
    /// The `From` address.
    pub from: Option<Mailbox>,
    /// The `Sender` address (differs from `from` for delegates).
    pub sender: Option<Mailbox>,
    /// `To` recipients.
    pub to: Vec<Mailbox>,
    /// `Cc` recipients.
    pub cc: Vec<Mailbox>,
    /// `Bcc` recipients (drafts and sent items).
    pub bcc: Vec<Mailbox>,
    /// `Reply-To` addresses.
    pub reply_to: Vec<Mailbox>,
    /// Display string of the `To` recipients (available in list views).
    pub display_to: Option<String>,
    /// Display string of the `Cc` recipients.
    pub display_cc: Option<String>,
    /// When the item was received.
    pub received: Option<DateTime<Utc>>,
    /// When the message was sent.
    pub sent: Option<DateTime<Utc>>,
    /// When the item was created in the mailbox.
    pub created: Option<DateTime<Utc>>,
    /// Last modification.
    pub last_modified: Option<DateTime<Utc>>,
    /// Read state.
    pub is_read: Option<bool>,
    /// Whether the item is an unsent draft.
    pub is_draft: Option<bool>,
    /// Follow-up flag.
    pub flag: Option<Flag>,
    /// Categories (Outlook color categories by name).
    pub categories: Vec<String>,
    /// Importance.
    pub importance: Option<Importance>,
    /// `Message-ID` header.
    pub internet_message_id: Option<String>,
    /// `In-Reply-To` header.
    pub in_reply_to: Option<String>,
    /// `References` header.
    pub references: Option<String>,
    /// Exchange conversation id.
    pub conversation_id: Option<String>,
    /// Conversation topic (subject without prefixes).
    pub conversation_topic: Option<String>,
    /// Whether the item has (non-inline) attachments.
    pub has_attachments: Option<bool>,
    /// Attachment metadata (when requested).
    pub attachments: Vec<AttachmentInfo>,
    /// Size in bytes.
    pub size: Option<u64>,
    /// Plain-text preview of the body (Exchange 2013 and later).
    pub preview: Option<String>,
    /// The body (when requested).
    pub body: Option<Body>,
    /// Full MIME content (when requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_content: Option<Vec<u8>>,
    /// For meeting messages: the calendar item they belong to.
    pub associated_calendar_item_id: Option<ItemId>,
}

impl Message {
    /// Parses an item element of any type.
    pub(crate) fn from_element(element: &Element) -> Option<Message> {
        let kind = ItemKind::parse(&element.name)?;
        let id = element.child("ItemId").and_then(ItemId::from_element)?;
        Some(Message {
            id,
            kind,
            parent_folder_id: element
                .child("ParentFolderId")
                .and_then(FolderId::from_element),
            item_class: element.child_text("ItemClass"),
            subject: element.child_raw_text("Subject"),
            from: element.child("From").and_then(Mailbox::from_single),
            sender: element.child("Sender").and_then(Mailbox::from_single),
            to: Mailbox::list(element.child("ToRecipients")),
            cc: Mailbox::list(element.child("CcRecipients")),
            bcc: Mailbox::list(element.child("BccRecipients")),
            reply_to: Mailbox::list(element.child("ReplyTo")),
            display_to: element.child_text("DisplayTo"),
            display_cc: element.child_text("DisplayCc"),
            received: child_datetime(element, "DateTimeReceived"),
            sent: child_datetime(element, "DateTimeSent"),
            created: child_datetime(element, "DateTimeCreated"),
            last_modified: child_datetime(element, "LastModifiedTime"),
            is_read: element.child_bool("IsRead"),
            is_draft: element.child_bool("IsDraft"),
            flag: Flag::from_item(element),
            categories: string_list(element.child("Categories")),
            importance: element
                .child_text("Importance")
                .and_then(|i| Importance::parse(&i)),
            internet_message_id: element.child_text("InternetMessageId"),
            in_reply_to: element.child_text("InReplyTo"),
            references: element.child_text("References"),
            conversation_id: element
                .child("ConversationId")
                .and_then(|c| c.attr("Id"))
                .map(str::to_owned),
            conversation_topic: element.child_raw_text("ConversationTopic"),
            has_attachments: element.child_bool("HasAttachments"),
            attachments: element
                .child("Attachments")
                .map(|list| {
                    list.elements()
                        .filter_map(AttachmentInfo::from_element)
                        .collect()
                })
                .unwrap_or_default(),
            size: element.child_u64("Size"),
            preview: element.child_raw_text("Preview"),
            body: element.child("Body").map(Body::from_element),
            mime_content: element
                .child("MimeContent")
                .and_then(|m| decode_base64(&m.text()).ok()),
            associated_calendar_item_id: element
                .child("AssociatedCalendarItemId")
                .and_then(ItemId::from_element),
        })
    }

    /// True when the follow-up flag is set (not completed).
    pub fn is_flagged(&self) -> bool {
        self.flag
            .as_ref()
            .is_some_and(|f| f.status == FlagStatus::Flagged)
    }
}

/// Decodes base64 content, ignoring line breaks and other whitespace.
pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, EwsError> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|e| EwsError::InvalidResponse(format!("invalid base64 content: {e}")))
}

fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Properties for list views, all of which `FindItem` can return.
pub fn message_summary_properties(version: ExchangeVersion) -> Vec<PropertyPath> {
    let mut properties: Vec<PropertyPath> = [
        "item:ItemClass",
        "item:Subject",
        "item:DateTimeReceived",
        "item:DateTimeSent",
        "item:Size",
        "item:Importance",
        "item:HasAttachments",
        "item:Categories",
        "item:DisplayTo",
        "item:DisplayCc",
        "message:From",
        "message:Sender",
        "message:IsRead",
        "message:InternetMessageId",
    ]
    .into_iter()
    .map(PropertyPath::field)
    .collect();
    if version.has_2010_features() {
        properties.push(PropertyPath::field("item:ConversationId"));
    }
    properties.push(PropertyPath::extended_tag(PR_FLAG_STATUS, "Integer"));
    properties
}

/// Properties fetched by [`EwsClient::get_messages`]: everything a client
/// shows for a message, except the body.
pub fn message_detail_properties(version: ExchangeVersion) -> Vec<PropertyPath> {
    let mut properties = message_summary_properties(version);
    properties.extend(
        [
            "item:ParentFolderId",
            "item:DateTimeCreated",
            "item:IsDraft",
            "item:InReplyTo",
            "item:Attachments",
            "message:ToRecipients",
            "message:CcRecipients",
            "message:BccRecipients",
            "message:ReplyTo",
            "message:References",
            "message:ConversationTopic",
        ]
        .into_iter()
        .map(PropertyPath::field),
    );
    if version.has_2010_features() {
        properties.push(PropertyPath::field("item:LastModifiedTime"));
    }
    if version.has_2013_features() {
        properties.push(PropertyPath::field("item:Preview"));
        properties.push(PropertyPath::field("item:Flag"));
    }
    properties
}

/// Options for [`EwsClient::find_messages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMessagesOptions {
    /// Offset of the page (from [`ItemPage::next_offset`]).
    pub offset: u32,
    /// Items per page (at most [`MAX_FIND_PAGE_SIZE`]).
    pub page_size: u32,
    /// Newest first (by receive time); oldest first otherwise.
    pub newest_first: bool,
    /// Only items received at or after this instant.
    pub received_since: Option<DateTime<Utc>>,
    /// Properties to return; `None` uses [`message_summary_properties`].
    pub properties: Option<Vec<PropertyPath>>,
}

impl Default for FindMessagesOptions {
    fn default() -> Self {
        FindMessagesOptions {
            offset: 0,
            page_size: 50,
            newest_first: true,
            received_since: None,
            properties: None,
        }
    }
}

/// Options for [`EwsClient::get_messages`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GetMessagesOptions {
    /// Fetch the full MIME content (base64-decoded into
    /// [`Message::mime_content`]).
    pub include_mime_content: bool,
    /// Fetch the body in this format.
    pub body_type: Option<BodyType>,
    /// Further properties beyond [`message_detail_properties`].
    pub extra_properties: Vec<PropertyPath>,
}

/// Changes applied by [`EwsClient::update_messages`]; `None` leaves a
/// property alone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageUpdate {
    /// New read state.
    pub is_read: Option<bool>,
    /// New flag state.
    pub flag: Option<FlagStatus>,
    /// New categories; an empty list removes all.
    pub categories: Option<Vec<String>>,
}

impl MessageUpdate {
    fn is_empty(&self) -> bool {
        self.is_read.is_none() && self.flag.is_none() && self.categories.is_none()
    }

    fn field_updates(&self, version: ExchangeVersion, now: DateTime<Utc>) -> Vec<FieldUpdate> {
        let mut updates = Vec::new();
        if let Some(is_read) = self.is_read {
            updates.push(FieldUpdate::set(
                PropertyPath::field("message:IsRead"),
                value_xml("Message", "IsRead", if is_read { "true" } else { "false" }),
            ));
        }
        if let Some(status) = self.flag {
            if version.has_2013_features() {
                let mut w = XmlWriter::new();
                w.start("t:Item", &[])
                    .start("t:Flag", &[])
                    .text_element("t:FlagStatus", status.as_str());
                if status == FlagStatus::Complete {
                    w.text_element("t:CompleteDate", &crate::types::format_datetime(now));
                }
                w.end("t:Flag").end("t:Item");
                updates.push(FieldUpdate::set(
                    PropertyPath::field("item:Flag"),
                    w.into_string(),
                ));
            } else {
                let status_path = PropertyPath::extended_tag(PR_FLAG_STATUS, "Integer");
                let icon_path = PropertyPath::extended_tag(PR_FOLLOWUP_ICON, "Integer");
                match status {
                    FlagStatus::NotFlagged => {
                        updates.push(FieldUpdate::Delete(status_path));
                        updates.push(FieldUpdate::Delete(icon_path));
                    }
                    FlagStatus::Flagged => {
                        updates.push(FieldUpdate::set(
                            status_path.clone(),
                            extended_value_xml(&status_path, "2"),
                        ));
                        updates.push(FieldUpdate::set(
                            icon_path.clone(),
                            extended_value_xml(&icon_path, "6"),
                        ));
                    }
                    FlagStatus::Complete => {
                        updates.push(FieldUpdate::set(
                            status_path.clone(),
                            extended_value_xml(&status_path, "1"),
                        ));
                        updates.push(FieldUpdate::Delete(icon_path));
                    }
                }
            }
        }
        if let Some(categories) = &self.categories {
            let path = PropertyPath::field("item:Categories");
            if categories.is_empty() {
                updates.push(FieldUpdate::Delete(path));
            } else {
                let mut w = XmlWriter::new();
                w.start("t:Item", &[]).start("t:Categories", &[]);
                for category in categories {
                    w.text_element("t:String", category);
                }
                w.end("t:Categories").end("t:Item");
                updates.push(FieldUpdate::set(path, w.into_string()));
            }
        }
        updates
    }
}

/// `<t:Item><t:ExtendedProperty>…<t:Value>v</t:Value></t:ExtendedProperty></t:Item>`
fn extended_value_xml(path: &PropertyPath, value: &str) -> String {
    let mut w = XmlWriter::new();
    w.start("t:Item", &[]).start("t:ExtendedProperty", &[]);
    path.write(&mut w);
    w.text_element("t:Value", value)
        .end("t:ExtendedProperty")
        .end("t:Item");
    w.into_string()
}

/// What happens to a MIME message handed to [`EwsClient::create_mime_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimeDisposition {
    /// Send it, keeping a copy in the given folder (normally sent items).
    SendAndSaveCopy(FolderRef),
    /// Send it without keeping a copy.
    SendOnly,
    /// Store it as an unsent draft in the given folder (normally drafts).
    SaveDraft(FolderRef),
    /// Store it as a received or sent message in the given folder
    /// (importing, or saving a copy of a message sent over SMTP).
    Import {
        /// Target folder.
        folder: FolderRef,
        /// Whether to mark it read.
        is_read: bool,
    },
}

pub(crate) fn create_mime_request(mime: &[u8], disposition: &MimeDisposition) -> String {
    let (message_disposition, folder) = match disposition {
        MimeDisposition::SendAndSaveCopy(folder) => ("SendAndSaveCopy", Some(folder)),
        MimeDisposition::SendOnly => ("SendOnly", None),
        MimeDisposition::SaveDraft(folder) => ("SaveOnly", Some(folder)),
        MimeDisposition::Import { folder, .. } => ("SaveOnly", Some(folder)),
    };
    let mut w = XmlWriter::new();
    w.start(
        "m:CreateItem",
        &[("MessageDisposition", message_disposition)],
    );
    if let Some(folder) = folder {
        w.start("m:SavedItemFolderId", &[]);
        folder.write(&mut w);
        w.end("m:SavedItemFolderId");
    }
    w.start("m:Items", &[]).start("t:Message", &[]);
    w.start("t:MimeContent", &[("CharacterSet", "UTF-8")])
        .raw(&encode_base64(mime))
        .end("t:MimeContent");
    match disposition {
        MimeDisposition::SaveDraft(_) => {
            let flags = (MSGFLAG_READ | MSGFLAG_UNSENT).to_string();
            w.start("t:ExtendedProperty", &[]);
            PropertyPath::extended_tag(PR_MESSAGE_FLAGS, "Integer").write(&mut w);
            w.text_element("t:Value", &flags).end("t:ExtendedProperty");
        }
        MimeDisposition::Import { is_read, .. } => {
            w.text_element("t:IsRead", if *is_read { "true" } else { "false" });
        }
        MimeDisposition::SendAndSaveCopy(_) | MimeDisposition::SendOnly => {}
    }
    w.end("t:Message").end("m:Items").end("m:CreateItem");
    w.into_string()
}

pub(crate) fn send_item_request(ids: &[ItemId], save_to: Option<&FolderRef>) -> String {
    let mut w = XmlWriter::new();
    w.start(
        "m:SendItem",
        &[(
            "SaveItemToFolder",
            if save_to.is_some() { "true" } else { "false" },
        )],
    )
    .start("m:ItemIds", &[]);
    for id in ids {
        id.write(&mut w, "t:ItemId");
    }
    w.end("m:ItemIds");
    if let Some(folder) = save_to {
        w.start("m:SavedItemFolderId", &[]);
        folder.write(&mut w);
        w.end("m:SavedItemFolderId");
    }
    w.end("m:SendItem");
    w.into_string()
}

pub(crate) fn get_attachment_request(ids: &[&str], include_mime: bool) -> String {
    let mut w = XmlWriter::new();
    w.start("m:GetAttachment", &[]);
    if include_mime {
        w.start("m:AttachmentShape", &[])
            .text_element("t:IncludeMimeContent", "true")
            .end("m:AttachmentShape");
    }
    w.start("m:AttachmentIds", &[]);
    for id in ids {
        w.empty("t:AttachmentId", &[("Id", id)]);
    }
    w.end("m:AttachmentIds").end("m:GetAttachment");
    w.into_string()
}

pub(crate) fn parse_attachment(element: &Element) -> Option<Attachment> {
    let info = AttachmentInfo::from_element(element)?;
    let content = element
        .child("Content")
        .and_then(|c| decode_base64(&c.text()).ok());
    let mime_content = element
        .elements()
        .find_map(|child| child.child("MimeContent"))
        .and_then(|m| decode_base64(&m.text()).ok());
    Some(Attachment {
        info,
        content,
        mime_content,
    })
}

pub(crate) fn mark_all_read_request(
    folders: &[FolderRef],
    read: bool,
    suppress_receipts: bool,
) -> String {
    let mut w = XmlWriter::new();
    w.start("m:MarkAllItemsAsRead", &[])
        .text_element("m:ReadFlag", if read { "true" } else { "false" })
        .text_element(
            "m:SuppressReadReceipts",
            if suppress_receipts { "true" } else { "false" },
        )
        .start("m:FolderIds", &[]);
    for folder in folders {
        folder.write(&mut w);
    }
    w.end("m:FolderIds").end("m:MarkAllItemsAsRead");
    w.into_string()
}

impl EwsClient {
    /// One page of messages of a folder, newest first by default: the
    /// initial window of a folder before switching to
    /// [`sync_folder_items`](Self::sync_folder_items).
    pub async fn find_messages(
        &self,
        folder: impl Into<FolderRef>,
        options: &FindMessagesOptions,
    ) -> Result<ItemPage<Message>, EwsError> {
        let folders = [folder.into()];
        let shape = ItemShape::with_properties(
            options
                .properties
                .clone()
                .unwrap_or_else(|| message_summary_properties(self.version())),
        );
        let query = FindItemQuery {
            shape: &shape,
            view: ItemView::Indexed {
                offset: options.offset,
                max: options.page_size.clamp(1, MAX_FIND_PAGE_SIZE),
            },
            restriction: options
                .received_since
                .map(|since| restriction_since("item:DateTimeReceived", since)),
            sort: Some(("item:DateTimeReceived", options.newest_first)),
            folders: &folders,
            query: None,
        };
        self.find_items_with(&query, Message::from_element).await
    }

    /// Server-side search with an Advanced Query Syntax string
    /// (`from:alice subject:report`, or plain words) in one or more
    /// folders, newest first. Requires Exchange 2010 or later; relies on
    /// the server's content index.
    pub async fn search_messages(
        &self,
        folders: &[FolderRef],
        query: &str,
        offset: u32,
        page_size: u32,
    ) -> Result<ItemPage<Message>, EwsError> {
        if !self.version().has_2010_features() {
            return Err(EwsError::InvalidInput(
                "search with QueryString requires Exchange 2010 or later".into(),
            ));
        }
        if folders.is_empty() || query.trim().is_empty() {
            return Err(EwsError::InvalidInput(
                "search needs at least one folder and a query".into(),
            ));
        }
        let shape = ItemShape::with_properties(message_summary_properties(self.version()));
        let request = FindItemQuery {
            shape: &shape,
            view: ItemView::Indexed {
                offset,
                max: page_size.clamp(1, MAX_FIND_PAGE_SIZE),
            },
            restriction: None,
            sort: Some(("item:DateTimeReceived", true)),
            folders,
            query: Some(query.trim()),
        };
        self.find_items_with(&request, Message::from_element).await
    }

    /// Fetches messages by id in batches with [`message_detail_properties`]
    /// (and optionally MIME content and body). One result per id, in order;
    /// deleted messages report `ErrorItemNotFound`.
    pub async fn get_messages(
        &self,
        ids: &[ItemId],
        options: &GetMessagesOptions,
    ) -> Result<Vec<Result<Message, ResponseError>>, EwsError> {
        let mut properties = message_detail_properties(self.version());
        properties.extend(options.extra_properties.iter().cloned());
        let shape = ItemShape {
            base: crate::types::BaseShape::IdOnly,
            include_mime_content: options.include_mime_content,
            body_type: options.body_type,
            additional: properties,
        };
        let batch = if options.include_mime_content {
            GET_ITEM_MIME_BATCH_SIZE
        } else {
            GET_ITEM_BATCH_SIZE
        };
        let refs: Vec<ItemRef> = ids.iter().map(ItemRef::from).collect();
        self.get_items_with(&refs, &shape, batch, Message::from_element)
            .await
    }

    /// One message.
    pub async fn get_message(
        &self,
        id: &ItemId,
        options: &GetMessagesOptions,
    ) -> Result<Message, EwsError> {
        self.get_messages(std::slice::from_ref(id), options)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("GetItem returned nothing".into()))?
            .map_err(EwsError::from)
    }

    /// The raw MIME content (RFC 5322) of a message.
    pub async fn get_mime_content(&self, id: &ItemId) -> Result<Vec<u8>, EwsError> {
        let shape = ItemShape {
            include_mime_content: true,
            ..ItemShape::id_only()
        };
        let mut results = self
            .get_items_with(&[ItemRef::from(id)], &shape, 1, |item| {
                item.child("MimeContent")
                    .and_then(|m| decode_base64(&m.text()).ok())
            })
            .await?;
        results
            .pop()
            .ok_or_else(|| EwsError::InvalidResponse("GetItem returned nothing".into()))?
            .map_err(EwsError::from)
    }

    /// Applies read state, flag and category changes to many messages in
    /// batched `UpdateItem` requests (`AlwaysOverwrite`; read receipts are
    /// suppressed on Exchange 2013 and later). Returns each item's new id.
    pub async fn update_messages(
        &self,
        updates: &[(ItemId, MessageUpdate)],
    ) -> Result<Vec<Result<Option<ItemId>, ResponseError>>, EwsError> {
        if updates.iter().any(|(_, update)| update.is_empty()) {
            return Err(EwsError::InvalidInput("empty message update".into()));
        }
        let version = self.version();
        let now = Utc::now();
        let changes: Vec<(ItemRef, Vec<FieldUpdate>)> = updates
            .iter()
            .map(|(id, update)| (ItemRef::from(id), update.field_updates(version, now)))
            .collect();
        let suppress = version.has_2013_features()
            && updates.iter().any(|(_, update)| update.is_read.is_some());
        self.update_items(
            &changes,
            &UpdateAttributes {
                message_disposition: Some("SaveOnly"),
                send_invitations: None,
                suppress_read_receipts: suppress,
            },
        )
        .await
    }

    /// Applies one update to one message and returns its new id.
    pub async fn update_message(
        &self,
        id: &ItemId,
        update: &MessageUpdate,
    ) -> Result<ItemId, EwsError> {
        let result = self
            .update_messages(&[(id.clone(), update.clone())])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("UpdateItem returned nothing".into()))?
            .map_err(EwsError::from)?;
        Ok(result.unwrap_or_else(|| ItemId::new(id.id.clone())))
    }

    /// Marks messages read or unread.
    pub async fn set_read(
        &self,
        ids: &[ItemId],
        is_read: bool,
    ) -> Result<Vec<Result<Option<ItemId>, ResponseError>>, EwsError> {
        let update = MessageUpdate {
            is_read: Some(is_read),
            ..MessageUpdate::default()
        };
        let updates: Vec<(ItemId, MessageUpdate)> =
            ids.iter().map(|id| (id.clone(), update.clone())).collect();
        self.update_messages(&updates).await
    }

    /// Hands a MIME message (RFC 5322 bytes) to Exchange: send it, save it as
    /// a draft or import it into a folder. Returns the new item's id when
    /// one is created in a folder (`None` when sending).
    pub async fn create_mime_message(
        &self,
        mime: &[u8],
        disposition: &MimeDisposition,
    ) -> Result<Option<ItemId>, EwsError> {
        if mime.is_empty() {
            return Err(EwsError::InvalidInput("empty MIME message".into()));
        }
        let body = create_mime_request(mime, disposition);
        let response = self.call(&SoapRequest::write("CreateItem", body)).await?;
        let message = soap::single(&response)?;
        Ok(first_item_id(message))
    }

    /// Sends a MIME message and keeps a copy in sent items. `Bcc` headers
    /// are honored and not transmitted.
    pub async fn send_mime(&self, mime: &[u8]) -> Result<(), EwsError> {
        self.create_mime_message(
            mime,
            &MimeDisposition::SendAndSaveCopy(DistinguishedFolder::SentItems.into()),
        )
        .await
        .map(drop)
    }

    /// Saves a MIME message as an unsent draft in the drafts folder.
    pub async fn save_draft_mime(&self, mime: &[u8]) -> Result<ItemId, EwsError> {
        self.create_mime_message(
            mime,
            &MimeDisposition::SaveDraft(DistinguishedFolder::Drafts.into()),
        )
        .await?
        .ok_or_else(|| EwsError::InvalidResponse("CreateItem returned no item id".into()))
    }

    /// Sends an existing draft, keeping a copy in sent items.
    pub async fn send_draft(&self, id: &ItemId) -> Result<(), EwsError> {
        let sent = FolderRef::from(DistinguishedFolder::SentItems);
        let body = send_item_request(std::slice::from_ref(id), Some(&sent));
        let response = self.call(&SoapRequest::write("SendItem", body)).await?;
        soap::single(&response).map(drop)
    }

    /// Fetches attachments by id; item attachments come with their MIME
    /// content. One result per id, in order.
    pub async fn get_attachments(
        &self,
        ids: &[&str],
    ) -> Result<Vec<Result<Attachment, ResponseError>>, EwsError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let body = get_attachment_request(ids, true);
        let response = self.call(&SoapRequest::read("GetAttachment", body)).await?;
        Ok(soap::each(&response)?
            .into_iter()
            .map(|result| {
                result.and_then(|message| {
                    message
                        .child("Attachments")
                        .and_then(|list| list.elements().find_map(parse_attachment))
                        .ok_or_else(|| {
                            ResponseError::new(
                                crate::ResponseCode::Other("ErrorNoAttachmentReturned".into()),
                                "GetAttachment returned no attachment",
                            )
                        })
                })
            })
            .collect())
    }

    /// One attachment with its content.
    pub async fn get_attachment(&self, id: &str) -> Result<Attachment, EwsError> {
        self.get_attachments(&[id])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("GetAttachment returned nothing".into()))?
            .map_err(EwsError::from)
    }

    /// Marks every item of a folder read or unread (Exchange 2013 and
    /// later), suppressing read receipts.
    pub async fn mark_all_read(
        &self,
        folder: impl Into<FolderRef>,
        read: bool,
    ) -> Result<(), EwsError> {
        if !self.version().has_2013_features() {
            return Err(EwsError::InvalidInput(
                "MarkAllItemsAsRead requires Exchange 2013 or later".into(),
            ));
        }
        let body = mark_all_read_request(&[folder.into()], read, true);
        let response = self
            .call(&SoapRequest::write("MarkAllItemsAsRead", body))
            .await?;
        soap::single(&response).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;
    use chrono::TimeZone;

    fn wrap(body: &str) -> Element {
        xml::parse(&format!(
            r#"<r xmlns:m="{}" xmlns:t="{}">{body}</r>"#,
            xml::NS_MESSAGES,
            xml::NS_TYPES
        ))
        .unwrap()
    }

    #[test]
    fn property_sets_follow_the_version() {
        let old = message_detail_properties(ExchangeVersion::Exchange2010Sp2);
        assert!(!old.contains(&PropertyPath::field("item:Preview")));
        assert!(!old.contains(&PropertyPath::field("item:Flag")));
        assert!(old.contains(&PropertyPath::extended_tag(0x1090, "Integer")));
        let new = message_detail_properties(ExchangeVersion::Exchange2013Sp1);
        assert!(new.contains(&PropertyPath::field("item:Preview")));
        assert!(new.contains(&PropertyPath::field("message:ToRecipients")));
        let summary = message_summary_properties(ExchangeVersion::Exchange2007Sp1);
        assert!(!summary.contains(&PropertyPath::field("item:ConversationId")));
        assert!(!summary.contains(&PropertyPath::field("message:ToRecipients")));
    }

    #[test]
    fn update_requests_for_2013_and_2010() {
        let now = Utc.with_ymd_and_hms(2024, 5, 1, 12, 0, 0).single().unwrap();
        let update = MessageUpdate {
            is_read: Some(true),
            flag: Some(FlagStatus::Complete),
            categories: Some(vec!["Red category".into(), "Work & Life".into()]),
        };
        let xml: Vec<String> = update
            .field_updates(ExchangeVersion::Exchange2013Sp1, now)
            .into_iter()
            .map(|u| match u {
                FieldUpdate::Set { value_xml, .. } => value_xml,
                FieldUpdate::Delete(_) => "delete".into(),
            })
            .collect();
        assert_eq!(xml[0], "<t:Message><t:IsRead>true</t:IsRead></t:Message>");
        assert_eq!(
            xml[1],
            "<t:Item><t:Flag><t:FlagStatus>Complete</t:FlagStatus><t:CompleteDate>2024-05-01T12:00:00Z</t:CompleteDate></t:Flag></t:Item>"
        );
        assert_eq!(
            xml[2],
            "<t:Item><t:Categories><t:String>Red category</t:String><t:String>Work &amp; Life</t:String></t:Categories></t:Item>"
        );

        let legacy = MessageUpdate {
            flag: Some(FlagStatus::Flagged),
            categories: Some(Vec::new()),
            ..MessageUpdate::default()
        }
        .field_updates(ExchangeVersion::Exchange2010Sp2, now);
        assert_eq!(legacy.len(), 3);
        assert!(
            matches!(&legacy[0], FieldUpdate::Set { path: PropertyPath::ExtendedTag { tag: 0x1090, .. }, value_xml } if value_xml.contains("<t:Value>2</t:Value>"))
        );
        assert!(
            matches!(&legacy[1], FieldUpdate::Set { path: PropertyPath::ExtendedTag { tag: 0x1095, .. }, value_xml } if value_xml.contains("<t:Value>6</t:Value>"))
        );
        assert!(
            matches!(&legacy[2], FieldUpdate::Delete(PropertyPath::Field(f)) if f == "item:Categories")
        );
        let unflag = MessageUpdate {
            flag: Some(FlagStatus::NotFlagged),
            ..MessageUpdate::default()
        }
        .field_updates(ExchangeVersion::Exchange2010, now);
        assert!(unflag.iter().all(|u| matches!(u, FieldUpdate::Delete(_))));
    }

    #[test]
    fn mime_create_requests() {
        let mime = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: Hi\r\n\r\nBody\r\n";
        let send = create_mime_request(
            mime,
            &MimeDisposition::SendAndSaveCopy(DistinguishedFolder::SentItems.into()),
        );
        assert!(send.starts_with(r#"<m:CreateItem MessageDisposition="SendAndSaveCopy"><m:SavedItemFolderId><t:DistinguishedFolderId Id="sentitems"/></m:SavedItemFolderId>"#));
        let root = wrap(&send);
        let content = root
            .path(&["CreateItem", "Items", "Message", "MimeContent"])
            .unwrap();
        assert_eq!(content.attr("CharacterSet"), Some("UTF-8"));
        assert_eq!(decode_base64(&content.text()).unwrap(), mime);

        let draft = create_mime_request(
            mime,
            &MimeDisposition::SaveDraft(DistinguishedFolder::Drafts.into()),
        );
        assert!(draft.contains(r#"MessageDisposition="SaveOnly""#));
        assert!(draft.contains(r#"<t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x0e07" PropertyType="Integer"/><t:Value>9</t:Value></t:ExtendedProperty>"#));
        let import = create_mime_request(
            mime,
            &MimeDisposition::Import {
                folder: DistinguishedFolder::Inbox.into(),
                is_read: true,
            },
        );
        assert!(import.contains("</t:MimeContent><t:IsRead>true</t:IsRead></t:Message>"));
        let only = create_mime_request(mime, &MimeDisposition::SendOnly);
        assert!(!only.contains("SavedItemFolderId"));

        let send_draft = send_item_request(
            &[ItemId::with_change_key("d", "k")],
            Some(&DistinguishedFolder::SentItems.into()),
        );
        assert_eq!(
            send_draft,
            r#"<m:SendItem SaveItemToFolder="true"><m:ItemIds><t:ItemId Id="d" ChangeKey="k"/></m:ItemIds><m:SavedItemFolderId><t:DistinguishedFolderId Id="sentitems"/></m:SavedItemFolderId></m:SendItem>"#
        );
    }

    #[test]
    fn attachment_and_mark_all_requests() {
        assert_eq!(
            get_attachment_request(&["att1", "att2"], true),
            r#"<m:GetAttachment><m:AttachmentShape><t:IncludeMimeContent>true</t:IncludeMimeContent></m:AttachmentShape><m:AttachmentIds><t:AttachmentId Id="att1"/><t:AttachmentId Id="att2"/></m:AttachmentIds></m:GetAttachment>"#
        );
        assert_eq!(
            mark_all_read_request(&[DistinguishedFolder::Inbox.into()], true, true),
            r#"<m:MarkAllItemsAsRead><m:ReadFlag>true</m:ReadFlag><m:SuppressReadReceipts>true</m:SuppressReadReceipts><m:FolderIds><t:DistinguishedFolderId Id="inbox"/></m:FolderIds></m:MarkAllItemsAsRead>"#
        );
    }

    #[test]
    fn parses_attachments() {
        let root = wrap(
            r#"<m:Attachments>
                 <t:FileAttachment><t:AttachmentId Id="f1"/><t:Name>report.pdf</t:Name><t:ContentType>application/pdf</t:ContentType><t:Size>5</t:Size><t:IsInline>false</t:IsInline><t:Content>JVBERi0=</t:Content></t:FileAttachment>
                 <t:ItemAttachment><t:AttachmentId Id="i1"/><t:Name>Fwd</t:Name><t:Message><t:MimeContent CharacterSet="UTF-8">U3ViamVjdDogeA0KDQp5</t:MimeContent><t:ItemId Id="x"/></t:Message></t:ItemAttachment>
               </m:Attachments>"#,
        );
        let list = root.child("Attachments").unwrap();
        let attachments: Vec<Attachment> = list.elements().filter_map(parse_attachment).collect();
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].info.kind, AttachmentKind::File);
        assert_eq!(attachments[0].content.as_deref(), Some(&b"%PDF-"[..]));
        assert_eq!(attachments[1].info.kind, AttachmentKind::Item);
        assert_eq!(
            attachments[1].mime_content.as_deref(),
            Some(&b"Subject: x\r\n\r\ny"[..])
        );
        assert!(attachments[1].content.is_none());
    }

    #[test]
    fn flags_from_extended_property_or_flag_element() {
        let legacy = wrap(
            r#"<t:Message><t:ItemId Id="a"/><t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x1090" PropertyType="Integer"/><t:Value>2</t:Value></t:ExtendedProperty></t:Message>"#,
        );
        let message = Message::from_element(legacy.child("Message").unwrap()).unwrap();
        assert!(message.is_flagged());
        let modern = wrap(
            r#"<t:Message><t:ItemId Id="a"/><t:Flag><t:FlagStatus>Complete</t:FlagStatus><t:CompleteDate>2024-05-02T10:00:00Z</t:CompleteDate></t:Flag></t:Message>"#,
        );
        let message = Message::from_element(modern.child("Message").unwrap()).unwrap();
        let flag = message.flag.unwrap();
        assert_eq!(flag.status, FlagStatus::Complete);
        assert!(flag.complete_date.is_some());
        let none = wrap(r#"<t:Message><t:ItemId Id="a"/></t:Message>"#);
        assert!(
            Message::from_element(none.child("Message").unwrap())
                .unwrap()
                .flag
                .is_none()
        );
    }

    #[test]
    fn base64_tolerates_line_breaks() {
        assert_eq!(decode_base64("SGVs\r\nbG8=\n").unwrap(), b"Hello");
        assert!(decode_base64("***").is_err());
    }
}

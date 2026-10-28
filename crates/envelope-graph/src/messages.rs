//! Mail messages: delta sync, listing, search and message operations.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::batch::BatchRequest;
use crate::client::{GraphClient, GraphRequest, Page};
use crate::delta::{DeltaCursor, DeltaPage};
use crate::error::GraphError;
use crate::folders::FolderRole;
use crate::serde_util::{graph_enum, lenient_bool, null_default};
use crate::types::{DateTimeTimeZone, Importance, Recipient};

/// Attachments below this size are posted inline; larger ones use an upload session.
const SMALL_ATTACHMENT: usize = 3 * 1024 * 1024;
/// Upload session chunk: a multiple of 320 KiB, below the 4 MiB maximum.
const UPLOAD_CHUNK: usize = 10 * 320 * 1024;

/// A file to attach to a draft with [`GraphClient::add_attachment`].
#[derive(Debug, Clone, Copy)]
pub struct NewAttachment<'a> {
    /// File name shown to recipients.
    pub name: &'a str,
    /// MIME type, e.g. `application/pdf`.
    pub content_type: &'a str,
    /// The file's contents.
    pub bytes: &'a [u8],
    /// `Content-ID` for inline images.
    pub content_id: Option<&'a str>,
    /// Whether the file is shown inside the body (an inline image).
    pub inline: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSession {
    upload_url: String,
}

/// The message properties a message list needs; the default `$select` of
/// delta sync, listing and search.
pub const MESSAGE_LIST_FIELDS: &[&str] = &[
    "id",
    "internetMessageId",
    "conversationId",
    "subject",
    "bodyPreview",
    "from",
    "sender",
    "toRecipients",
    "ccRecipients",
    "bccRecipients",
    "replyTo",
    "receivedDateTime",
    "sentDateTime",
    "isRead",
    "isDraft",
    "flag",
    "hasAttachments",
    "importance",
    "categories",
    "parentFolderId",
    "inferenceClassification",
    "changeKey",
];

graph_enum! {
    /// Follow-up flag state of a message.
    pub enum FlagStatus {
        /// Not flagged.
        NotFlagged = "notFlagged",
        /// Flag marked complete.
        Complete = "complete",
        /// Flagged for follow-up.
        Flagged = "flagged",
    }
    unknown = NotFlagged;
    default = NotFlagged;
}

graph_enum! {
    /// Focused Inbox classification.
    pub enum InferenceClassification {
        /// Shown in the Focused tab.
        Focused = "focused",
        /// Shown in the Other tab.
        Other = "other",
    }
    unknown = Focused;
    default = Focused;
}

/// Follow-up flag of a message (Graph `followupFlag`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowupFlag {
    /// Flag state.
    #[serde(default, deserialize_with = "null_default")]
    pub flag_status: FlagStatus,
    /// When follow-up starts. Graph requires it whenever `due_date_time` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date_time: Option<DateTimeTimeZone>,
    /// When follow-up is due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date_time: Option<DateTimeTimeZone>,
    /// When the flag was completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_date_time: Option<DateTimeTimeZone>,
}

impl FollowupFlag {
    /// A flag with only the status set.
    pub fn with_status(status: FlagStatus) -> Self {
        FollowupFlag {
            flag_status: status,
            ..FollowupFlag::default()
        }
    }
}

/// One Internet message header (`internetMessageHeaders`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternetMessageHeader {
    /// Header name.
    pub name: String,
    /// Header value.
    #[serde(default, deserialize_with = "null_default")]
    pub value: String,
}

/// A message as listed in a folder (the [`MESSAGE_LIST_FIELDS`] subset of the
/// Graph `message` resource). Properties that were not selected keep their
/// defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// Graph item id (immutable when the client requests immutable ids).
    pub id: String,
    /// RFC 5322 `Message-ID`.
    #[serde(default)]
    pub internet_message_id: Option<String>,
    /// Conversation (thread) id.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Subject.
    #[serde(default)]
    pub subject: Option<String>,
    /// First 255 characters of the body, as text.
    #[serde(default)]
    pub body_preview: Option<String>,
    /// The `From` mailbox.
    #[serde(default)]
    pub from: Option<Recipient>,
    /// The mailbox that actually sent the message (`Sender`).
    #[serde(default)]
    pub sender: Option<Recipient>,
    /// `To` recipients.
    #[serde(default, deserialize_with = "null_default")]
    pub to_recipients: Vec<Recipient>,
    /// `Cc` recipients.
    #[serde(default, deserialize_with = "null_default")]
    pub cc_recipients: Vec<Recipient>,
    /// `Bcc` recipients (only visible on drafts and sent items).
    #[serde(default, deserialize_with = "null_default")]
    pub bcc_recipients: Vec<Recipient>,
    /// `Reply-To` addresses.
    #[serde(default, deserialize_with = "null_default")]
    pub reply_to: Vec<Recipient>,
    /// When the message arrived.
    #[serde(default)]
    pub received_date_time: Option<DateTime<Utc>>,
    /// When the message was sent.
    #[serde(default)]
    pub sent_date_time: Option<DateTime<Utc>>,
    /// Read state.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub is_read: bool,
    /// True for unsent drafts.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub is_draft: bool,
    /// Follow-up flag.
    #[serde(default)]
    pub flag: Option<FollowupFlag>,
    /// Whether the message has (non-inline) attachments.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub has_attachments: bool,
    /// Importance.
    #[serde(default, deserialize_with = "null_default")]
    pub importance: Importance,
    /// Outlook categories.
    #[serde(default, deserialize_with = "null_default")]
    pub categories: Vec<String>,
    /// Folder that contains the message.
    #[serde(default)]
    pub parent_folder_id: Option<String>,
    /// Focused Inbox classification.
    #[serde(default)]
    pub inference_classification: Option<InferenceClassification>,
    /// Version of the item; changes with every modification.
    #[serde(default)]
    pub change_key: Option<String>,
    /// Internet headers (only when explicitly selected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internet_message_headers: Option<Vec<InternetMessageHeader>>,
}

impl Message {
    /// True when the follow-up flag is set (not completed).
    pub fn is_flagged(&self) -> bool {
        self.flag
            .as_ref()
            .is_some_and(|f| f.flag_status == FlagStatus::Flagged)
    }
}

/// Changes applied to a message with `PATCH`. Only fields that are `Some`
/// are sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageUpdate {
    /// New read state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_read: Option<bool>,
    /// New follow-up flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<FollowupFlag>,
    /// New category list (replaces the existing one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
    /// New importance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<Importance>,
    /// Move between the Focused and Other tabs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_classification: Option<InferenceClassification>,
}

impl MessageUpdate {
    /// An empty update.
    pub fn new() -> Self {
        MessageUpdate::default()
    }

    /// Sets the read state.
    pub fn read(mut self, read: bool) -> Self {
        self.is_read = Some(read);
        self
    }

    /// Sets the follow-up flag state.
    pub fn flag(mut self, status: FlagStatus) -> Self {
        self.flag = Some(FollowupFlag::with_status(status));
        self
    }

    /// Replaces the categories.
    pub fn categories<I, S>(mut self, categories: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.categories = Some(categories.into_iter().map(Into::into).collect());
        self
    }

    /// Sets the importance.
    pub fn importance(mut self, importance: Importance) -> Self {
        self.importance = Some(importance);
        self
    }

    /// Sets the Focused Inbox classification.
    pub fn inference_classification(mut self, classification: InferenceClassification) -> Self {
        self.inference_classification = Some(classification);
        self
    }

    /// True when nothing would change.
    pub fn is_empty(&self) -> bool {
        self == &MessageUpdate::default()
    }
}

/// Options for the first request of a message delta round. When resuming
/// from a [`DeltaCursor`], only `page_size` is used: everything else is
/// encoded in the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDeltaOptions {
    /// Properties to `$select` (default [`MESSAGE_LIST_FIELDS`]).
    pub select: Vec<String>,
    /// Maximum messages per page (`Prefer: odata.maxpagesize`).
    pub page_size: Option<u32>,
    /// Limits the round to messages received at or after this instant
    /// (`$filter=receivedDateTime ge …`). Graph then returns at most 5,000
    /// messages, and may still report removals and read-state changes of
    /// older messages.
    pub received_since: Option<DateTime<Utc>>,
    /// Newest messages first (`$orderby=receivedDateTime desc`, the only
    /// ordering delta supports). Without it the order is unspecified.
    pub newest_first: bool,
}

impl Default for MessageDeltaOptions {
    fn default() -> Self {
        MessageDeltaOptions {
            select: default_select(),
            page_size: Some(100),
            received_since: None,
            newest_first: true,
        }
    }
}

/// Options for [`GraphClient::list_messages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageListOptions {
    /// Page size (`$top`, 1–1000).
    pub top: u32,
    /// Number of messages to skip (`$skip`).
    pub skip: u32,
    /// Properties to `$select` (default [`MESSAGE_LIST_FIELDS`]).
    pub select: Vec<String>,
    /// Optional OData `$filter`.
    pub filter: Option<String>,
    /// Optional `$orderby`, default `receivedDateTime desc`.
    pub order_by: Option<String>,
}

impl Default for MessageListOptions {
    fn default() -> Self {
        MessageListOptions {
            top: 50,
            skip: 0,
            select: default_select(),
            filter: None,
            order_by: Some("receivedDateTime desc".into()),
        }
    }
}

fn default_select() -> Vec<String> {
    MESSAGE_LIST_FIELDS
        .iter()
        .map(|f| (*f).to_owned())
        .collect()
}

#[derive(Deserialize)]
struct IdOnly {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadersOnly {
    #[serde(default, deserialize_with = "null_default")]
    internet_message_headers: Vec<InternetMessageHeader>,
}

/// Quotes a `$search` term, escaping embedded quotes and backslashes.
fn search_term(query: &str) -> String {
    let mut quoted = String::with_capacity(query.len() + 2);
    quoted.push('"');
    for ch in query.chars() {
        if ch == '"' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

fn message_path(id: &str) -> [&str; 3] {
    ["me", "messages", id]
}

impl GraphClient {
    /// Fetches one page of the message delta for a folder.
    ///
    /// Pass `None` to start a full sync, or the cursor stored from the
    /// previous page: [`DeltaCursor::Next`] continues the current round and
    /// [`DeltaCursor::Done`] starts an incremental round. Persist each page's
    /// items, removals and [`DeltaPage::next`] together. When the stored link
    /// has expired the error satisfies [`GraphError::is_resync_required`];
    /// restart with `None` and reconcile.
    pub async fn message_delta(
        &self,
        folder_id: &str,
        cursor: Option<&DeltaCursor>,
        options: &MessageDeltaOptions,
    ) -> Result<DeltaPage<Message>, GraphError> {
        let mut request = match cursor {
            Some(cursor) => GraphRequest::link(Method::GET, cursor.url()),
            None => {
                let mut request =
                    GraphRequest::get(["me", "mailFolders", folder_id, "messages", "delta"]);
                if !options.select.is_empty() {
                    request = request.query("$select", options.select.join(","));
                }
                if let Some(since) = options.received_since {
                    request = request.query(
                        "$filter",
                        format!(
                            "receivedDateTime ge {}",
                            since.to_rfc3339_opts(SecondsFormat::Secs, true)
                        ),
                    );
                }
                if options.newest_first {
                    request = request.query("$orderby", "receivedDateTime desc");
                }
                request
            }
        };
        if let Some(size) = options.page_size {
            request = request.prefer(format!("odata.maxpagesize={size}"));
        }
        self.delta_page_raw(&request).await
    }

    /// Lists messages of a folder without delta tracking. Follow
    /// [`Page::next_link`] with [`GraphClient::list_messages_next`].
    pub async fn list_messages(
        &self,
        folder_id: &str,
        options: &MessageListOptions,
    ) -> Result<Page<Message>, GraphError> {
        let mut request = GraphRequest::get(["me", "mailFolders", folder_id, "messages"])
            .query("$top", options.top.clamp(1, 1000).to_string());
        if options.skip > 0 {
            request = request.query("$skip", options.skip.to_string());
        }
        if !options.select.is_empty() {
            request = request.query("$select", options.select.join(","));
        }
        if let Some(filter) = &options.filter {
            request = request.query("$filter", filter.as_str());
        }
        if let Some(order_by) = &options.order_by {
            request = request.query("$orderby", order_by.as_str());
        }
        self.get_page(&request).await
    }

    /// Fetches the page behind a `next_link` of [`GraphClient::list_messages`]
    /// or [`GraphClient::search_messages`].
    pub async fn list_messages_next(&self, next_link: &str) -> Result<Page<Message>, GraphError> {
        self.get_page(&GraphRequest::link(Method::GET, next_link))
            .await
    }

    /// Server-side search (`$search`, KQL syntax such as `from:alex` or
    /// `subject:report`) across the mailbox or within one folder. Results are
    /// ordered by sent date; Graph returns at most 1,000 of them and does not
    /// allow combining `$search` with `$filter`, `$orderby` or `$skip`.
    pub async fn search_messages(
        &self,
        query: &str,
        folder_id: Option<&str>,
        top: u32,
    ) -> Result<Page<Message>, GraphError> {
        if query.trim().is_empty() {
            return Err(GraphError::InvalidInput("empty search query".into()));
        }
        let request = match folder_id {
            Some(folder) => GraphRequest::get(["me", "mailFolders", folder, "messages"]),
            None => GraphRequest::get(["me", "messages"]),
        }
        .query("$search", search_term(query))
        .query("$select", MESSAGE_LIST_FIELDS.join(","))
        .query("$top", top.clamp(1, 1000).to_string());
        self.get_page(&request).await
    }

    /// One message with the [`MESSAGE_LIST_FIELDS`] properties.
    pub async fn get_message(&self, message_id: &str) -> Result<Message, GraphError> {
        let request = GraphRequest::get(message_path(message_id))
            .query("$select", MESSAGE_LIST_FIELDS.join(","));
        self.send_json(&request).await
    }

    /// The raw RFC 822 / MIME content of a message (`$value`).
    pub async fn get_message_mime(&self, message_id: &str) -> Result<Bytes, GraphError> {
        let request =
            GraphRequest::get(["me", "messages", message_id, "$value"]).header("Accept", "*/*");
        Ok(self.execute(&request).await?.into_body())
    }

    /// The Internet message headers of a message.
    pub async fn get_message_headers(
        &self,
        message_id: &str,
    ) -> Result<Vec<InternetMessageHeader>, GraphError> {
        let request =
            GraphRequest::get(message_path(message_id)).query("$select", "internetMessageHeaders");
        let headers: HeadersOnly = self.send_json(&request).await?;
        Ok(headers.internet_message_headers)
    }

    /// Applies an update (read state, flag, categories, …) to one message.
    pub async fn update_message(
        &self,
        message_id: &str,
        update: &MessageUpdate,
    ) -> Result<(), GraphError> {
        if update.is_empty() {
            return Ok(());
        }
        let request = GraphRequest::patch(message_path(message_id)).json(update)?;
        self.send_empty(&request).await
    }

    /// Marks a message read or unread.
    pub async fn set_message_read(&self, message_id: &str, read: bool) -> Result<(), GraphError> {
        self.update_message(message_id, &MessageUpdate::new().read(read))
            .await
    }

    /// Sets the follow-up flag state of a message.
    pub async fn set_message_flag(
        &self,
        message_id: &str,
        status: FlagStatus,
    ) -> Result<(), GraphError> {
        self.update_message(message_id, &MessageUpdate::new().flag(status))
            .await
    }

    /// Replaces the categories of a message.
    pub async fn set_message_categories(
        &self,
        message_id: &str,
        categories: &[String],
    ) -> Result<(), GraphError> {
        self.update_message(
            message_id,
            &MessageUpdate::new().categories(categories.iter().cloned()),
        )
        .await
    }

    /// Applies updates to many messages through JSON batches. The outer
    /// `Result` fails only when batching itself fails; the inner results are
    /// in input order.
    pub async fn update_messages(
        &self,
        updates: &[(String, MessageUpdate)],
    ) -> Result<Vec<Result<(), GraphError>>, GraphError> {
        let mut requests = Vec::new();
        for (index, (id, update)) in updates.iter().enumerate() {
            if !update.is_empty() {
                requests.push(BatchRequest::new(
                    index.to_string(),
                    GraphRequest::patch(message_path(id)).json(update)?,
                ));
            }
        }
        let mut results: Vec<Result<(), GraphError>> = updates.iter().map(|_| Ok(())).collect();
        for response in self.batch(requests).await? {
            if let Some(slot) = response
                .id
                .parse::<usize>()
                .ok()
                .and_then(|i| results.get_mut(i))
            {
                *slot = response.into_result().map(drop);
            }
        }
        Ok(results)
    }

    /// Moves a message to another folder (id or well-known name) and returns
    /// the id of the moved message, which differs from the original unless
    /// immutable ids are enabled.
    pub async fn move_message(
        &self,
        message_id: &str,
        destination_id: &str,
    ) -> Result<String, GraphError> {
        let request = GraphRequest::post(["me", "messages", message_id, "move"])
            .json(&json!({ "destinationId": destination_id }))?;
        let moved: IdOnly = self.send_json(&request).await?;
        Ok(moved.id)
    }

    /// Moves many messages through JSON batches; returns the new ids in input
    /// order.
    pub async fn move_messages(
        &self,
        message_ids: &[String],
        destination_id: &str,
    ) -> Result<Vec<Result<String, GraphError>>, GraphError> {
        let requests = message_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                Ok(BatchRequest::new(
                    index.to_string(),
                    GraphRequest::post(["me", "messages", id, "move"])
                        .json(&json!({ "destinationId": destination_id }))?,
                ))
            })
            .collect::<Result<Vec<_>, GraphError>>()?;
        Ok(self
            .batch(requests)
            .await?
            .into_iter()
            .map(|response| response.json::<IdOnly>().map(|moved| moved.id))
            .collect())
    }

    /// Copies a message to another folder; returns the id of the copy.
    pub async fn copy_message(
        &self,
        message_id: &str,
        destination_id: &str,
    ) -> Result<String, GraphError> {
        let request = GraphRequest::post(["me", "messages", message_id, "copy"])
            .json(&json!({ "destinationId": destination_id }))?;
        let copy: IdOnly = self.send_json(&request).await?;
        Ok(copy.id)
    }

    /// Moves a message to Deleted Items (what "Delete" does in Outlook) and
    /// returns its new id.
    pub async fn trash_message(&self, message_id: &str) -> Result<String, GraphError> {
        self.move_message(message_id, FolderRole::DeletedItems.as_str())
            .await
    }

    /// Soft-deletes a message (`DELETE`): Exchange moves it to the
    /// Recoverable Items folder, bypassing Deleted Items. Use
    /// [`GraphClient::trash_message`] for a regular delete.
    pub async fn delete_message(&self, message_id: &str) -> Result<(), GraphError> {
        self.send_empty(&GraphRequest::delete(message_path(message_id)))
            .await
    }

    /// Permanently deletes a message (moved to the purges folder of the
    /// dumpster; not recoverable by mail clients). Global cloud only.
    pub async fn permanently_delete_message(&self, message_id: &str) -> Result<(), GraphError> {
        let request = GraphRequest::post(["me", "messages", message_id, "permanentDelete"]);
        self.send_empty(&request).await
    }

    /// Sends a complete RFC 822 / MIME message (`POST /me/sendMail` with a
    /// base64 body). Recipients are taken from the MIME headers and the
    /// message is saved to Sent Items. Graph limits the request to about
    /// 4 MB; larger messages fail with `413`.
    pub async fn send_mime(&self, mime: &[u8]) -> Result<(), GraphError> {
        if mime.is_empty() {
            return Err(GraphError::InvalidInput("empty MIME message".into()));
        }
        let request = GraphRequest::post(["me", "sendMail"]).text(BASE64.encode(mime));
        self.send_empty(&request).await
    }

    /// Creates a draft from MIME content in Drafts (or in `folder_id`) and
    /// returns it. Subject to the same ~4 MB limit as [`GraphClient::send_mime`].
    pub async fn create_draft_mime(
        &self,
        mime: &[u8],
        folder_id: Option<&str>,
    ) -> Result<Message, GraphError> {
        if mime.is_empty() {
            return Err(GraphError::InvalidInput("empty MIME message".into()));
        }
        let request = match folder_id {
            Some(folder) => GraphRequest::post(["me", "mailFolders", folder, "messages"]),
            None => GraphRequest::post(["me", "messages"]),
        }
        .text(BASE64.encode(mime));
        self.send_json(&request).await
    }

    /// Attaches a file to a draft. Files of 3 MB or more go through an
    /// upload session in chunks, so attachments up to 150 MB work.
    pub async fn add_attachment(
        &self,
        message_id: &str,
        attachment: &NewAttachment<'_>,
    ) -> Result<(), GraphError> {
        let a = attachment;
        if a.bytes.len() < SMALL_ATTACHMENT {
            let body = json!({
                "@odata.type": "#microsoft.graph.fileAttachment",
                "name": a.name,
                "contentType": a.content_type,
                "contentBytes": BASE64.encode(a.bytes),
                "isInline": a.inline,
                "contentId": a.content_id,
            });
            let request =
                GraphRequest::post(["me", "messages", message_id, "attachments"]).json(&body)?;
            let _: serde_json::Value = self.send_json(&request).await?;
            return Ok(());
        }
        let body = json!({
            "AttachmentItem": {
                "attachmentType": "file",
                "name": a.name,
                "size": a.bytes.len(),
                "contentType": a.content_type,
                "isInline": a.inline,
                "contentId": a.content_id,
            }
        });
        let request = GraphRequest::post([
            "me",
            "messages",
            message_id,
            "attachments",
            "createUploadSession",
        ])
        .json(&body)?;
        let session: UploadSession = self.send_json(&request).await?;
        let total = a.bytes.len();
        let mut start = 0;
        while start < total {
            let end = (start + UPLOAD_CHUNK).min(total);
            self.put_range(
                &session.upload_url,
                Bytes::copy_from_slice(&a.bytes[start..end]),
                start,
                total,
            )
            .await?;
            start = end;
        }
        Ok(())
    }

    /// Sends an existing draft.
    pub async fn send_draft(&self, message_id: &str) -> Result<(), GraphError> {
        self.send_empty(&GraphRequest::post(["me", "messages", message_id, "send"]))
            .await
    }

    /// Deletes a draft.
    pub async fn delete_draft(&self, message_id: &str) -> Result<(), GraphError> {
        self.delete_message(message_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_search_terms() {
        assert_eq!(search_term("from:alex"), "\"from:alex\"");
        assert_eq!(search_term("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn update_serializes_only_set_fields() {
        let update = MessageUpdate::new().read(true).flag(FlagStatus::Flagged);
        let json = serde_json::to_value(&update).unwrap_or_default();
        assert_eq!(
            json,
            json!({"isRead": true, "flag": {"flagStatus": "flagged"}})
        );
        assert!(MessageUpdate::new().is_empty());
    }

    #[test]
    fn decodes_sparse_messages() {
        let message: Message = serde_json::from_value(json!({
            "id": "AAMk",
            "toRecipients": null,
            "importance": "High",
            "flag": {"flagStatus": "flagged"}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(message.to_recipients.is_empty());
        assert_eq!(message.importance, Importance::High);
        assert!(message.is_flagged());
        assert!(!message.is_read);
    }

    #[test]
    fn accepts_string_booleans() {
        let message: Message =
            serde_json::from_value(json!({"id": "a", "isRead": "true", "hasAttachments": null}))
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(message.is_read);
        assert!(!message.has_attachments);
        let bad: Result<Message, _> = serde_json::from_value(json!({"id": "a", "isRead": "yes"}));
        assert!(bad.is_err());
    }
}

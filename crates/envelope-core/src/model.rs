//! Types shared with the user interface (serialized as camelCase JSON).

use serde::{Deserialize, Serialize};

/// Kind of backend an account syncs through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountKind {
    /// Standard IMAP + SMTP (+ optional CalDAV/CardDAV).
    Imap,
    /// Gmail / Google Workspace: IMAP with Gmail extensions + Google APIs, OAuth.
    Gmail,
    /// Outlook.com / Microsoft 365 via Microsoft Graph, OAuth.
    Microsoft,
    /// On-premises Microsoft Exchange via Exchange Web Services (EWS), password.
    Exchange,
}

impl AccountKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountKind::Imap => "imap",
            AccountKind::Gmail => "gmail",
            AccountKind::Microsoft => "microsoft",
            AccountKind::Exchange => "exchange",
        }
    }

    pub fn parse(s: &str) -> AccountKind {
        match s {
            "gmail" => AccountKind::Gmail,
            "microsoft" => AccountKind::Microsoft,
            "exchange" => AccountKind::Exchange,
            _ => AccountKind::Imap,
        }
    }
}

/// Connection security for a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Security {
    #[default]
    Tls,
    StartTls,
    None,
}

/// A mail or DAV server endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// Login name; defaults to the account e-mail when empty.
    #[serde(default)]
    pub username: String,
}

/// How the account authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMethod {
    #[default]
    Password,
    OAuthGoogle,
    OAuthMicrosoft,
}

/// Persistent account configuration (secrets live in the system keyring).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountConfig {
    pub auth: AuthMethod,
    pub imap: Option<ServerConfig>,
    pub smtp: Option<ServerConfig>,
    /// CalDAV server URL (iCloud, Yahoo, Fastmail, Nextcloud, …).
    pub caldav_url: Option<String>,
    /// CardDAV server URL.
    pub carddav_url: Option<String>,
    /// Accept self-signed / invalid TLS certificates.
    pub accept_invalid_certs: bool,
    /// Whether to save a copy of sent mail to the Sent folder ourselves
    /// (servers like Gmail and Outlook do it automatically).
    pub save_sent_copy: Option<bool>,
    /// How many days of mail to download message bodies for: a year when
    /// unset, all mail with 0 (see [`AccountConfig::bodies_since`]).
    pub body_sync_days: Option<u32>,
    /// Which new mail of the account notifies.
    pub notify: NotifyMode,
    /// Exchange Web Services settings (Exchange accounts only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange: Option<ExchangeConfig>,
}

impl AccountConfig {
    /// The oldest date (ms) whose message bodies are downloaded ahead of
    /// time; `None` for all mail.
    pub fn bodies_since(&self, now: i64) -> Option<i64> {
        match self.body_sync_days.unwrap_or(365) {
            0 => None,
            days => Some(now - i64::from(days) * 86_400_000),
        }
    }
}

/// Which new mail of an account notifies (on top of the app's own switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotifyMode {
    /// Every new conversation in the inbox.
    #[default]
    All,
    /// Conversations with people: no newsletters or automatic notifications.
    People,
    /// Nothing.
    None,
}

/// How an Exchange account signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExchangeAuth {
    /// NTLM (Windows integrated authentication), falling back to Basic when
    /// the server does not offer NTLM.
    #[default]
    Auto,
    Ntlm,
    Basic,
}

/// Connection settings of an on-premises Exchange account.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExchangeConfig {
    /// EWS endpoint, e.g. `https://mail.example.com/EWS/Exchange.asmx`.
    pub ews_url: String,
    /// Login name (`DOMAIN\user` or a UPN); empty means the account e-mail.
    pub username: String,
    pub auth: ExchangeAuth,
    /// EWS schema version the server speaks (`Exchange2016`), once known.
    pub version: Option<String>,
}

/// High-level sync state of an account, shown in the sidebar.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum SyncState {
    #[default]
    Idle,
    Connecting,
    Syncing {
        /// Folder currently being synchronized.
        folder: Option<String>,
        /// 0..=1 progress of the initial sync, when known.
        progress: Option<f32>,
    },
    /// The server could not be reached; retried with growing pauses.
    Offline {
        /// What went wrong the last time.
        #[serde(default)]
        message: String,
        /// Attempts in a row that failed (filled in by the engine).
        #[serde(default)]
        failures: u32,
    },
    AuthError {
        message: String,
    },
    Error {
        message: String,
        /// Attempts in a row that failed (filled in by the engine).
        #[serde(default)]
        failures: u32,
    },
}

impl SyncState {
    /// A failed attempt to sync (offline when it may work again by itself).
    pub fn failed(message: String, transient: bool) -> SyncState {
        if transient {
            SyncState::Offline {
                message,
                failures: 0,
            }
        } else {
            SyncState::Error {
                message,
                failures: 0,
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: i64,
    pub kind: AccountKind,
    pub provider: String,
    pub email: String,
    pub name: String,
    pub label: Option<String>,
    pub color: String,
    pub sort_order: i64,
    pub enabled: bool,
    pub config: AccountConfig,
    pub sync: SyncState,
    pub has_calendar: bool,
    pub has_contacts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub email: String,
    pub reply_to: Option<String>,
    pub signature: Option<String>,
    pub is_default: bool,
}

/// Special purpose of a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    Starred,
    Important,
}

impl FolderRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Sent => "sent",
            FolderRole::Drafts => "drafts",
            FolderRole::Trash => "trash",
            FolderRole::Junk => "junk",
            FolderRole::Archive => "archive",
            FolderRole::All => "all",
            FolderRole::Starred => "starred",
            FolderRole::Important => "important",
        }
    }

    pub fn parse(s: &str) -> Option<FolderRole> {
        Some(match s {
            "inbox" => FolderRole::Inbox,
            "sent" => FolderRole::Sent,
            "drafts" => FolderRole::Drafts,
            "trash" => FolderRole::Trash,
            "junk" => FolderRole::Junk,
            "archive" => FolderRole::Archive,
            "all" => FolderRole::All,
            "starred" => FolderRole::Starred,
            "important" => FolderRole::Important,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub path: String,
    pub parent_id: Option<i64>,
    pub role: Option<FolderRole>,
    /// `folder` or `label`.
    pub kind: String,
    pub unread_count: i64,
    pub total_count: i64,
    pub color: Option<String>,
    pub hidden: bool,
    pub selectable: bool,
    pub sort_order: i64,
    /// Separator of nested IMAP folder names (`/`, `.`), which names cannot contain.
    pub delimiter: Option<String>,
}

/// An e-mail address with optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    pub fn new(name: Option<String>, email: impl Into<String>) -> Self {
        Address {
            name: name.filter(|n| !n.trim().is_empty()),
            email: email.into(),
        }
    }

    /// `"Name" <email>` or just the address.
    pub fn to_header(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => {
                let needs_quotes = n.chars().any(|c| ",;:<>@\"()[]\\".contains(c));
                if needs_quotes {
                    format!(
                        "\"{}\" <{}>",
                        n.replace('\\', "\\\\").replace('"', "\\\""),
                        self.email
                    )
                } else {
                    format!("{n} <{}>", self.email)
                }
            }
            _ => self.email.clone(),
        }
    }
}

/// Message flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub forwarded: bool,
}

impl Flags {
    pub const SEEN: i64 = 1;
    pub const FLAGGED: i64 = 2;
    pub const ANSWERED: i64 = 4;
    pub const DRAFT: i64 = 8;
    pub const FORWARDED: i64 = 16;

    pub fn from_bits(bits: i64) -> Flags {
        Flags {
            seen: bits & Self::SEEN != 0,
            flagged: bits & Self::FLAGGED != 0,
            answered: bits & Self::ANSWERED != 0,
            draft: bits & Self::DRAFT != 0,
            forwarded: bits & Self::FORWARDED != 0,
        }
    }

    pub fn bits(&self) -> i64 {
        ((self.seen as i64) * Self::SEEN)
            | ((self.flagged as i64) * Self::FLAGGED)
            | ((self.answered as i64) * Self::ANSWERED)
            | ((self.draft as i64) * Self::DRAFT)
            | ((self.forwarded as i64) * Self::FORWARDED)
    }
}

/// A conversation as shown in the message list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: i64,
    pub account_id: i64,
    pub subject: String,
    pub snippet: String,
    pub last_date: i64,
    pub message_count: i64,
    pub unread_count: i64,
    pub flagged: bool,
    pub has_attachments: bool,
    pub draft_count: i64,
    pub participants: Vec<Address>,
    /// Sender of the most recent message (for the avatar).
    pub last_sender: Option<Address>,
    pub folder_ids: Vec<i64>,
    pub category: Option<String>,
    pub is_invite: bool,
    /// Later replies skip the inbox and do not notify.
    pub muted: bool,
    pub snoozed_until: Option<i64>,
}

/// A page of the message list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPage {
    pub total: i64,
    pub offset: i64,
    pub items: Vec<ThreadSummary>,
}

/// Which threads to list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Mailbox {
    /// The given role across all accounts (unified inbox, all sent, …).
    Unified { role: FolderRole },
    /// One folder or label.
    Folder { id: i64 },
    /// Starred threads across all accounts.
    Starred,
    /// Snoozed threads.
    Snoozed,
    /// Sent conversations whose follow-up reminder came due without a reply.
    FollowUp,
    /// Everything except trash and junk.
    Everything,
}

/// Extra filters applied to a list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListFilter {
    pub unread: bool,
    pub starred: bool,
    pub attachments: bool,
    /// Smart inbox category: `people`, `newsletters`, `notifications`.
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQuery {
    pub mailbox: Mailbox,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub filter: ListFilter,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInfo {
    pub id: i64,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub inline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeInfo {
    /// `https:` URL to open or POST (one-click).
    pub url: Option<String>,
    /// `mailto:` address to write to.
    pub mailto: Option<String>,
    /// RFC 8058 one-click unsubscribe is supported.
    pub one_click: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageBody {
    pub html: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub thread_id: i64,
    pub account_id: i64,
    pub folder_ids: Vec<i64>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub date: i64,
    pub flags: Flags,
    pub snippet: String,
    pub size: i64,
    pub has_attachments: bool,
    pub attachments: Vec<AttachmentInfo>,
    pub body: Option<MessageBody>,
    pub unsubscribe: Option<UnsubscribeInfo>,
    pub calendar_method: Option<String>,
    pub is_auto: bool,
    /// OpenPGP content: `encrypted`, `signed`, `inline-encrypted` or
    /// `inline-signed` (see `Engine::open_message`).
    pub crypto: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread: ThreadSummary,
    pub messages: Vec<MessageView>,
}

/// Where to move threads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MoveTarget {
    Inbox,
    Archive,
    Trash,
    Junk,
    Folder { id: i64 },
}

/// Flag changes to apply.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FlagChange {
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
}

/// An attachment in a draft (a local file or a part of an existing message).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAttachment {
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    /// Absolute path of a local file.
    #[serde(default)]
    pub path: Option<String>,
    /// Existing attachment (forwarding).
    #[serde(default)]
    pub attachment_id: Option<i64>,
    /// A whole message, attached as `message/rfc822` (forwarding as an attachment).
    #[serde(default)]
    pub message_id: Option<i64>,
    /// Content-ID for inline images.
    #[serde(default)]
    pub content_id: Option<String>,
    #[serde(default)]
    pub inline: bool,
}

/// Composer state persisted as a draft and used to send.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DraftInput {
    pub account_id: i64,
    pub identity_id: Option<i64>,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub html: String,
    pub text: Option<String>,
    pub attachments: Vec<DraftAttachment>,
    /// Message being replied to / forwarded.
    pub reply_to_message_id: Option<i64>,
    /// `reply`, `replyAll`, `forward` or `new`.
    pub mode: Option<String>,
    pub request_read_receipt: bool,
    /// Remind me if nobody replies within this many days.
    pub follow_up_days: Option<u32>,
    /// Encrypt with OpenPGP (to every recipient and the sender).
    pub encrypt: Option<bool>,
    /// Sign with the sender's OpenPGP key.
    pub sign: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: i64,
    pub data: DraftInput,
    pub updated_at: i64,
    /// The synced message that is this draft's copy on the server, if any.
    #[serde(default)]
    pub server_message_id: Option<i64>,
    /// Only on this computer: never stored on the server, or changed here
    /// since and its server copy is gone. Shown in Drafts by the interface.
    #[serde(default)]
    pub local_only: bool,
}

/// The header above quoted mail in replies and forwards, in the interface
/// language. Missing texts are English.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QuoteText {
    /// The line above a quoted reply, with `{date}` and `{sender}`:
    /// "On {date}, {sender} wrote:".
    pub wrote: Option<String>,
    /// The first line of a forwarded message's header.
    pub forwarded: Option<String>,
    /// Field names in a forwarded message's header.
    pub from: Option<String>,
    pub date: Option<String>,
    pub subject: Option<String>,
    pub to: Option<String>,
    /// The original's date and time as the interface writes them.
    pub when: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxItem {
    pub id: i64,
    pub account_id: i64,
    pub subject: String,
    pub to: Vec<Address>,
    pub send_at: i64,
    pub state: String,
    pub error: Option<String>,
}

/// A contact suggestion for address autocomplete.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactSuggestion {
    pub name: Option<String>,
    pub email: String,
    pub score: f64,
}

/// Auto-detected settings for an e-mail address.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub email: String,
    pub provider: String,
    pub provider_name: String,
    pub kind: AccountKind,
    pub auth: AuthMethod,
    pub imap: Option<ServerConfig>,
    pub smtp: Option<ServerConfig>,
    pub caldav_url: Option<String>,
    pub carddav_url: Option<String>,
    /// Hint shown to users (e.g. "create an app-specific password").
    pub password_hint: Option<String>,
    pub password_help_url: Option<String>,
}

/// Input for adding an account with a password.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAccount {
    pub email: String,
    pub name: String,
    pub provider: String,
    pub password: String,
    pub config: AccountConfig,
}

/// Input for adding an on-premises Exchange account.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewExchangeAccount {
    pub email: String,
    pub name: String,
    pub password: String,
    /// EWS URL or server name; found with Autodiscover when empty.
    pub ews_url: Option<String>,
    /// Login name (`DOMAIN\user` or a UPN) when it is not the e-mail address.
    pub username: Option<String>,
    pub accept_invalid_certs: bool,
    pub auth: ExchangeAuth,
}

/// Events pushed from the engine to the UI, e.g.
/// `{ "type": "mailChanged", "accountId": 1, "folderIds": [..], "threadIds": [..] }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum EngineEvent {
    AccountsChanged,
    FoldersChanged {
        account_id: i64,
    },
    /// Threads changed in the given folders; visible lists should refresh.
    MailChanged {
        account_id: i64,
        folder_ids: Vec<i64>,
        thread_ids: Vec<i64>,
    },
    SyncStatus {
        account_id: i64,
        sync: SyncState,
    },
    /// Messages are being imported into a folder: how many so far.
    ImportProgress {
        folder_id: i64,
        done: usize,
    },
    /// New unread mail arrived in an inbox (for notifications).
    NewMail {
        account_id: i64,
        threads: Vec<ThreadSummary>,
    },
    CalendarChanged {
        calendar_ids: Vec<i64>,
    },
    /// Contacts (or the list of address books) changed.
    ContactsChanged {
        book_ids: Vec<i64>,
    },
    /// Tasks in these lists changed; an empty `list_ids` means the lists
    /// themselves changed (reload everything).
    TasksChanged {
        list_ids: Vec<i64>,
    },
    OutboxChanged,
    /// A message failed to send.
    SendFailed {
        outbox_id: i64,
        error: String,
    },
    /// An event reminder is due.
    Reminder {
        event_id: i64,
        title: String,
        start: i64,
        location: Option<String>,
    },
    /// Nobody replied to a sent message before its follow-up reminder.
    FollowUpDue {
        thread_id: i64,
        subject: String,
    },
    /// An open task became due at its time of day.
    TaskDue {
        task_id: i64,
        title: String,
        due_at: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contacts_changed_wire_format() {
        let json = serde_json::to_value(EngineEvent::ContactsChanged {
            book_ids: vec![3, 7],
        })
        .unwrap_or_default();
        assert_eq!(
            json,
            serde_json::json!({ "type": "contactsChanged", "bookIds": [3, 7] })
        );
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;

    #[test]
    fn events_use_camel_case_fields() {
        let json = serde_json::to_value(EngineEvent::MailChanged {
            account_id: 1,
            folder_ids: vec![2],
            thread_ids: vec![3],
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "mailChanged", "accountId": 1, "folderIds": [2], "threadIds": [3] })
        );
        let json =
            serde_json::to_value(EngineEvent::ContactsChanged { book_ids: vec![4] }).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "contactsChanged", "bookIds": [4] })
        );
    }

    #[test]
    fn task_events_wire_format() {
        let json = serde_json::to_value(EngineEvent::TasksChanged {
            list_ids: vec![1, 5],
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "tasksChanged", "listIds": [1, 5] })
        );
        let json = serde_json::to_value(EngineEvent::TaskDue {
            task_id: 7,
            title: "Call Maya".into(),
            due_at: 1_700_000_000_000,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "taskDue", "taskId": 7, "title": "Call Maya", "dueAt": 1_700_000_000_000_i64 })
        );
    }

    #[test]
    fn bodies_are_kept_for_a_year_unless_the_account_says_otherwise() {
        let now = 1_000 * 86_400_000;
        let with = |days: Option<u32>| AccountConfig {
            body_sync_days: days,
            ..AccountConfig::default()
        };
        assert_eq!(with(None).bodies_since(now), Some(now - 365 * 86_400_000));
        assert_eq!(
            with(Some(30)).bodies_since(now),
            Some(now - 30 * 86_400_000)
        );
        assert_eq!(with(Some(0)).bodies_since(now), None);
    }
}

//! Identifiers, folder and item references, mailboxes, property paths and
//! other building blocks shared by all operations.

use std::fmt;

use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::xml::{Element, XmlWriter};

/// The id of an item together with its change key.
///
/// Exchange item ids are opaque strings. The change key identifies a
/// version of the item; it may be omitted when reading, and updates made by
/// this crate do not need it (they use `ConflictResolution="AlwaysOverwrite"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemId {
    /// The item id.
    pub id: String,
    /// The change key of the version the id was obtained from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_key: Option<String>,
}

impl ItemId {
    /// An id without change key.
    pub fn new(id: impl Into<String>) -> Self {
        ItemId {
            id: id.into(),
            change_key: None,
        }
    }

    /// An id with change key.
    pub fn with_change_key(id: impl Into<String>, change_key: impl Into<String>) -> Self {
        ItemId {
            id: id.into(),
            change_key: Some(change_key.into()),
        }
    }

    /// Reads the `Id` / `ChangeKey` attributes of an id element.
    pub(crate) fn from_element(element: &Element) -> Option<Self> {
        let id = element.attr("Id").filter(|id| !id.is_empty())?;
        Some(ItemId {
            id: id.to_owned(),
            change_key: element
                .attr("ChangeKey")
                .filter(|ck| !ck.is_empty())
                .map(str::to_owned),
        })
    }

    pub(crate) fn write(&self, w: &mut XmlWriter, tag: &str) {
        write_id(w, tag, &self.id, self.change_key.as_deref());
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)
    }
}

/// The id of a folder together with its change key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderId {
    /// The folder id.
    pub id: String,
    /// The change key of the version the id was obtained from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_key: Option<String>,
}

impl FolderId {
    /// An id without change key.
    pub fn new(id: impl Into<String>) -> Self {
        FolderId {
            id: id.into(),
            change_key: None,
        }
    }

    /// An id with change key.
    pub fn with_change_key(id: impl Into<String>, change_key: impl Into<String>) -> Self {
        FolderId {
            id: id.into(),
            change_key: Some(change_key.into()),
        }
    }

    pub(crate) fn from_element(element: &Element) -> Option<Self> {
        ItemId::from_element(element).map(|ItemId { id, change_key }| FolderId { id, change_key })
    }

    pub(crate) fn write(&self, w: &mut XmlWriter, tag: &str) {
        write_id(w, tag, &self.id, self.change_key.as_deref());
    }
}

impl fmt::Display for FolderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)
    }
}

fn write_id(w: &mut XmlWriter, tag: &str, id: &str, change_key: Option<&str>) {
    match change_key {
        Some(change_key) => w.empty(tag, &[("Id", id), ("ChangeKey", change_key)]),
        None => w.empty(tag, &[("Id", id)]),
    };
}

macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$vmeta:meta])* $variant:ident = $wire:literal,)+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant,)+];

            /// The value as written in EWS XML.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $wire,)+
                }
            }

            /// Parses the EWS XML value (case-insensitive).
            pub fn parse(value: &str) -> Option<Self> {
                let value = value.trim();
                $name::ALL
                    .iter()
                    .copied()
                    .find(|v| v.as_str().eq_ignore_ascii_case(value))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub(crate) use wire_enum;

wire_enum! {
    /// Distinguished (well-known) folders, addressable without knowing
    /// their ids.
    #[non_exhaustive]
    pub enum DistinguishedFolder {
        /// The inbox.
        Inbox = "inbox",
        /// Drafts.
        Drafts = "drafts",
        /// Sent items.
        SentItems = "sentitems",
        /// Deleted items (trash).
        DeletedItems = "deleteditems",
        /// Junk e-mail.
        JunkEmail = "junkemail",
        /// The outbox.
        Outbox = "outbox",
        /// The one-click "Archive" folder of the primary mailbox. Only newer
        /// servers know this name; older ones reject requests using it with
        /// a schema validation fault.
        Archive = "archive",
        /// Root of the IPM subtree: the parent of the visible folders.
        MsgFolderRoot = "msgfolderroot",
        /// Root of the mailbox, including non-IPM folders.
        Root = "root",
        /// The default calendar.
        Calendar = "calendar",
        /// The default contacts folder.
        Contacts = "contacts",
        /// Tasks.
        Tasks = "tasks",
        /// Notes.
        Notes = "notes",
        /// Journal.
        Journal = "journal",
        /// Search folders.
        SearchFolders = "searchfolders",
        /// Conversation history (Skype / Lync).
        ConversationHistory = "conversationhistory",
        /// Sync issues.
        SyncIssues = "syncissues",
        /// Recoverable items: deletions.
        RecoverableItemsDeletions = "recoverableitemsdeletions",
        /// Root of the online archive mailbox.
        ArchiveRoot = "archiveroot",
        /// IPM subtree root of the online archive mailbox.
        ArchiveMsgFolderRoot = "archivemsgfolderroot",
        /// Inbox of the online archive mailbox.
        ArchiveInbox = "archiveinbox",
        /// Deleted items of the online archive mailbox.
        ArchiveDeletedItems = "archivedeleteditems",
    }
}

/// A reference to a folder: a folder id, or a distinguished folder of the
/// own (or another) mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FolderRef {
    /// A concrete folder.
    Id(FolderId),
    /// A distinguished folder.
    Distinguished {
        /// Which folder.
        folder: DistinguishedFolder,
        /// SMTP address of another mailbox (delegate access); `None` for
        /// the mailbox the client acts for.
        mailbox: Option<String>,
    },
}

impl FolderRef {
    /// A distinguished folder of another mailbox.
    pub fn distinguished_in(folder: DistinguishedFolder, mailbox: impl Into<String>) -> Self {
        FolderRef::Distinguished {
            folder,
            mailbox: Some(mailbox.into()),
        }
    }

    pub(crate) fn write(&self, w: &mut XmlWriter) {
        match self {
            FolderRef::Id(id) => id.write(w, "t:FolderId"),
            FolderRef::Distinguished {
                folder,
                mailbox: None,
            } => {
                w.empty("t:DistinguishedFolderId", &[("Id", folder.as_str())]);
            }
            FolderRef::Distinguished {
                folder,
                mailbox: Some(mailbox),
            } => {
                w.start("t:DistinguishedFolderId", &[("Id", folder.as_str())])
                    .start("t:Mailbox", &[])
                    .text_element("t:EmailAddress", mailbox)
                    .end("t:Mailbox")
                    .end("t:DistinguishedFolderId");
            }
        }
    }
}

impl From<FolderId> for FolderRef {
    fn from(value: FolderId) -> Self {
        FolderRef::Id(value)
    }
}

impl From<&FolderId> for FolderRef {
    fn from(value: &FolderId) -> Self {
        FolderRef::Id(value.clone())
    }
}

impl From<DistinguishedFolder> for FolderRef {
    fn from(folder: DistinguishedFolder) -> Self {
        FolderRef::Distinguished {
            folder,
            mailbox: None,
        }
    }
}

/// A reference to an item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemRef {
    /// A concrete item (also the id of a calendar occurrence returned by a
    /// calendar view).
    Id(ItemId),
    /// The occurrence with the given one-based index of a recurring series.
    Occurrence {
        /// Id of the series master.
        master: ItemId,
        /// One-based index of the occurrence.
        index: u32,
    },
    /// The series master of a recurring occurrence or exception.
    MasterOf(ItemId),
}

impl ItemRef {
    pub(crate) fn write(&self, w: &mut XmlWriter) {
        match self {
            ItemRef::Id(id) => id.write(w, "t:ItemId"),
            ItemRef::Occurrence { master, index } => {
                let index = index.to_string();
                let mut attrs = vec![("RecurringMasterId", master.id.as_str())];
                if let Some(ck) = &master.change_key {
                    attrs.push(("ChangeKey", ck.as_str()));
                }
                attrs.push(("InstanceIndex", index.as_str()));
                w.empty("t:OccurrenceItemId", &attrs);
            }
            ItemRef::MasterOf(occurrence) => {
                let mut attrs = vec![("OccurrenceId", occurrence.id.as_str())];
                if let Some(ck) = &occurrence.change_key {
                    attrs.push(("ChangeKey", ck.as_str()));
                }
                w.empty("t:RecurringMasterItemId", &attrs);
            }
        }
    }
}

impl From<ItemId> for ItemRef {
    fn from(value: ItemId) -> Self {
        ItemRef::Id(value)
    }
}

impl From<&ItemId> for ItemRef {
    fn from(value: &ItemId) -> Self {
        ItemRef::Id(value.clone())
    }
}

/// A mailbox: an e-mail address with display name.
///
/// For senders inside the organization Exchange usually reports the SMTP
/// address; when it cannot, `routing_type` is `EX` and `email` holds the
/// legacy Exchange DN.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The address (`EmailAddress`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// `SMTP`, `EX`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_type: Option<String>,
    /// `Mailbox`, `PublicDL`, `PrivateDL`, `Contact`, `OneOff`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox_type: Option<String>,
}

impl Mailbox {
    /// A mailbox with only an address.
    pub fn new(email: impl Into<String>) -> Self {
        Mailbox {
            email: Some(email.into()),
            ..Mailbox::default()
        }
    }

    /// A mailbox with display name and address.
    pub fn with_name(name: impl Into<String>, email: impl Into<String>) -> Self {
        Mailbox {
            name: Some(name.into()),
            email: Some(email.into()),
            ..Mailbox::default()
        }
    }

    /// The SMTP address, if the address is one.
    pub fn smtp_address(&self) -> Option<&str> {
        let email = self.email.as_deref()?;
        match self.routing_type.as_deref() {
            None => email.contains('@').then_some(email),
            Some(rt) if rt.eq_ignore_ascii_case("SMTP") => Some(email),
            Some(_) => None,
        }
    }

    /// Reads a `Mailbox` element (or any element of `EmailAddressType`).
    pub(crate) fn from_element(element: &Element) -> Self {
        Mailbox {
            name: element.child_text("Name"),
            email: element.child_text("EmailAddress"),
            routing_type: element.child_text("RoutingType"),
            mailbox_type: element.child_text("MailboxType"),
        }
    }

    /// Reads the `Mailbox` child of a single-recipient element (`From`,
    /// `Sender`, `Organizer`).
    pub(crate) fn from_single(element: &Element) -> Option<Self> {
        element.child("Mailbox").map(Mailbox::from_element)
    }

    /// Reads the `Mailbox` children of a recipient list (`ToRecipients`).
    pub(crate) fn list(element: Option<&Element>) -> Vec<Self> {
        element
            .map(|list| {
                list.children_named("Mailbox")
                    .map(Mailbox::from_element)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn write(&self, w: &mut XmlWriter) {
        w.start("t:Mailbox", &[]);
        if let Some(name) = &self.name {
            w.text_element("t:Name", name);
        }
        if let Some(email) = &self.email {
            w.text_element("t:EmailAddress", email);
        }
        if let Some(routing_type) = &self.routing_type {
            w.text_element("t:RoutingType", routing_type);
        }
        w.end("t:Mailbox");
    }
}

impl fmt::Display for Mailbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.name, &self.email) {
            (Some(name), Some(email)) => write!(f, "{name} <{email}>"),
            (Some(name), None) => f.write_str(name),
            (None, Some(email)) => f.write_str(email),
            (None, None) => Ok(()),
        }
    }
}

wire_enum! {
    /// Format of a body.
    pub enum BodyType {
        /// HTML.
        Html = "HTML",
        /// Plain text.
        Text = "Text",
        /// Whatever the item was stored as (only for requesting).
        Best = "Best",
    }
}

/// An item body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    /// HTML or text.
    pub body_type: BodyType,
    /// The content, verbatim.
    pub content: String,
    /// Whether the server truncated the content.
    #[serde(default)]
    pub is_truncated: bool,
}

impl Body {
    /// A plain-text body.
    pub fn text(content: impl Into<String>) -> Self {
        Body {
            body_type: BodyType::Text,
            content: content.into(),
            is_truncated: false,
        }
    }

    /// An HTML body.
    pub fn html(content: impl Into<String>) -> Self {
        Body {
            body_type: BodyType::Html,
            content: content.into(),
            is_truncated: false,
        }
    }

    pub(crate) fn from_element(element: &Element) -> Self {
        Body {
            body_type: element
                .attr("BodyType")
                .and_then(BodyType::parse)
                .unwrap_or(BodyType::Text),
            content: element.text(),
            is_truncated: element
                .attr("IsTruncated")
                .and_then(crate::xml::parse_bool)
                .unwrap_or(false),
        }
    }

    pub(crate) fn write(&self, w: &mut XmlWriter) {
        let body_type = match self.body_type {
            BodyType::Html => "HTML",
            BodyType::Text | BodyType::Best => "Text",
        };
        w.text_element_with("t:Body", &[("BodyType", body_type)], &self.content);
    }
}

wire_enum! {
    /// Importance of an item.
    pub enum Importance {
        /// Low.
        Low = "Low",
        /// Normal.
        Normal = "Normal",
        /// High.
        High = "High",
    }
}

wire_enum! {
    /// Sensitivity of an item.
    pub enum Sensitivity {
        /// Normal.
        Normal = "Normal",
        /// Personal.
        Personal = "Personal",
        /// Private.
        Private = "Private",
        /// Confidential.
        Confidential = "Confidential",
    }
}

wire_enum! {
    /// How many properties a response returns before additional ones are
    /// added.
    pub enum BaseShape {
        /// Only ids.
        IdOnly = "IdOnly",
        /// A default set of properties.
        Default = "Default",
        /// All properties the operation can return.
        AllProperties = "AllProperties",
    }
}

wire_enum! {
    /// Element name of an item, i.e. its EWS item type.
    pub enum ItemKind {
        /// A generic item.
        Item = "Item",
        /// An e-mail message.
        Message = "Message",
        /// A meeting message without a more specific type.
        MeetingMessage = "MeetingMessage",
        /// A meeting request (invitation).
        MeetingRequest = "MeetingRequest",
        /// A reply to a meeting request.
        MeetingResponse = "MeetingResponse",
        /// A meeting cancellation.
        MeetingCancellation = "MeetingCancellation",
        /// An appointment or meeting.
        CalendarItem = "CalendarItem",
        /// A contact.
        Contact = "Contact",
        /// A contact group.
        DistributionList = "DistributionList",
        /// A task.
        Task = "Task",
        /// A post in a public folder.
        PostItem = "PostItem",
    }
}

impl ItemKind {
    /// True for the meeting message kinds.
    pub fn is_meeting_message(self) -> bool {
        matches!(
            self,
            ItemKind::MeetingMessage
                | ItemKind::MeetingRequest
                | ItemKind::MeetingResponse
                | ItemKind::MeetingCancellation
        )
    }

    /// True for kinds that are mail messages in a mail folder (including
    /// meeting messages).
    pub fn is_mail(self) -> bool {
        matches!(self, ItemKind::Message | ItemKind::PostItem) || self.is_meeting_message()
    }
}

/// A property to request, order by or change.
///
/// ```
/// use envelope_ews::PropertyPath;
/// let subject = PropertyPath::field("item:Subject");
/// let flag_status = PropertyPath::extended_tag(0x1090, "Integer");
/// # let _ = (subject, flag_status);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PropertyPath {
    /// An unindexed property such as `item:Subject` or `message:IsRead`.
    Field(String),
    /// An indexed property such as `contacts:EmailAddress` with index
    /// `EmailAddress1`.
    Indexed {
        /// The field URI.
        uri: String,
        /// The index.
        index: String,
    },
    /// A MAPI property by tag (`0x1090`) and type (`Integer`, `String`, ...).
    ExtendedTag {
        /// The property tag (identifier part).
        tag: u16,
        /// The EWS `MapiPropertyTypeType` name.
        property_type: String,
    },
}

impl PropertyPath {
    /// An unindexed property.
    pub fn field(uri: impl Into<String>) -> Self {
        PropertyPath::Field(uri.into())
    }

    /// An indexed property.
    pub fn indexed(uri: impl Into<String>, index: impl Into<String>) -> Self {
        PropertyPath::Indexed {
            uri: uri.into(),
            index: index.into(),
        }
    }

    /// A MAPI property by tag.
    pub fn extended_tag(tag: u16, property_type: impl Into<String>) -> Self {
        PropertyPath::ExtendedTag {
            tag,
            property_type: property_type.into(),
        }
    }

    pub(crate) fn write(&self, w: &mut XmlWriter) {
        match self {
            PropertyPath::Field(uri) => {
                w.empty("t:FieldURI", &[("FieldURI", uri)]);
            }
            PropertyPath::Indexed { uri, index } => {
                w.empty(
                    "t:IndexedFieldURI",
                    &[("FieldURI", uri), ("FieldIndex", index)],
                );
            }
            PropertyPath::ExtendedTag { tag, property_type } => {
                let tag = format!("0x{tag:04x}");
                w.empty(
                    "t:ExtendedFieldURI",
                    &[("PropertyTag", &tag), ("PropertyType", property_type)],
                );
            }
        }
    }
}

/// What a request for items returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemShape {
    /// The base set of properties.
    pub base: BaseShape,
    /// Whether to include the full MIME content (`MimeContent`).
    pub include_mime_content: bool,
    /// Format of `Body`, when the body is requested.
    pub body_type: Option<BodyType>,
    /// Properties returned in addition to the base shape.
    pub additional: Vec<PropertyPath>,
}

impl ItemShape {
    /// Only ids.
    pub fn id_only() -> Self {
        ItemShape {
            base: BaseShape::IdOnly,
            include_mime_content: false,
            body_type: None,
            additional: Vec::new(),
        }
    }

    /// Ids plus the given properties.
    pub fn with_properties(additional: Vec<PropertyPath>) -> Self {
        ItemShape {
            additional,
            ..ItemShape::id_only()
        }
    }

    pub(crate) fn write(&self, w: &mut XmlWriter, tag: &str) {
        w.start(tag, &[])
            .text_element("t:BaseShape", self.base.as_str());
        if self.include_mime_content {
            w.text_element("t:IncludeMimeContent", "true");
        }
        if let Some(body_type) = self.body_type {
            w.text_element("t:BodyType", body_type.as_str());
        }
        write_additional(w, &self.additional);
        w.end(tag);
    }
}

pub(crate) fn write_additional(w: &mut XmlWriter, additional: &[PropertyPath]) {
    if additional.is_empty() {
        return;
    }
    w.start("t:AdditionalProperties", &[]);
    for path in additional {
        path.write(w);
    }
    w.end("t:AdditionalProperties");
}

/// Value of a MAPI property in the `ExtendedProperty` children of an item.
pub(crate) fn extended_tag_value(item: &Element, tag: u16) -> Option<String> {
    item.children_named("ExtendedProperty").find_map(|prop| {
        let uri = prop.child("ExtendedFieldURI")?;
        let raw = uri.attr("PropertyTag")?.trim();
        let parsed = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => raw.parse::<u32>().ok(),
        }?;
        // Some servers report the full tag (identifier and type).
        let id = if parsed > 0xFFFF {
            parsed >> 16
        } else {
            parsed
        };
        (id == u32::from(tag)).then(|| prop.child_text("Value"))?
    })
}

/// Parses an `xs:dateTime`. Values without offset are taken as UTC (the
/// client does not send a `TimeZoneContext` unless configured to).
pub fn parse_datetime(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|naive| naive.and_utc())
}

/// Formats an instant as `xs:dateTime` in UTC (`2024-05-01T08:30:00Z`).
pub fn format_datetime(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parses an `xs:date`, ignoring a trailing zone (`2024-05-01Z`,
/// `2024-05-01+02:00`).
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    let date = text.get(..10)?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

pub(crate) fn child_datetime(element: &Element, name: &str) -> Option<DateTime<Utc>> {
    element.child_text(name).and_then(|t| parse_datetime(&t))
}

/// Text of every `String` child of a list element (`Categories`).
pub(crate) fn string_list(element: Option<&Element>) -> Vec<String> {
    element
        .map(|list| {
            list.children_named("String")
                .filter_map(Element::trimmed_text)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;
    use chrono::TimeZone;

    fn written(f: impl FnOnce(&mut XmlWriter)) -> String {
        let mut w = XmlWriter::new();
        f(&mut w);
        w.into_string()
    }

    #[test]
    fn ids_read_and_write() {
        let el = xml::parse(r#"<t:ItemId xmlns:t="x" Id="AAMk=" ChangeKey="CQAA"/>"#).unwrap();
        let id = ItemId::from_element(&el).unwrap();
        assert_eq!(id, ItemId::with_change_key("AAMk=", "CQAA"));
        assert_eq!(
            written(|w| id.write(w, "t:ItemId")),
            r#"<t:ItemId Id="AAMk=" ChangeKey="CQAA"/>"#
        );
        let el = xml::parse(r#"<FolderId Id="F" ChangeKey=""/>"#).unwrap();
        assert_eq!(FolderId::from_element(&el), Some(FolderId::new("F")));
        assert_eq!(
            ItemId::from_element(&xml::parse("<ItemId/>").unwrap()),
            None
        );
    }

    #[test]
    fn folder_refs() {
        assert_eq!(
            written(|w| FolderRef::from(DistinguishedFolder::Inbox).write(w)),
            r#"<t:DistinguishedFolderId Id="inbox"/>"#
        );
        assert_eq!(
            written(|w| FolderRef::distinguished_in(
                DistinguishedFolder::Calendar,
                "boss@example.com"
            )
            .write(w)),
            r#"<t:DistinguishedFolderId Id="calendar"><t:Mailbox><t:EmailAddress>boss@example.com</t:EmailAddress></t:Mailbox></t:DistinguishedFolderId>"#
        );
        assert_eq!(
            written(|w| FolderRef::from(FolderId::with_change_key("a&b", "c")).write(w)),
            r#"<t:FolderId Id="a&amp;b" ChangeKey="c"/>"#
        );
        assert_eq!(
            DistinguishedFolder::parse("SentItems"),
            Some(DistinguishedFolder::SentItems)
        );
        assert_eq!(DistinguishedFolder::JunkEmail.to_string(), "junkemail");
    }

    #[test]
    fn item_refs() {
        let master = ItemId::with_change_key("M", "K");
        assert_eq!(
            written(|w| ItemRef::Occurrence {
                master: master.clone(),
                index: 3
            }
            .write(w)),
            r#"<t:OccurrenceItemId RecurringMasterId="M" ChangeKey="K" InstanceIndex="3"/>"#
        );
        assert_eq!(
            written(|w| ItemRef::MasterOf(ItemId::new("O")).write(w)),
            r#"<t:RecurringMasterItemId OccurrenceId="O"/>"#
        );
    }

    #[test]
    fn mailboxes() {
        let el = xml::parse(
            "<From><Mailbox><Name>Jane Doe</Name><EmailAddress>jane@example.com</EmailAddress><RoutingType>SMTP</RoutingType><MailboxType>Mailbox</MailboxType></Mailbox></From>",
        )
        .unwrap();
        let mailbox = Mailbox::from_single(&el).unwrap();
        assert_eq!(mailbox.smtp_address(), Some("jane@example.com"));
        assert_eq!(mailbox.to_string(), "Jane Doe <jane@example.com>");
        let ex = Mailbox {
            email: Some("/O=EXAMPLE/OU=EXCHANGE ADMINISTRATIVE GROUP/CN=RECIPIENTS/CN=JANE".into()),
            routing_type: Some("EX".into()),
            ..Mailbox::default()
        };
        assert_eq!(ex.smtp_address(), None);
        assert_eq!(
            written(|w| Mailbox::with_name("A & B", "ab@example.com").write(w)),
            "<t:Mailbox><t:Name>A &amp; B</t:Name><t:EmailAddress>ab@example.com</t:EmailAddress></t:Mailbox>"
        );
    }

    #[test]
    fn property_paths_and_shapes() {
        let shape = ItemShape {
            base: BaseShape::IdOnly,
            include_mime_content: true,
            body_type: Some(BodyType::Text),
            additional: vec![
                PropertyPath::field("item:Subject"),
                PropertyPath::indexed("contacts:EmailAddress", "EmailAddress1"),
                PropertyPath::extended_tag(0x1090, "Integer"),
            ],
        };
        assert_eq!(
            written(|w| shape.write(w, "m:ItemShape")),
            concat!(
                "<m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:IncludeMimeContent>true</t:IncludeMimeContent>",
                "<t:BodyType>Text</t:BodyType><t:AdditionalProperties><t:FieldURI FieldURI=\"item:Subject\"/>",
                "<t:IndexedFieldURI FieldURI=\"contacts:EmailAddress\" FieldIndex=\"EmailAddress1\"/>",
                "<t:ExtendedFieldURI PropertyTag=\"0x1090\" PropertyType=\"Integer\"/></t:AdditionalProperties></m:ItemShape>"
            )
        );
        assert_eq!(
            written(|w| ItemShape::id_only().write(w, "m:ItemShape")),
            "<m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape>"
        );
    }

    #[test]
    fn extended_properties_by_tag() {
        let item = xml::parse(
            r#"<Message>
                 <ExtendedProperty><ExtendedFieldURI PropertyTag="0x1090" PropertyType="Integer"/><Value>2</Value></ExtendedProperty>
                 <ExtendedProperty><ExtendedFieldURI PropertyTag="4245" PropertyType="Integer"/><Value>6</Value></ExtendedProperty>
               </Message>"#,
        )
        .unwrap();
        assert_eq!(extended_tag_value(&item, 0x1090).as_deref(), Some("2"));
        assert_eq!(extended_tag_value(&item, 0x1095).as_deref(), Some("6"));
        assert_eq!(extended_tag_value(&item, 0x0E07), None);
    }

    #[test]
    fn dates() {
        let expected = Utc.with_ymd_and_hms(2024, 5, 1, 8, 30, 0).single().unwrap();
        assert_eq!(parse_datetime("2024-05-01T08:30:00Z"), Some(expected));
        assert_eq!(parse_datetime("2024-05-01T10:30:00+02:00"), Some(expected));
        assert_eq!(parse_datetime(" 2024-05-01T08:30:00 "), Some(expected));
        assert_eq!(
            parse_datetime("2024-05-01T08:30:00.250Z").map(|d| d.timestamp_subsec_millis()),
            Some(250)
        );
        assert_eq!(parse_datetime("yesterday"), None);
        assert_eq!(format_datetime(expected), "2024-05-01T08:30:00Z");
        let day = NaiveDate::from_ymd_opt(2024, 5, 1);
        assert_eq!(parse_date("2024-05-01Z"), day);
        assert_eq!(parse_date("2024-05-01+02:00"), day);
        assert_eq!(parse_date("2024-05-01"), day);
        assert_eq!(parse_date("2024-5-1"), None);
    }

    #[test]
    fn bodies() {
        let el =
            xml::parse(r#"<Body BodyType="HTML" IsTruncated="true">&lt;p&gt;Hi&lt;/p&gt;</Body>"#)
                .unwrap();
        let body = Body::from_element(&el);
        assert_eq!(body.body_type, BodyType::Html);
        assert_eq!(body.content, "<p>Hi</p>");
        assert!(body.is_truncated);
        assert_eq!(
            written(|w| Body::text("a < b").write(w)),
            r#"<t:Body BodyType="Text">a &lt; b</t:Body>"#
        );
    }
}

//! Operations shared by every item type: paged `FindItem`, batched
//! `GetItem`, `SyncFolderItems`, `UpdateItem` building blocks, and moving,
//! copying and deleting items.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseError};
use crate::folders::DeleteType;
use crate::messages::Message;
use crate::soap;
use crate::types::{FolderRef, ItemId, ItemRef, ItemShape, PropertyPath, format_datetime};
use crate::xml::{Element, XmlWriter, parse_bool};

/// Largest `MaxChangesReturned` Exchange accepts for `SyncFolderItems`.
pub const MAX_SYNC_CHANGES: u32 = 512;

/// Ids per `GetItem` request when no MIME content is fetched.
pub const GET_ITEM_BATCH_SIZE: usize = 50;

/// Ids per `GetItem` request when MIME content is fetched.
pub const GET_ITEM_MIME_BATCH_SIZE: usize = 10;

/// Items per `UpdateItem`, `MoveItem`, `CopyItem` and `DeleteItem` request.
pub const WRITE_BATCH_SIZE: usize = 100;

/// One page of a `FindItem` query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPage<T> {
    /// The items of this page.
    pub items: Vec<T>,
    /// Offset of the next page; `None` after the last page.
    pub next_offset: Option<u32>,
    /// Number of items matching the query.
    pub total_items_in_view: Option<u32>,
    /// Whether this page ends the result set.
    pub includes_last_item_in_range: bool,
}

/// One change reported by `SyncFolderItems`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ItemChange {
    /// An item appeared in the folder (created, or moved in).
    Created(Message),
    /// An item changed.
    Updated(Message),
    /// An item left the folder (deleted, or moved out).
    Deleted {
        /// The item.
        id: ItemId,
    },
    /// Only the read state changed.
    ReadFlagChanged {
        /// The item.
        id: ItemId,
        /// The new read state.
        is_read: bool,
    },
}

/// One round of `SyncFolderItems`.
///
/// The changed items are parsed as [`Message`]s whatever their type; in
/// calendar and contact folders only the base item properties are filled,
/// and [`Message::kind`] tells what to fetch the details with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSyncPage {
    /// The state to send with the next call.
    pub sync_state: String,
    /// False when more changes are pending: call again with `sync_state`.
    pub includes_last_item_in_range: bool,
    /// The changes, in server order.
    pub changes: Vec<ItemChange>,
}

/// Options for [`EwsClient::sync_folder_items`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOptions {
    /// Changes per round, 1 to [`MAX_SYNC_CHANGES`] (default 512).
    pub max_changes: u32,
    /// Properties returned with created and updated items in addition to
    /// their ids (default: item class, read state and receive time).
    pub properties: Vec<PropertyPath>,
    /// Item ids whose changes are not reported (changes the client made
    /// itself).
    pub ignore: Vec<ItemId>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            max_changes: MAX_SYNC_CHANGES,
            properties: vec![
                PropertyPath::field("item:ItemClass"),
                PropertyPath::field("item:DateTimeReceived"),
            ],
            ignore: Vec::new(),
        }
    }
}

/// The view of a `FindItem` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItemView {
    /// Paging by offset from the beginning.
    Indexed { offset: u32, max: u32 },
    /// Expanded calendar occurrences between two instants.
    Calendar {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        max: u32,
    },
}

/// A `FindItem` request.
#[derive(Debug, Clone)]
pub(crate) struct FindItemQuery<'a> {
    pub(crate) shape: &'a ItemShape,
    pub(crate) view: ItemView,
    /// A rendered `Restriction` (the element itself).
    pub(crate) restriction: Option<String>,
    /// Sort field and direction (`true` = descending).
    pub(crate) sort: Option<(&'a str, bool)>,
    pub(crate) folders: &'a [FolderRef],
    /// An AQS query (`QueryString`).
    pub(crate) query: Option<&'a str>,
}

pub(crate) fn find_item_request(query: &FindItemQuery<'_>) -> String {
    let mut w = XmlWriter::new();
    w.start("m:FindItem", &[("Traversal", "Shallow")]);
    query.shape.write(&mut w, "m:ItemShape");
    match &query.view {
        ItemView::Indexed { offset, max } => {
            w.empty(
                "m:IndexedPageItemView",
                &[
                    ("MaxEntriesReturned", &max.to_string()),
                    ("Offset", &offset.to_string()),
                    ("BasePoint", "Beginning"),
                ],
            );
        }
        ItemView::Calendar { start, end, max } => {
            w.empty(
                "m:CalendarView",
                &[
                    ("MaxEntriesReturned", &max.to_string()),
                    ("StartDate", &format_datetime(*start)),
                    ("EndDate", &format_datetime(*end)),
                ],
            );
        }
    }
    if let Some(restriction) = &query.restriction {
        w.raw(restriction);
    }
    if let Some((field, descending)) = query.sort {
        w.start("m:SortOrder", &[])
            .start(
                "t:FieldOrder",
                &[(
                    "Order",
                    if descending {
                        "Descending"
                    } else {
                        "Ascending"
                    },
                )],
            )
            .empty("t:FieldURI", &[("FieldURI", field)])
            .end("t:FieldOrder")
            .end("m:SortOrder");
    }
    w.start("m:ParentFolderIds", &[]);
    for folder in query.folders {
        folder.write(&mut w);
    }
    w.end("m:ParentFolderIds");
    if let Some(aqs) = query.query {
        w.text_element("m:QueryString", aqs);
    }
    w.end("m:FindItem");
    w.into_string()
}

/// A `Restriction` matching items whose `field` is at or after `since`.
pub(crate) fn restriction_since(field: &str, since: DateTime<Utc>) -> String {
    let mut w = XmlWriter::new();
    w.start("m:Restriction", &[])
        .start("t:IsGreaterThanOrEqualTo", &[])
        .empty("t:FieldURI", &[("FieldURI", field)])
        .start("t:FieldURIOrConstant", &[])
        .empty("t:Constant", &[("Value", &format_datetime(since))])
        .end("t:FieldURIOrConstant")
        .end("t:IsGreaterThanOrEqualTo")
        .end("m:Restriction");
    w.into_string()
}

/// Parses the `RootFolder` of a `FindItem` response message.
pub(crate) fn parse_find_item<T>(
    message: &Element,
    parse: impl Fn(&Element) -> Option<T>,
) -> Result<ItemPage<T>, EwsError> {
    let root = message
        .child("RootFolder")
        .ok_or_else(|| EwsError::InvalidResponse("FindItem response without RootFolder".into()))?;
    let items: Vec<T> = root
        .child("Items")
        .map(|items| items.elements().filter_map(&parse).collect())
        .unwrap_or_default();
    let includes_last = root
        .attr("IncludesLastItemInRange")
        .and_then(parse_bool)
        .unwrap_or(true);
    let next_offset = if includes_last {
        None
    } else {
        root.attr("IndexedPagingOffset")
            .and_then(|v| v.trim().parse().ok())
    };
    Ok(ItemPage {
        items,
        next_offset,
        total_items_in_view: root
            .attr("TotalItemsInView")
            .and_then(|v| v.trim().parse().ok()),
        includes_last_item_in_range: includes_last,
    })
}

pub(crate) fn get_item_request(ids: &[ItemRef], shape: &ItemShape) -> String {
    let mut w = XmlWriter::new();
    w.start("m:GetItem", &[]);
    shape.write(&mut w, "m:ItemShape");
    w.start("m:ItemIds", &[]);
    for id in ids {
        id.write(&mut w);
    }
    w.end("m:ItemIds").end("m:GetItem");
    w.into_string()
}

/// The first item element inside the `Items` child of a response message.
pub(crate) fn first_item(message: &Element) -> Option<&Element> {
    message.child("Items")?.elements().next()
}

/// The id of the first item inside `Items`.
pub(crate) fn first_item_id(message: &Element) -> Option<ItemId> {
    first_item(message)?
        .child("ItemId")
        .and_then(ItemId::from_element)
}

pub(crate) fn sync_folder_items_request(
    folder: &FolderRef,
    sync_state: Option<&str>,
    options: &SyncOptions,
) -> String {
    let mut w = XmlWriter::new();
    w.start("m:SyncFolderItems", &[]);
    ItemShape::with_properties(options.properties.clone()).write(&mut w, "m:ItemShape");
    w.start("m:SyncFolderId", &[]);
    folder.write(&mut w);
    w.end("m:SyncFolderId");
    if let Some(state) = sync_state.filter(|s| !s.is_empty()) {
        w.text_element("m:SyncState", state);
    }
    if !options.ignore.is_empty() {
        w.start("m:Ignore", &[]);
        for id in &options.ignore {
            id.write(&mut w, "t:ItemId");
        }
        w.end("m:Ignore");
    }
    let max = options.max_changes.clamp(1, MAX_SYNC_CHANGES);
    w.text_element("m:MaxChangesReturned", &max.to_string());
    w.end("m:SyncFolderItems");
    w.into_string()
}

pub(crate) fn parse_sync_folder_items(message: &Element) -> Result<ItemSyncPage, EwsError> {
    let sync_state = message.child_text("SyncState").ok_or_else(|| {
        EwsError::InvalidResponse("SyncFolderItems response without SyncState".into())
    })?;
    let mut changes = Vec::new();
    if let Some(list) = message.child("Changes") {
        for change in list.elements() {
            match change.name.as_str() {
                "Create" | "Update" => {
                    let Some(item) = change.elements().find_map(Message::from_element) else {
                        continue;
                    };
                    changes.push(if change.is("Create") {
                        ItemChange::Created(item)
                    } else {
                        ItemChange::Updated(item)
                    });
                }
                "Delete" => {
                    if let Some(id) = change.child("ItemId").and_then(ItemId::from_element) {
                        changes.push(ItemChange::Deleted { id });
                    }
                }
                "ReadFlagChange" => {
                    let id = change.child("ItemId").and_then(ItemId::from_element);
                    let is_read = change.child_bool("IsRead");
                    if let (Some(id), Some(is_read)) = (id, is_read) {
                        changes.push(ItemChange::ReadFlagChanged { id, is_read });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(ItemSyncPage {
        sync_state,
        includes_last_item_in_range: message
            .child_bool("IncludesLastItemInRange")
            .unwrap_or(true),
        changes,
    })
}

/// `MoveItem` / `CopyItem`.
pub(crate) fn move_copy_request(
    operation: &str,
    ids: &[ItemId],
    to: &FolderRef,
    return_new_ids: bool,
) -> String {
    let tag = format!("m:{operation}");
    let mut w = XmlWriter::new();
    w.start(&tag, &[]).start("m:ToFolderId", &[]);
    to.write(&mut w);
    w.end("m:ToFolderId").start("m:ItemIds", &[]);
    for id in ids {
        id.write(&mut w, "t:ItemId");
    }
    w.end("m:ItemIds");
    if return_new_ids {
        w.text_element("m:ReturnNewItemIds", "true");
    }
    w.end(&tag);
    w.into_string()
}

/// How deleting calendar items notifies attendees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SendCancellations {
    /// Notify nobody.
    SendToNone,
    /// Send cancellations without keeping a copy.
    SendOnlyToAll,
    /// Send cancellations and keep a copy in sent items.
    SendToAllAndSaveCopy,
}

impl SendCancellations {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SendCancellations::SendToNone => "SendToNone",
            SendCancellations::SendOnlyToAll => "SendOnlyToAll",
            SendCancellations::SendToAllAndSaveCopy => "SendToAllAndSaveCopy",
        }
    }
}

pub(crate) fn delete_item_request(
    ids: &[ItemRef],
    delete_type: DeleteType,
    cancellations: SendCancellations,
    suppress_read_receipts: bool,
) -> String {
    let mut attrs = vec![
        ("DeleteType", delete_type.as_str()),
        ("SendMeetingCancellations", cancellations.as_str()),
        ("AffectedTaskOccurrences", "AllOccurrences"),
    ];
    if suppress_read_receipts {
        attrs.push(("SuppressReadReceipts", "true"));
    }
    let mut w = XmlWriter::new();
    w.start("m:DeleteItem", &attrs).start("m:ItemIds", &[]);
    for id in ids {
        id.write(&mut w);
    }
    w.end("m:ItemIds").end("m:DeleteItem");
    w.into_string()
}

/// A single property change inside an `ItemChange`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldUpdate {
    /// `SetItemField`: the path and the item element carrying the value
    /// (`<t:Message><t:IsRead>true</t:IsRead></t:Message>`).
    Set {
        path: PropertyPath,
        value_xml: String,
    },
    /// `DeleteItemField`.
    Delete(PropertyPath),
}

impl FieldUpdate {
    pub(crate) fn set(path: PropertyPath, value_xml: String) -> Self {
        FieldUpdate::Set { path, value_xml }
    }

    fn write(&self, w: &mut XmlWriter) {
        match self {
            FieldUpdate::Set { path, value_xml } => {
                w.start("t:SetItemField", &[]);
                path.write(w);
                w.raw(value_xml).end("t:SetItemField");
            }
            FieldUpdate::Delete(path) => {
                w.start("t:DeleteItemField", &[]);
                path.write(w);
                w.end("t:DeleteItemField");
            }
        }
    }
}

/// Attributes of an `UpdateItem` request.
#[derive(Debug, Clone, Default)]
pub(crate) struct UpdateAttributes<'a> {
    pub(crate) message_disposition: Option<&'a str>,
    pub(crate) send_invitations: Option<&'a str>,
    pub(crate) suppress_read_receipts: bool,
}

pub(crate) fn update_item_request(
    changes: &[(ItemRef, Vec<FieldUpdate>)],
    attributes: &UpdateAttributes<'_>,
) -> String {
    let mut attrs = Vec::new();
    if let Some(disposition) = attributes.message_disposition {
        attrs.push(("MessageDisposition", disposition));
    }
    attrs.push(("ConflictResolution", "AlwaysOverwrite"));
    if let Some(send) = attributes.send_invitations {
        attrs.push(("SendMeetingInvitationsOrCancellations", send));
    }
    if attributes.suppress_read_receipts {
        attrs.push(("SuppressReadReceipts", "true"));
    }
    let mut w = XmlWriter::new();
    w.start("m:UpdateItem", &attrs).start("m:ItemChanges", &[]);
    for (id, updates) in changes {
        w.start("t:ItemChange", &[]);
        id.write(&mut w);
        w.start("t:Updates", &[]);
        for update in updates {
            update.write(&mut w);
        }
        w.end("t:Updates").end("t:ItemChange");
    }
    w.end("m:ItemChanges").end("m:UpdateItem");
    w.into_string()
}

/// The `<t:{element}><t:{field}>text</t:{field}></t:{element}>` value of a
/// `SetItemField`.
pub(crate) fn value_xml(element: &str, field: &str, text: &str) -> String {
    let mut w = XmlWriter::new();
    let outer = format!("t:{element}");
    let inner = format!("t:{field}");
    w.start(&outer, &[]).text_element(&inner, text).end(&outer);
    w.into_string()
}

impl EwsClient {
    /// Fetches items with the given shape in batches of `batch_size`,
    /// returning one result per id, in order.
    pub(crate) async fn get_items_with<T>(
        &self,
        ids: &[ItemRef],
        shape: &ItemShape,
        batch_size: usize,
        parse: impl Fn(&Element) -> Option<T>,
    ) -> Result<Vec<Result<T, ResponseError>>, EwsError> {
        let mut results = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(batch_size.max(1)) {
            let request = SoapRequest::read("GetItem", get_item_request(chunk, shape));
            let response = self.call(&request).await?;
            let messages = soap::each(&response)?;
            if messages.len() != chunk.len() {
                return Err(EwsError::InvalidResponse(format!(
                    "GetItem returned {} responses for {} ids",
                    messages.len(),
                    chunk.len()
                )));
            }
            for message in messages {
                results.push(message.and_then(|message| {
                    first_item(message).and_then(&parse).ok_or_else(|| {
                        ResponseError::new(
                            crate::ResponseCode::Other("ErrorNoItemReturned".into()),
                            "GetItem returned no usable item",
                        )
                    })
                }));
            }
        }
        Ok(results)
    }

    /// Runs a paged `FindItem` query.
    pub(crate) async fn find_items_with<T>(
        &self,
        query: &FindItemQuery<'_>,
        parse: impl Fn(&Element) -> Option<T>,
    ) -> Result<ItemPage<T>, EwsError> {
        let request = SoapRequest::read("FindItem", find_item_request(query));
        let response = self.call(&request).await?;
        parse_find_item(soap::single(&response)?, parse)
    }

    /// One round of item synchronization for a folder (any folder type).
    /// Start without a state; repeat with the returned state while
    /// [`includes_last_item_in_range`](ItemSyncPage::includes_last_item_in_range)
    /// is false. The first rounds report every existing item as created.
    /// An [`EwsError::requires_resync`] error means the state expired.
    pub async fn sync_folder_items(
        &self,
        folder: impl Into<FolderRef>,
        sync_state: Option<&str>,
        options: &SyncOptions,
    ) -> Result<ItemSyncPage, EwsError> {
        let body = sync_folder_items_request(&folder.into(), sync_state, options);
        let response = self
            .call(&SoapRequest::read("SyncFolderItems", body))
            .await?;
        parse_sync_folder_items(soap::single(&response)?)
    }

    /// Moves items to another folder. Returns the new id of every item
    /// (`None` when the server does not report it, e.g. for moves into
    /// another mailbox).
    pub async fn move_items(
        &self,
        ids: &[ItemId],
        to: impl Into<FolderRef>,
    ) -> Result<Vec<Result<Option<ItemId>, ResponseError>>, EwsError> {
        self.move_or_copy("MoveItem", ids, &to.into()).await
    }

    /// Copies items to another folder and returns the ids of the copies.
    pub async fn copy_items(
        &self,
        ids: &[ItemId],
        to: impl Into<FolderRef>,
    ) -> Result<Vec<Result<Option<ItemId>, ResponseError>>, EwsError> {
        self.move_or_copy("CopyItem", ids, &to.into()).await
    }

    async fn move_or_copy(
        &self,
        operation: &'static str,
        ids: &[ItemId],
        to: &FolderRef,
    ) -> Result<Vec<Result<Option<ItemId>, ResponseError>>, EwsError> {
        let return_new_ids = self.version().has_2010_sp1_features();
        let mut results = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(WRITE_BATCH_SIZE) {
            let body = move_copy_request(operation, chunk, to, return_new_ids);
            let response = self.call(&SoapRequest::write(operation, body)).await?;
            results.extend(
                soap::each(&response)?
                    .into_iter()
                    .map(|r| r.map(first_item_id)),
            );
        }
        Ok(results)
    }

    /// Deletes items. For calendar items no cancellations are sent (see
    /// [`EwsClient::delete_calendar_item`]). An already deleted item
    /// reports `ErrorItemNotFound`.
    pub async fn delete_items(
        &self,
        ids: &[ItemId],
        delete_type: DeleteType,
    ) -> Result<Vec<Result<(), ResponseError>>, EwsError> {
        let suppress = self.version().has_2013_features();
        let mut results = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(WRITE_BATCH_SIZE) {
            let refs: Vec<ItemRef> = chunk.iter().map(ItemRef::from).collect();
            let body =
                delete_item_request(&refs, delete_type, SendCancellations::SendToNone, suppress);
            let response = self.call(&SoapRequest::write("DeleteItem", body)).await?;
            results.extend(soap::each(&response)?.into_iter().map(|r| r.map(drop)));
        }
        Ok(results)
    }

    /// Sends `UpdateItem` requests in batches; one result (the item's new
    /// id) per change.
    pub(crate) async fn update_items(
        &self,
        changes: &[(ItemRef, Vec<FieldUpdate>)],
        attributes: &UpdateAttributes<'_>,
    ) -> Result<Vec<Result<Option<ItemId>, ResponseError>>, EwsError> {
        let mut results = Vec::with_capacity(changes.len());
        for chunk in changes.chunks(WRITE_BATCH_SIZE) {
            let body = update_item_request(chunk, attributes);
            let response = self.call(&SoapRequest::write("UpdateItem", body)).await?;
            results.extend(
                soap::each(&response)?
                    .into_iter()
                    .map(|r| r.map(first_item_id)),
            );
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BaseShape, DistinguishedFolder, FolderId};
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
    fn find_item_request_order_and_content() {
        let shape = ItemShape::with_properties(vec![PropertyPath::field("item:Subject")]);
        let since = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).single().unwrap();
        let folders = [FolderRef::from(DistinguishedFolder::Inbox)];
        let body = find_item_request(&FindItemQuery {
            shape: &shape,
            view: ItemView::Indexed {
                offset: 100,
                max: 50,
            },
            restriction: Some(restriction_since("item:DateTimeReceived", since)),
            sort: Some(("item:DateTimeReceived", true)),
            folders: &folders,
            query: None,
        });
        let root = wrap(&body);
        let op = root.child("FindItem").unwrap();
        assert_eq!(op.attr("Traversal"), Some("Shallow"));
        let names: Vec<&str> = op.elements().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "ItemShape",
                "IndexedPageItemView",
                "Restriction",
                "SortOrder",
                "ParentFolderIds"
            ]
        );
        let view = op.child("IndexedPageItemView").unwrap();
        assert_eq!(view.attr("Offset"), Some("100"));
        assert_eq!(view.attr("MaxEntriesReturned"), Some("50"));
        assert!(body.contains(r#"<t:Constant Value="2024-01-01T00:00:00Z"/>"#));
        assert!(body.contains(r#"<t:FieldOrder Order="Descending"><t:FieldURI FieldURI="item:DateTimeReceived"/></t:FieldOrder>"#));

        let search = find_item_request(&FindItemQuery {
            shape: &shape,
            view: ItemView::Indexed { offset: 0, max: 25 },
            restriction: None,
            sort: None,
            folders: &folders,
            query: Some("subject:\"quarterly report\" from:alice"),
        });
        assert!(search.ends_with("<m:QueryString>subject:&quot;quarterly report&quot; from:alice</m:QueryString></m:FindItem>"));
    }

    #[test]
    fn calendar_view_request() {
        let shape = ItemShape::id_only();
        let start = Utc.with_ymd_and_hms(2024, 5, 1, 0, 0, 0).single().unwrap();
        let end = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).single().unwrap();
        let folders = [FolderRef::from(DistinguishedFolder::Calendar)];
        let body = find_item_request(&FindItemQuery {
            shape: &shape,
            view: ItemView::Calendar {
                start,
                end,
                max: 1000,
            },
            restriction: None,
            sort: None,
            folders: &folders,
            query: None,
        });
        assert!(body.contains(r#"<m:CalendarView MaxEntriesReturned="1000" StartDate="2024-05-01T00:00:00Z" EndDate="2024-06-01T00:00:00Z"/>"#));
    }

    #[test]
    fn parses_find_item_pages() {
        let message = wrap(
            r#"<m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>
                 <m:RootFolder IndexedPagingOffset="2" TotalItemsInView="7" IncludesLastItemInRange="false">
                   <t:Items>
                     <t:Message><t:ItemId Id="a" ChangeKey="1"/><t:Subject>One</t:Subject></t:Message>
                     <t:MeetingRequest><t:ItemId Id="b"/><t:Subject>Two</t:Subject></t:MeetingRequest>
                   </t:Items>
                 </m:RootFolder></m:FindItemResponseMessage>"#,
        );
        let message = message.child("FindItemResponseMessage").unwrap();
        let page = parse_find_item(message, Message::from_element).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.next_offset, Some(2));
        assert_eq!(page.total_items_in_view, Some(7));
        assert!(!page.includes_last_item_in_range);
        assert_eq!(page.items[1].kind, crate::ItemKind::MeetingRequest);

        let last = wrap(
            r#"<m:FindItemResponseMessage><m:RootFolder IndexedPagingOffset="7" TotalItemsInView="7" IncludesLastItemInRange="true"><t:Items/></m:RootFolder></m:FindItemResponseMessage>"#,
        );
        let page = parse_find_item(
            last.child("FindItemResponseMessage").unwrap(),
            Message::from_element,
        )
        .unwrap();
        assert_eq!(page.next_offset, None);
        assert!(page.items.is_empty());
    }

    #[test]
    fn sync_folder_items_request_and_response() {
        let options = SyncOptions {
            max_changes: 9999,
            ignore: vec![ItemId::new("mine")],
            ..SyncOptions::default()
        };
        let body = sync_folder_items_request(&FolderId::new("F").into(), Some("state"), &options);
        let root = wrap(&body);
        let op = root.child("SyncFolderItems").unwrap();
        let names: Vec<&str> = op.elements().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "ItemShape",
                "SyncFolderId",
                "SyncState",
                "Ignore",
                "MaxChangesReturned"
            ]
        );
        assert_eq!(op.child_text("MaxChangesReturned").as_deref(), Some("512"));
        assert_eq!(
            op.path(&["ItemShape", "BaseShape"])
                .and_then(Element::trimmed_text)
                .as_deref(),
            Some(BaseShape::IdOnly.as_str())
        );

        let response = wrap(
            r#"<m:SyncFolderItemsResponseMessage ResponseClass="Success">
                 <m:ResponseCode>NoError</m:ResponseCode>
                 <m:SyncState>H4sIAAAA</m:SyncState>
                 <m:IncludesLastItemInRange>false</m:IncludesLastItemInRange>
                 <m:Changes>
                   <t:Create><t:Message><t:ItemId Id="n1" ChangeKey="c1"/><t:ItemClass>IPM.Note</t:ItemClass><t:DateTimeReceived>2024-05-01T08:00:00Z</t:DateTimeReceived></t:Message></t:Create>
                   <t:Create><t:CalendarItem><t:ItemId Id="cal1"/></t:CalendarItem></t:Create>
                   <t:Update><t:Message><t:ItemId Id="u1" ChangeKey="c2"/><t:IsRead>true</t:IsRead></t:Message></t:Update>
                   <t:Delete><t:ItemId Id="d1" ChangeKey="c3"/></t:Delete>
                   <t:ReadFlagChange><t:ItemId Id="r1" ChangeKey="c4"/><t:IsRead>false</t:IsRead></t:ReadFlagChange>
                 </m:Changes>
               </m:SyncFolderItemsResponseMessage>"#,
        );
        let page =
            parse_sync_folder_items(response.child("SyncFolderItemsResponseMessage").unwrap())
                .unwrap();
        assert_eq!(page.sync_state, "H4sIAAAA");
        assert!(!page.includes_last_item_in_range);
        assert_eq!(page.changes.len(), 5);
        match &page.changes[0] {
            ItemChange::Created(m) => {
                assert_eq!(m.id.id, "n1");
                assert_eq!(m.item_class.as_deref(), Some("IPM.Note"));
                assert!(m.received.is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            matches!(&page.changes[1], ItemChange::Created(m) if m.kind == crate::ItemKind::CalendarItem)
        );
        assert!(matches!(&page.changes[2], ItemChange::Updated(m) if m.is_read == Some(true)));
        assert!(matches!(&page.changes[3], ItemChange::Deleted { id } if id.id == "d1"));
        assert!(
            matches!(&page.changes[4], ItemChange::ReadFlagChanged { id, is_read: false } if id.id == "r1")
        );
    }

    #[test]
    fn write_requests() {
        let moved = move_copy_request(
            "MoveItem",
            &[ItemId::new("a"), ItemId::new("b")],
            &DistinguishedFolder::JunkEmail.into(),
            true,
        );
        assert_eq!(
            moved,
            r#"<m:MoveItem><m:ToFolderId><t:DistinguishedFolderId Id="junkemail"/></m:ToFolderId><m:ItemIds><t:ItemId Id="a"/><t:ItemId Id="b"/></m:ItemIds><m:ReturnNewItemIds>true</m:ReturnNewItemIds></m:MoveItem>"#
        );
        let copied = move_copy_request(
            "CopyItem",
            &[ItemId::new("a")],
            &FolderId::new("F").into(),
            false,
        );
        assert!(!copied.contains("ReturnNewItemIds"));
        let deleted = delete_item_request(
            &[ItemRef::Id(ItemId::new("a"))],
            DeleteType::MoveToDeletedItems,
            SendCancellations::SendToNone,
            true,
        );
        assert_eq!(
            deleted,
            r#"<m:DeleteItem DeleteType="MoveToDeletedItems" SendMeetingCancellations="SendToNone" AffectedTaskOccurrences="AllOccurrences" SuppressReadReceipts="true"><m:ItemIds><t:ItemId Id="a"/></m:ItemIds></m:DeleteItem>"#
        );
        let update = update_item_request(
            &[(
                ItemRef::Id(ItemId::with_change_key("a", "k")),
                vec![
                    FieldUpdate::set(
                        PropertyPath::field("message:IsRead"),
                        value_xml("Message", "IsRead", "true"),
                    ),
                    FieldUpdate::Delete(PropertyPath::field("item:Categories")),
                ],
            )],
            &UpdateAttributes {
                message_disposition: Some("SaveOnly"),
                send_invitations: None,
                suppress_read_receipts: true,
            },
        );
        assert_eq!(
            update,
            concat!(
                r#"<m:UpdateItem MessageDisposition="SaveOnly" ConflictResolution="AlwaysOverwrite" SuppressReadReceipts="true"><m:ItemChanges><t:ItemChange><t:ItemId Id="a" ChangeKey="k"/><t:Updates>"#,
                r#"<t:SetItemField><t:FieldURI FieldURI="message:IsRead"/><t:Message><t:IsRead>true</t:IsRead></t:Message></t:SetItemField>"#,
                r#"<t:DeleteItemField><t:FieldURI FieldURI="item:Categories"/></t:DeleteItemField>"#,
                r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
            )
        );
    }
}

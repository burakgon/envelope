//! Folders: the folder tree with well-known roles, hierarchy
//! synchronization (`SyncFolderHierarchy`) and folder management.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseError};
use crate::soap;
use crate::types::{
    DistinguishedFolder, FolderId, FolderRef, PropertyPath, extended_tag_value, wire_enum,
    write_additional,
};
use crate::xml::{Element, XmlWriter, parse_bool};

/// `PR_ATTR_HIDDEN`: set on folders clients should not show.
const PR_ATTR_HIDDEN: u16 = 0x10F4;

/// Page size of `FindFolder` requests.
const FIND_FOLDER_PAGE_SIZE: u32 = 500;

/// Folder class of mail folders.
pub const FOLDER_CLASS_MAIL: &str = "IPF.Note";
/// Folder class of calendars.
pub const FOLDER_CLASS_CALENDAR: &str = "IPF.Appointment";
/// Folder class of contact folders.
pub const FOLDER_CLASS_CONTACTS: &str = "IPF.Contact";
/// Folder class of task folders.
pub const FOLDER_CLASS_TASKS: &str = "IPF.Task";

wire_enum! {
    /// Well-known role of a folder. The wire value is the distinguished
    /// folder id.
    pub enum FolderRole {
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
        /// The one-click "Archive" folder (absent in many mailboxes).
        Archive = "archive",
        /// The default calendar.
        Calendar = "calendar",
        /// The default contacts folder.
        Contacts = "contacts",
        /// Tasks.
        Tasks = "tasks",
        /// Notes.
        Notes = "notes",
        /// Root of the visible folder tree (IPM subtree).
        MsgFolderRoot = "msgfolderroot",
    }
}

impl FolderRole {
    /// The distinguished folder id of the role.
    pub fn distinguished(self) -> DistinguishedFolder {
        match self {
            FolderRole::Inbox => DistinguishedFolder::Inbox,
            FolderRole::Drafts => DistinguishedFolder::Drafts,
            FolderRole::SentItems => DistinguishedFolder::SentItems,
            FolderRole::DeletedItems => DistinguishedFolder::DeletedItems,
            FolderRole::JunkEmail => DistinguishedFolder::JunkEmail,
            FolderRole::Outbox => DistinguishedFolder::Outbox,
            FolderRole::Archive => DistinguishedFolder::Archive,
            FolderRole::Calendar => DistinguishedFolder::Calendar,
            FolderRole::Contacts => DistinguishedFolder::Contacts,
            FolderRole::Tasks => DistinguishedFolder::Tasks,
            FolderRole::Notes => DistinguishedFolder::Notes,
            FolderRole::MsgFolderRoot => DistinguishedFolder::MsgFolderRoot,
        }
    }
}

wire_enum! {
    /// Kind of folder, from the element Exchange describes it with.
    pub enum FolderKind {
        /// A mail (or generic) folder.
        Folder = "Folder",
        /// A calendar.
        Calendar = "CalendarFolder",
        /// A contacts folder.
        Contacts = "ContactsFolder",
        /// A tasks folder.
        Tasks = "TasksFolder",
        /// A search folder.
        Search = "SearchFolder",
    }
}

wire_enum! {
    /// Which folders `FindFolder` returns.
    pub enum FolderTraversal {
        /// Direct children only.
        Shallow = "Shallow",
        /// All descendants.
        Deep = "Deep",
        /// Soft-deleted children.
        SoftDeleted = "SoftDeleted",
    }
}

wire_enum! {
    /// How items and folders are deleted.
    pub enum DeleteType {
        /// Permanently delete.
        HardDelete = "HardDelete",
        /// Move to the recoverable items ("dumpster").
        SoftDelete = "SoftDelete",
        /// Move to the deleted items folder.
        MoveToDeletedItems = "MoveToDeletedItems",
    }
}

/// A folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    /// Folder id and change key.
    pub id: FolderId,
    /// The parent folder (for top-level folders the IPM subtree root).
    pub parent_id: Option<FolderId>,
    /// Display name.
    pub display_name: String,
    /// `IPF.Note`, `IPF.Appointment`, `IPF.Contact`, ... (absent on folders
    /// created by some clients; treat those as mail folders).
    pub folder_class: Option<String>,
    /// The element kind.
    pub kind: FolderKind,
    /// Number of items.
    pub total_count: Option<u32>,
    /// Number of unread items (mail folders).
    pub unread_count: Option<u32>,
    /// Number of direct child folders.
    pub child_folder_count: Option<u32>,
    /// Whether the folder is hidden from clients (`PR_ATTR_HIDDEN`).
    pub is_hidden: bool,
    /// The distinguished id the server reports for the folder (Exchange
    /// 2013 and later, when requested).
    pub distinguished_id: Option<String>,
    /// Well-known role, filled in by [`EwsClient::folder_tree`].
    pub role: Option<FolderRole>,
}

impl Folder {
    /// True for folders holding mail.
    pub fn is_mail_folder(&self) -> bool {
        match (&self.kind, &self.folder_class) {
            (FolderKind::Folder, None) => true,
            (FolderKind::Folder | FolderKind::Search, Some(class)) => is_mail_class(class),
            _ => false,
        }
    }

    /// True for calendars.
    pub fn is_calendar(&self) -> bool {
        self.kind == FolderKind::Calendar
            || self
                .folder_class
                .as_deref()
                .is_some_and(|c| c.starts_with(FOLDER_CLASS_CALENDAR))
    }

    /// True for contact folders.
    pub fn is_contacts(&self) -> bool {
        self.kind == FolderKind::Contacts
            || self
                .folder_class
                .as_deref()
                .is_some_and(|c| c.starts_with(FOLDER_CLASS_CONTACTS))
    }

    /// True for task folders.
    pub fn is_tasks(&self) -> bool {
        self.kind == FolderKind::Tasks
            || self
                .folder_class
                .as_deref()
                .is_some_and(|c| c.starts_with(FOLDER_CLASS_TASKS))
    }

    /// Parses a folder element (`Folder`, `CalendarFolder`, ...).
    pub(crate) fn from_element(element: &Element) -> Option<Folder> {
        let kind = FolderKind::parse(&element.name)?;
        let id = element.child("FolderId").and_then(FolderId::from_element)?;
        Some(Folder {
            id,
            parent_id: element
                .child("ParentFolderId")
                .and_then(FolderId::from_element),
            display_name: element.child_text("DisplayName").unwrap_or_default(),
            folder_class: element.child_text("FolderClass"),
            kind,
            total_count: element.child_u32("TotalCount"),
            unread_count: element.child_u32("UnreadCount"),
            child_folder_count: element.child_u32("ChildFolderCount"),
            is_hidden: extended_tag_value(element, PR_ATTR_HIDDEN)
                .and_then(|v| parse_bool(&v))
                .unwrap_or(false),
            distinguished_id: element.child_text("DistinguishedFolderId"),
            role: None,
        })
    }
}

fn is_mail_class(class: &str) -> bool {
    class == FOLDER_CLASS_MAIL || class.starts_with("IPF.Note.")
}

/// One change reported by `SyncFolderHierarchy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum FolderChange {
    /// A folder was created.
    Created(Folder),
    /// A folder changed (renamed, moved, counts changed).
    Updated(Folder),
    /// A folder was deleted.
    Deleted(FolderId),
}

/// One round of `SyncFolderHierarchy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSyncPage {
    /// The state to send with the next call.
    pub sync_state: String,
    /// False when more changes are pending: call again with `sync_state`.
    pub includes_last_folder_in_range: bool,
    /// The changes, in server order.
    pub changes: Vec<FolderChange>,
}

/// The folder shape used by every folder request: all properties plus the
/// hidden flag.
fn write_folder_shape(w: &mut XmlWriter) {
    w.start("m:FolderShape", &[])
        .text_element("t:BaseShape", "AllProperties");
    write_additional(w, &[PropertyPath::extended_tag(PR_ATTR_HIDDEN, "Boolean")]);
    w.end("m:FolderShape");
}

fn write_folder_ids(w: &mut XmlWriter, tag: &str, folders: &[FolderRef]) {
    w.start(tag, &[]);
    for folder in folders {
        folder.write(w);
    }
    w.end(tag);
}

pub(crate) fn get_folder_request(folders: &[FolderRef]) -> String {
    let mut w = XmlWriter::new();
    w.start("m:GetFolder", &[]);
    write_folder_shape(&mut w);
    write_folder_ids(&mut w, "m:FolderIds", folders);
    w.end("m:GetFolder");
    w.into_string()
}

/// `GetFolder` with ids only, for resolving distinguished folders.
pub(crate) fn get_folder_ids_request(folders: &[FolderRef]) -> String {
    let mut w = XmlWriter::new();
    w.start("m:GetFolder", &[])
        .start("m:FolderShape", &[])
        .text_element("t:BaseShape", "IdOnly")
        .end("m:FolderShape");
    write_folder_ids(&mut w, "m:FolderIds", folders);
    w.end("m:GetFolder");
    w.into_string()
}

pub(crate) fn find_folder_request(
    parent: &FolderRef,
    traversal: FolderTraversal,
    offset: u32,
    page_size: u32,
) -> String {
    let mut w = XmlWriter::new();
    w.start("m:FindFolder", &[("Traversal", traversal.as_str())]);
    write_folder_shape(&mut w);
    w.empty(
        "m:IndexedPageFolderView",
        &[
            ("MaxEntriesReturned", &page_size.to_string()),
            ("Offset", &offset.to_string()),
            ("BasePoint", "Beginning"),
        ],
    );
    write_folder_ids(&mut w, "m:ParentFolderIds", std::slice::from_ref(parent));
    w.end("m:FindFolder");
    w.into_string()
}

pub(crate) fn sync_folder_hierarchy_request(root: &FolderRef, sync_state: Option<&str>) -> String {
    let mut w = XmlWriter::new();
    w.start("m:SyncFolderHierarchy", &[]);
    write_folder_shape(&mut w);
    w.start("m:SyncFolderId", &[]);
    root.write(&mut w);
    w.end("m:SyncFolderId");
    if let Some(state) = sync_state.filter(|s| !s.is_empty()) {
        w.text_element("m:SyncState", state);
    }
    w.end("m:SyncFolderHierarchy");
    w.into_string()
}

pub(crate) fn create_folder_request(parent: &FolderRef, name: &str, class: &str) -> String {
    let element = if class.starts_with(FOLDER_CLASS_CALENDAR) {
        "t:CalendarFolder"
    } else if class.starts_with(FOLDER_CLASS_CONTACTS) {
        "t:ContactsFolder"
    } else if class.starts_with(FOLDER_CLASS_TASKS) {
        "t:TasksFolder"
    } else {
        "t:Folder"
    };
    let mut w = XmlWriter::new();
    w.start("m:CreateFolder", &[])
        .start("m:ParentFolderId", &[]);
    parent.write(&mut w);
    w.end("m:ParentFolderId")
        .start("m:Folders", &[])
        .start(element, &[])
        .text_element("t:FolderClass", class)
        .text_element("t:DisplayName", name)
        .end(element)
        .end("m:Folders")
        .end("m:CreateFolder");
    w.into_string()
}

pub(crate) fn rename_folder_request(folder: &FolderId, name: &str) -> String {
    let mut w = XmlWriter::new();
    w.start("m:UpdateFolder", &[])
        .start("m:FolderChanges", &[])
        .start("t:FolderChange", &[]);
    folder.write(&mut w, "t:FolderId");
    w.start("t:Updates", &[])
        .start("t:SetFolderField", &[])
        .empty("t:FieldURI", &[("FieldURI", "folder:DisplayName")])
        .start("t:Folder", &[])
        .text_element("t:DisplayName", name)
        .end("t:Folder")
        .end("t:SetFolderField")
        .end("t:Updates")
        .end("t:FolderChange")
        .end("m:FolderChanges")
        .end("m:UpdateFolder");
    w.into_string()
}

pub(crate) fn delete_folder_request(folders: &[FolderRef], delete_type: DeleteType) -> String {
    let mut w = XmlWriter::new();
    w.start("m:DeleteFolder", &[("DeleteType", delete_type.as_str())]);
    write_folder_ids(&mut w, "m:FolderIds", folders);
    w.end("m:DeleteFolder");
    w.into_string()
}

pub(crate) fn move_folder_request(folders: &[FolderRef], to: &FolderRef) -> String {
    let mut w = XmlWriter::new();
    w.start("m:MoveFolder", &[]).start("m:ToFolderId", &[]);
    to.write(&mut w);
    w.end("m:ToFolderId");
    write_folder_ids(&mut w, "m:FolderIds", folders);
    w.end("m:MoveFolder");
    w.into_string()
}

pub(crate) fn empty_folder_request(
    folders: &[FolderRef],
    delete_type: DeleteType,
    delete_subfolders: bool,
) -> String {
    let mut w = XmlWriter::new();
    w.start(
        "m:EmptyFolder",
        &[
            ("DeleteType", delete_type.as_str()),
            (
                "DeleteSubFolders",
                if delete_subfolders { "true" } else { "false" },
            ),
        ],
    );
    write_folder_ids(&mut w, "m:FolderIds", folders);
    w.end("m:EmptyFolder");
    w.into_string()
}

/// The folders inside the `Folders` child of a response message.
pub(crate) fn folders_of(message: &Element) -> Vec<Folder> {
    message
        .child("Folders")
        .map(|folders| {
            folders
                .elements()
                .filter_map(Folder::from_element)
                .collect()
        })
        .unwrap_or_default()
}

/// The first folder id inside the `Folders` child of a response message.
fn first_folder_id(message: &Element) -> Option<FolderId> {
    message
        .child("Folders")?
        .elements()
        .find_map(|f| f.child("FolderId").and_then(FolderId::from_element))
}

/// One page of `FindFolder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FindFolderPage {
    pub(crate) folders: Vec<Folder>,
    pub(crate) next_offset: Option<u32>,
    pub(crate) includes_last: bool,
}

pub(crate) fn parse_find_folder(message: &Element) -> Result<FindFolderPage, EwsError> {
    let root = message.child("RootFolder").ok_or_else(|| {
        EwsError::InvalidResponse("FindFolder response without RootFolder".into())
    })?;
    Ok(FindFolderPage {
        folders: folders_of(root),
        next_offset: root
            .attr("IndexedPagingOffset")
            .and_then(|v| v.trim().parse().ok()),
        includes_last: root
            .attr("IncludesLastItemInRange")
            .and_then(parse_bool)
            .unwrap_or(true),
    })
}

pub(crate) fn parse_sync_folder_hierarchy(message: &Element) -> Result<FolderSyncPage, EwsError> {
    let sync_state = message.child_text("SyncState").ok_or_else(|| {
        EwsError::InvalidResponse("SyncFolderHierarchy response without SyncState".into())
    })?;
    let mut changes = Vec::new();
    if let Some(list) = message.child("Changes") {
        for change in list.elements() {
            match change.name.as_str() {
                "Create" | "Update" => {
                    let Some(folder) = change.elements().find_map(Folder::from_element) else {
                        continue;
                    };
                    changes.push(if change.is("Create") {
                        FolderChange::Created(folder)
                    } else {
                        FolderChange::Updated(folder)
                    });
                }
                "Delete" => {
                    if let Some(id) = change.child("FolderId").and_then(FolderId::from_element) {
                        changes.push(FolderChange::Deleted(id));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(FolderSyncPage {
        sync_state,
        includes_last_folder_in_range: message
            .child_bool("IncludesLastFolderInRange")
            .unwrap_or(true),
        changes,
    })
}

/// Orders folders so that parents precede their children (breadth-first
/// from `root`); folders not reachable from it keep their order at the end.
pub(crate) fn order_tree(root: &str, folders: Vec<Folder>) -> Vec<Folder> {
    let mut children: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, folder) in folders.iter().enumerate() {
        if let Some(parent) = &folder.parent_id {
            children.entry(parent.id.clone()).or_default().push(index);
        }
    }
    let mut order = Vec::with_capacity(folders.len());
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([root.to_owned()]);
    while let Some(parent) = queue.pop_front() {
        for &index in children.get(&parent).map(Vec::as_slice).unwrap_or_default() {
            if seen.insert(index) {
                order.push(index);
                queue.push_back(folders[index].id.id.clone());
            }
        }
    }
    for index in 0..folders.len() {
        if seen.insert(index) {
            order.push(index);
        }
    }
    let mut slots: Vec<Option<Folder>> = folders.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

impl EwsClient {
    /// One folder.
    pub async fn get_folder(&self, folder: impl Into<FolderRef>) -> Result<Folder, EwsError> {
        let request = SoapRequest::read("GetFolder", get_folder_request(&[folder.into()]));
        let response = self.call(&request).await?;
        let message = soap::single(&response)?;
        folders_of(message)
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("GetFolder returned no folder".into()))
    }

    /// Several folders in one request; one result per requested folder.
    pub async fn get_folders(
        &self,
        folders: &[FolderRef],
    ) -> Result<Vec<Result<Folder, ResponseError>>, EwsError> {
        if folders.is_empty() {
            return Ok(Vec::new());
        }
        let request = SoapRequest::read("GetFolder", get_folder_request(folders));
        let response = self.call(&request).await?;
        Ok(soap::each(&response)?
            .into_iter()
            .map(|result| {
                result.and_then(|message| {
                    folders_of(message).into_iter().next().ok_or_else(|| {
                        ResponseError::new(
                            crate::ResponseCode::Other("ErrorNoFolderReturned".into()),
                            "GetFolder returned no folder",
                        )
                    })
                })
            })
            .collect())
    }

    /// All folders below `parent` (every page).
    pub async fn find_folders(
        &self,
        parent: impl Into<FolderRef>,
        traversal: FolderTraversal,
    ) -> Result<Vec<Folder>, EwsError> {
        let parent = parent.into();
        let mut folders = Vec::new();
        let mut offset = 0u32;
        loop {
            let body = find_folder_request(&parent, traversal, offset, FIND_FOLDER_PAGE_SIZE);
            let response = self.call(&SoapRequest::read("FindFolder", body)).await?;
            let page = parse_find_folder(soap::single(&response)?)?;
            let returned = page.folders.len();
            folders.extend(page.folders);
            let next = page.next_offset.unwrap_or(offset + returned as u32);
            if page.includes_last || returned == 0 || next <= offset {
                break;
            }
            offset = next;
        }
        Ok(folders)
    }

    /// Ids of the well-known folders of the mailbox. Roles whose folder does
    /// not exist are missing from the map; the one-click archive folder is
    /// looked up separately because older servers do not know its
    /// distinguished id.
    pub async fn well_known_folder_ids(&self) -> Result<HashMap<FolderRole, FolderId>, EwsError> {
        let roles: Vec<FolderRole> = FolderRole::ALL
            .iter()
            .copied()
            .filter(|role| *role != FolderRole::Archive)
            .collect();
        let refs: Vec<FolderRef> = roles.iter().map(|r| r.distinguished().into()).collect();
        let request = SoapRequest::read("GetFolder", get_folder_ids_request(&refs));
        let response = self.call(&request).await?;
        let mut ids = HashMap::new();
        for (role, result) in roles.iter().zip(soap::each(&response)?) {
            if let Some(id) = result.ok().and_then(first_folder_id) {
                ids.insert(*role, id);
            }
        }
        if self.archive_id_unsupported() {
            return Ok(ids);
        }
        let archive = SoapRequest::read(
            "GetFolder",
            get_folder_ids_request(&[DistinguishedFolder::Archive.into()]),
        );
        match self.call(&archive).await {
            Ok(response) => match soap::single(&response) {
                Ok(message) => {
                    if let Some(id) = first_folder_id(message) {
                        ids.insert(FolderRole::Archive, id);
                    }
                }
                // The mailbox has no such folder (yet); ask again next time.
                Err(err) => tracing::debug!(error = %err, "no archive folder"),
            },
            Err(err) if err.is_transient() || err.is_auth_error() => return Err(err),
            Err(err) => {
                // Older schemas do not know the id at all.
                tracing::debug!(error = %err, "server does not know the archive folder id");
                self.mark_archive_id_unsupported();
            }
        }
        Ok(ids)
    }

    /// The complete folder tree below the IPM subtree root (mail,
    /// calendar, contact and other folders, hidden ones included and
    /// flagged), parents before children, with well-known roles assigned.
    ///
    /// When the server has no distinguished archive folder, a top-level mail
    /// folder named "Archive" gets the [`FolderRole::Archive`] role.
    pub async fn folder_tree(&self) -> Result<Vec<Folder>, EwsError> {
        let roles = self.well_known_folder_ids().await?;
        let folders = self
            .find_folders(DistinguishedFolder::MsgFolderRoot, FolderTraversal::Deep)
            .await?;
        let root = roles
            .get(&FolderRole::MsgFolderRoot)
            .map(|id| id.id.clone());
        let by_id: HashMap<&str, FolderRole> = roles
            .iter()
            .map(|(role, id)| (id.id.as_str(), *role))
            .collect();
        let mut folders: Vec<Folder> = folders
            .into_iter()
            .map(|mut folder| {
                folder.role = by_id.get(folder.id.id.as_str()).copied();
                folder
            })
            .collect();
        if !roles.contains_key(&FolderRole::Archive)
            && let Some(root) = &root
            && let Some(archive) = folders.iter_mut().find(|f| {
                f.role.is_none()
                    && f.is_mail_folder()
                    && f.parent_id.as_ref().is_some_and(|p| &p.id == root)
                    && f.display_name.eq_ignore_ascii_case("Archive")
            })
        {
            archive.role = Some(FolderRole::Archive);
        }
        Ok(match root {
            Some(root) => order_tree(&root, folders),
            None => folders,
        })
    }

    /// One round of hierarchy synchronization below the IPM subtree root.
    /// Start without a state; repeat with the returned state while
    /// [`includes_last_folder_in_range`](FolderSyncPage::includes_last_folder_in_range)
    /// is false. An [`EwsError::requires_resync`] error means the state
    /// expired: start over without one.
    pub async fn sync_folder_hierarchy(
        &self,
        sync_state: Option<&str>,
    ) -> Result<FolderSyncPage, EwsError> {
        self.sync_folder_hierarchy_from(DistinguishedFolder::MsgFolderRoot, sync_state)
            .await
    }

    /// Hierarchy synchronization below any folder.
    pub async fn sync_folder_hierarchy_from(
        &self,
        root: impl Into<FolderRef>,
        sync_state: Option<&str>,
    ) -> Result<FolderSyncPage, EwsError> {
        let body = sync_folder_hierarchy_request(&root.into(), sync_state);
        let response = self
            .call(&SoapRequest::read("SyncFolderHierarchy", body))
            .await?;
        parse_sync_folder_hierarchy(soap::single(&response)?)
    }

    /// Creates a folder of the given class (see [`FOLDER_CLASS_MAIL`] and
    /// friends) and returns its id.
    pub async fn create_folder(
        &self,
        parent: impl Into<FolderRef>,
        display_name: &str,
        folder_class: &str,
    ) -> Result<FolderId, EwsError> {
        if display_name.trim().is_empty() {
            return Err(EwsError::InvalidInput("folder name is empty".into()));
        }
        let body = create_folder_request(&parent.into(), display_name, folder_class);
        let response = self.call(&SoapRequest::write("CreateFolder", body)).await?;
        first_folder_id(soap::single(&response)?)
            .ok_or_else(|| EwsError::InvalidResponse("CreateFolder returned no folder id".into()))
    }

    /// Renames a folder and returns its id with the new change key.
    pub async fn rename_folder(
        &self,
        folder: &FolderId,
        display_name: &str,
    ) -> Result<FolderId, EwsError> {
        if display_name.trim().is_empty() {
            return Err(EwsError::InvalidInput("folder name is empty".into()));
        }
        let body = rename_folder_request(folder, display_name);
        let response = self.call(&SoapRequest::write("UpdateFolder", body)).await?;
        Ok(first_folder_id(soap::single(&response)?).unwrap_or_else(|| folder.clone()))
    }

    /// Deletes a folder.
    pub async fn delete_folder(
        &self,
        folder: impl Into<FolderRef>,
        delete_type: DeleteType,
    ) -> Result<(), EwsError> {
        let body = delete_folder_request(&[folder.into()], delete_type);
        let response = self.call(&SoapRequest::write("DeleteFolder", body)).await?;
        soap::single(&response).map(drop)
    }

    /// Moves a folder below another one and returns its id.
    pub async fn move_folder(
        &self,
        folder: &FolderId,
        to: impl Into<FolderRef>,
    ) -> Result<FolderId, EwsError> {
        let body = move_folder_request(&[folder.into()], &to.into());
        let response = self.call(&SoapRequest::write("MoveFolder", body)).await?;
        Ok(first_folder_id(soap::single(&response)?).unwrap_or_else(|| folder.clone()))
    }

    /// Deletes every item of a folder ("empty trash"), optionally with its
    /// subfolders.
    pub async fn empty_folder(
        &self,
        folder: impl Into<FolderRef>,
        delete_type: DeleteType,
        delete_subfolders: bool,
    ) -> Result<(), EwsError> {
        let body = empty_folder_request(&[folder.into()], delete_type, delete_subfolders);
        let response = self.call(&SoapRequest::write("EmptyFolder", body)).await?;
        soap::single(&response).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    #[test]
    fn get_folder_request_lists_ids_after_the_shape() {
        let body = get_folder_request(&[
            DistinguishedFolder::Inbox.into(),
            FolderId::with_change_key("AQMk", "AQAA").into(),
        ]);
        let root = xml::parse(&format!(
            r#"<r xmlns:m="{}" xmlns:t="{}">{body}</r>"#,
            xml::NS_MESSAGES,
            xml::NS_TYPES
        ))
        .unwrap();
        let op = root.child("GetFolder").unwrap();
        let names: Vec<&str> = op.elements().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["FolderShape", "FolderIds"]);
        assert_eq!(
            op.path(&["FolderShape", "BaseShape"])
                .and_then(Element::trimmed_text)
                .as_deref(),
            Some("AllProperties")
        );
        assert!(
            body.contains(r#"<t:ExtendedFieldURI PropertyTag="0x10f4" PropertyType="Boolean"/>"#)
        );
        assert!(body.contains(
            r#"<t:DistinguishedFolderId Id="inbox"/><t:FolderId Id="AQMk" ChangeKey="AQAA"/>"#
        ));
    }

    #[test]
    fn find_and_sync_requests() {
        let body = find_folder_request(
            &DistinguishedFolder::MsgFolderRoot.into(),
            FolderTraversal::Deep,
            500,
            500,
        );
        assert!(body.starts_with(r#"<m:FindFolder Traversal="Deep">"#));
        assert!(body.contains(r#"<m:IndexedPageFolderView MaxEntriesReturned="500" Offset="500" BasePoint="Beginning"/>"#));
        assert!(body.ends_with(r#"<m:ParentFolderIds><t:DistinguishedFolderId Id="msgfolderroot"/></m:ParentFolderIds></m:FindFolder>"#));

        let initial =
            sync_folder_hierarchy_request(&DistinguishedFolder::MsgFolderRoot.into(), None);
        assert!(!initial.contains("SyncState"));
        let next = sync_folder_hierarchy_request(
            &DistinguishedFolder::MsgFolderRoot.into(),
            Some("H4sI<&>"),
        );
        assert!(
            next.contains("<m:SyncState>H4sI&lt;&amp;&gt;</m:SyncState></m:SyncFolderHierarchy>")
        );
    }

    #[test]
    fn folder_management_requests() {
        let create = create_folder_request(
            &DistinguishedFolder::Inbox.into(),
            "Rechnungen & Belege",
            FOLDER_CLASS_MAIL,
        );
        assert!(create.contains("<t:Folder><t:FolderClass>IPF.Note</t:FolderClass><t:DisplayName>Rechnungen &amp; Belege</t:DisplayName></t:Folder>"));
        let calendar = create_folder_request(
            &DistinguishedFolder::Calendar.into(),
            "Team",
            FOLDER_CLASS_CALENDAR,
        );
        assert!(calendar.contains("<t:CalendarFolder>"));
        let rename = rename_folder_request(&FolderId::with_change_key("F", "K"), "New");
        assert!(rename.contains(r#"<t:FolderId Id="F" ChangeKey="K"/><t:Updates><t:SetFolderField><t:FieldURI FieldURI="folder:DisplayName"/><t:Folder><t:DisplayName>New</t:DisplayName></t:Folder></t:SetFolderField></t:Updates>"#));
        let delete = delete_folder_request(&[FolderId::new("F").into()], DeleteType::SoftDelete);
        assert_eq!(
            delete,
            r#"<m:DeleteFolder DeleteType="SoftDelete"><m:FolderIds><t:FolderId Id="F"/></m:FolderIds></m:DeleteFolder>"#
        );
        let moved = move_folder_request(
            &[FolderId::new("F").into()],
            &DistinguishedFolder::Inbox.into(),
        );
        assert_eq!(
            moved,
            r#"<m:MoveFolder><m:ToFolderId><t:DistinguishedFolderId Id="inbox"/></m:ToFolderId><m:FolderIds><t:FolderId Id="F"/></m:FolderIds></m:MoveFolder>"#
        );
        let empty = empty_folder_request(
            &[DistinguishedFolder::DeletedItems.into()],
            DeleteType::HardDelete,
            true,
        );
        assert!(
            empty.starts_with(r#"<m:EmptyFolder DeleteType="HardDelete" DeleteSubFolders="true">"#)
        );
    }

    const FIND_FOLDER: &str = r#"<m:FindFolderResponseMessage ResponseClass="Success" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <m:ResponseCode>NoError</m:ResponseCode>
  <m:RootFolder IndexedPagingOffset="3" TotalItemsInView="5" IncludesLastItemInRange="false">
    <t:Folders>
      <t:Folder>
        <t:FolderId Id="inbox-id" ChangeKey="AQAAABYA"/>
        <t:ParentFolderId Id="root-id" ChangeKey="AQAAAA=="/>
        <t:FolderClass>IPF.Note</t:FolderClass>
        <t:DisplayName>Inbox</t:DisplayName>
        <t:TotalCount>12</t:TotalCount>
        <t:ChildFolderCount>1</t:ChildFolderCount>
        <t:UnreadCount>3</t:UnreadCount>
      </t:Folder>
      <t:CalendarFolder>
        <t:FolderId Id="cal-id" ChangeKey="AgAAABYA"/>
        <t:ParentFolderId Id="root-id"/>
        <t:FolderClass>IPF.Appointment</t:FolderClass>
        <t:DisplayName>Calendar</t:DisplayName>
        <t:TotalCount>40</t:TotalCount>
        <t:ChildFolderCount>0</t:ChildFolderCount>
      </t:CalendarFolder>
      <t:Folder>
        <t:FolderId Id="hidden-id"/>
        <t:ParentFolderId Id="root-id"/>
        <t:FolderClass>IPF.Configuration</t:FolderClass>
        <t:DisplayName>Conversation Action Settings</t:DisplayName>
        <t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x10f4" PropertyType="Boolean"/><t:Value>true</t:Value></t:ExtendedProperty>
      </t:Folder>
    </t:Folders>
  </m:RootFolder>
</m:FindFolderResponseMessage>"#;

    #[test]
    fn parses_find_folder_pages() {
        let message = xml::parse(FIND_FOLDER).unwrap();
        let page = parse_find_folder(&message).unwrap();
        assert_eq!(page.next_offset, Some(3));
        assert!(!page.includes_last);
        assert_eq!(page.folders.len(), 3);
        let inbox = &page.folders[0];
        assert_eq!(inbox.id, FolderId::with_change_key("inbox-id", "AQAAABYA"));
        assert_eq!(
            inbox.parent_id.as_ref().map(|p| p.id.as_str()),
            Some("root-id")
        );
        assert_eq!(inbox.unread_count, Some(3));
        assert_eq!(inbox.total_count, Some(12));
        assert_eq!(inbox.child_folder_count, Some(1));
        assert!(inbox.is_mail_folder());
        assert!(!inbox.is_hidden);
        let calendar = &page.folders[1];
        assert_eq!(calendar.kind, FolderKind::Calendar);
        assert!(calendar.is_calendar());
        assert!(!calendar.is_mail_folder());
        assert_eq!(calendar.unread_count, None);
        let hidden = &page.folders[2];
        assert!(hidden.is_hidden);
        assert!(!hidden.is_mail_folder());
    }

    #[test]
    fn parses_hierarchy_changes() {
        let message = xml::parse(
            r#"<m:SyncFolderHierarchyResponseMessage ResponseClass="Success" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
                 <m:ResponseCode>NoError</m:ResponseCode>
                 <m:SyncState>H4sIAAAAAAAEAO29B2AcSZY=</m:SyncState>
                 <m:IncludesLastFolderInRange>false</m:IncludesLastFolderInRange>
                 <m:Changes>
                   <t:Create><t:Folder><t:FolderId Id="new"/><t:ParentFolderId Id="root"/><t:FolderClass>IPF.Note</t:FolderClass><t:DisplayName>Projects</t:DisplayName></t:Folder></t:Create>
                   <t:Update><t:ContactsFolder><t:FolderId Id="contacts"/><t:DisplayName>Contacts</t:DisplayName></t:ContactsFolder></t:Update>
                   <t:Delete><t:FolderId Id="gone" ChangeKey="x"/></t:Delete>
                 </m:Changes>
               </m:SyncFolderHierarchyResponseMessage>"#,
        )
        .unwrap();
        let page = parse_sync_folder_hierarchy(&message).unwrap();
        assert_eq!(page.sync_state, "H4sIAAAAAAAEAO29B2AcSZY=");
        assert!(!page.includes_last_folder_in_range);
        assert_eq!(page.changes.len(), 3);
        assert!(
            matches!(&page.changes[0], FolderChange::Created(f) if f.display_name == "Projects")
        );
        assert!(matches!(&page.changes[1], FolderChange::Updated(f) if f.is_contacts()));
        assert!(matches!(&page.changes[2], FolderChange::Deleted(id) if id.id == "gone"));
        let empty = xml::parse("<m:SyncFolderHierarchyResponseMessage xmlns:m=\"m\"><m:ResponseCode>NoError</m:ResponseCode></m:SyncFolderHierarchyResponseMessage>").unwrap();
        assert!(matches!(
            parse_sync_folder_hierarchy(&empty),
            Err(EwsError::InvalidResponse(_))
        ));
    }

    #[test]
    fn orders_trees_parents_first() {
        let folder = |id: &str, parent: &str| Folder {
            id: FolderId::new(id),
            parent_id: Some(FolderId::new(parent)),
            display_name: id.to_owned(),
            folder_class: None,
            kind: FolderKind::Folder,
            total_count: None,
            unread_count: None,
            child_folder_count: None,
            is_hidden: false,
            distinguished_id: None,
            role: None,
        };
        let ordered = order_tree(
            "root",
            vec![
                folder("grandchild", "child"),
                folder("child", "inbox"),
                folder("inbox", "root"),
                folder("orphan", "elsewhere"),
                folder("sent", "root"),
            ],
        );
        let names: Vec<&str> = ordered.iter().map(|f| f.display_name.as_str()).collect();
        assert_eq!(names, ["inbox", "sent", "child", "grandchild", "orphan"]);
    }

    #[test]
    fn roles_map_to_distinguished_ids() {
        for role in FolderRole::ALL {
            assert_eq!(role.distinguished().as_str(), role.as_str());
        }
    }
}

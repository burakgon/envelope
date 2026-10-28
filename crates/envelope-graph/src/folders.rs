//! Mail folders: the full folder tree, well-known roles and folder management.

use std::collections::{HashMap, HashSet};

use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::batch::BatchRequest;
use crate::client::{GraphClient, GraphRequest};
use crate::error::GraphError;
use crate::serde_util::{graph_enum, null_default};

/// Properties requested for every folder.
const FOLDER_FIELDS: &str =
    "id,displayName,parentFolderId,childFolderCount,unreadItemCount,totalItemCount,isHidden";

/// Page size used when listing folders.
const FOLDER_PAGE_SIZE: &str = "100";

/// How many child-folder listings run concurrently (Outlook allows four
/// concurrent requests per mailbox).
const FOLDER_CONCURRENCY: usize = 4;

graph_enum! {
    /// Well-known role of a mail folder. The wire value is the Graph
    /// well-known folder name, usable in place of a folder id.
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
        /// The archive folder (absent in some mailboxes).
        Archive = "archive",
        /// Outgoing messages waiting to be sent.
        Outbox = "outbox",
    }
}

/// A mail folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailFolder {
    /// Folder id.
    pub id: String,
    /// Id of the parent folder. Top-level folders point at the (hidden)
    /// message folder root.
    #[serde(default, rename = "parentFolderId")]
    pub parent_id: Option<String>,
    /// Display name.
    #[serde(default, deserialize_with = "null_default")]
    pub display_name: String,
    /// Number of unread items.
    #[serde(default, rename = "unreadItemCount", deserialize_with = "null_default")]
    pub unread_count: u32,
    /// Number of items.
    #[serde(default, rename = "totalItemCount", deserialize_with = "null_default")]
    pub total_count: u32,
    /// Number of direct child folders.
    #[serde(default, deserialize_with = "null_default")]
    pub child_folder_count: u32,
    /// Whether the folder is hidden.
    #[serde(default, deserialize_with = "null_default")]
    pub is_hidden: bool,
    /// Well-known role, filled in by [`GraphClient::mail_folder_tree`].
    #[serde(default)]
    pub role: Option<FolderRole>,
}

#[derive(Deserialize)]
struct IdOnly {
    id: String,
}

impl GraphClient {
    /// The complete folder tree (hidden folders excluded) as a flat list in
    /// breadth-first order: parents always precede their children. Each folder
    /// carries its well-known [`FolderRole`], if any.
    pub async fn mail_folder_tree(&self) -> Result<Vec<MailFolder>, GraphError> {
        let top_request = GraphRequest::get(["me", "mailFolders"])
            .query("$select", FOLDER_FIELDS)
            .query("$top", FOLDER_PAGE_SIZE);
        let (roles, top) = futures::try_join!(
            self.well_known_folder_ids(),
            self.get_all::<MailFolder>(&top_request, None)
        )?;

        let mut seen: HashSet<String> = HashSet::new();
        let mut folders: Vec<MailFolder> = Vec::new();
        let mut frontier: Vec<String> = Vec::new();
        for folder in top {
            if seen.insert(folder.id.clone()) {
                if folder.child_folder_count > 0 {
                    frontier.push(folder.id.clone());
                }
                folders.push(folder);
            }
        }
        while !frontier.is_empty() {
            // Owned ids keep the stream's futures `Send` (no borrowed
            // closure arguments with higher-ranked lifetimes).
            let levels: Vec<Vec<MailFolder>> = futures::stream::iter(std::mem::take(&mut frontier))
                .map(|id| async move { self.child_folders(&id).await })
                .buffered(FOLDER_CONCURRENCY)
                .try_collect()
                .await?;
            let mut next = Vec::new();
            for folder in levels.into_iter().flatten() {
                if seen.insert(folder.id.clone()) {
                    if folder.child_folder_count > 0 {
                        next.push(folder.id.clone());
                    }
                    folders.push(folder);
                }
            }
            frontier = next;
        }

        let by_id: HashMap<&str, FolderRole> = roles
            .iter()
            .map(|(role, id)| (id.as_str(), *role))
            .collect();
        for folder in &mut folders {
            folder.role = by_id.get(folder.id.as_str()).copied();
        }
        Ok(folders)
    }

    /// Direct, non-hidden child folders of a folder.
    pub async fn child_folders(&self, folder_id: &str) -> Result<Vec<MailFolder>, GraphError> {
        let request = GraphRequest::get(["me", "mailFolders", folder_id, "childFolders"])
            .query("$select", FOLDER_FIELDS)
            .query("$top", FOLDER_PAGE_SIZE);
        self.get_all(&request, None).await
    }

    /// Resolves the ids of the well-known folders with one JSON batch.
    /// Folders the mailbox does not have (for example `archive`) are absent.
    pub async fn well_known_folder_ids(&self) -> Result<HashMap<FolderRole, String>, GraphError> {
        let requests = FolderRole::ALL
            .iter()
            .map(|role| {
                BatchRequest::new(
                    role.as_str(),
                    GraphRequest::get(["me", "mailFolders", role.as_str()]).query("$select", "id"),
                )
            })
            .collect();
        let responses = self.batch(requests).await?;
        let mut ids = HashMap::new();
        let mut first_error = None;
        for response in responses {
            let Some(role) = FolderRole::parse(&response.id) else {
                continue;
            };
            match response.json::<IdOnly>() {
                Ok(found) => {
                    ids.insert(role, found.id);
                }
                Err(err) if err.is_not_found() => {}
                Err(err) => {
                    tracing::warn!(%role, error = %err, "could not resolve well-known folder");
                    first_error.get_or_insert(err);
                }
            }
        }
        match first_error {
            Some(err) if ids.is_empty() => Err(err),
            _ => Ok(ids),
        }
    }

    /// One folder, by id or well-known name (`inbox`, `sentitems`, …).
    pub async fn mail_folder(&self, id_or_name: &str) -> Result<MailFolder, GraphError> {
        let request =
            GraphRequest::get(["me", "mailFolders", id_or_name]).query("$select", FOLDER_FIELDS);
        self.send_json(&request).await
    }

    /// Creates a folder at the top level or below `parent_id`.
    pub async fn create_mail_folder(
        &self,
        parent_id: Option<&str>,
        display_name: &str,
    ) -> Result<MailFolder, GraphError> {
        let request = match parent_id {
            Some(parent) => GraphRequest::post(["me", "mailFolders", parent, "childFolders"]),
            None => GraphRequest::post(["me", "mailFolders"]),
        }
        .json(&json!({ "displayName": display_name, "isHidden": false }))?;
        self.send_json(&request).await
    }

    /// Renames a folder.
    pub async fn rename_mail_folder(
        &self,
        folder_id: &str,
        display_name: &str,
    ) -> Result<MailFolder, GraphError> {
        let request = GraphRequest::patch(["me", "mailFolders", folder_id])
            .json(&json!({ "displayName": display_name }))?;
        self.send_json(&request).await
    }

    /// Moves a folder below another folder (id or well-known name).
    pub async fn move_mail_folder(
        &self,
        folder_id: &str,
        destination_id: &str,
    ) -> Result<MailFolder, GraphError> {
        let request = GraphRequest::post(["me", "mailFolders", folder_id, "move"])
            .json(&json!({ "destinationId": destination_id }))?;
        self.send_json(&request).await
    }

    /// Deletes a folder and its contents.
    pub async fn delete_mail_folder(&self, folder_id: &str) -> Result<(), GraphError> {
        self.send_empty(&GraphRequest::delete(["me", "mailFolders", folder_id]))
            .await
    }
}

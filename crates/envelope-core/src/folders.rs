//! Managing folders and labels: creating, renaming, nesting, deleting and
//! coloring folders, Gmail labels and Outlook categories, and emptying Trash
//! and Spam.
//!
//! Changes to the folder tree go to the server right away and are applied
//! here once it agrees. Queued operations name folders by their server path
//! or id, Microsoft and Exchange servers choose the ids of new folders, and
//! the reasons a server refuses (a name already taken, a folder that cannot
//! hold others) have to reach the person who asked rather than a retry
//! queue. Colors and emptying Trash or Spam work offline like other changes.

use std::collections::BTreeSet;

use envelope_graph::CategoryColor;
use rusqlite::params;

use crate::db::mail::{self, AccountRow, FolderRow, FolderUpsert};
use crate::db::misc::{self, PendingOp};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::*;
use crate::sync::ews::{self as ews_sync, CategoryEdit, EwsMail};
use crate::sync::graph::{GraphMail, category_name, category_upsert};
use crate::sync::imap::folder_priority;

/// Gmail's own folders (`[Gmail]/…`), which hold no user labels.
fn gmail_system(f: &FolderRow) -> bool {
    f.remote_id.starts_with("[Gmail]") || f.remote_id.starts_with("[Google Mail]")
}

/// Special folders keep their name and place.
fn protected(f: &FolderRow) -> bool {
    f.role.is_some() || gmail_system(f)
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.to_string())
}

/// A trimmed folder name, refused when empty or when it holds the server's
/// hierarchy delimiter (which would make it a path).
fn clean_name(name: &str, delimiter: Option<&str>) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("A folder needs a name"));
    }
    if let Some(d) = delimiter.filter(|d| !d.is_empty())
        && name.contains(d)
    {
        return Err(Error::Invalid(format!("A folder name can't contain “{d}”")));
    }
    Ok(name.to_string())
}

/// Refuses a name another folder of the same kind already has in that place.
fn check_free(
    folders: &[FolderRow],
    parent_id: Option<i64>,
    kind: &str,
    name: &str,
    except: Option<i64>,
) -> Result<()> {
    let taken = folders.iter().any(|f| {
        Some(f.id) != except
            && f.parent_id == parent_id
            && f.kind == kind
            && f.name.eq_ignore_ascii_case(name)
    });
    if taken {
        return Err(Error::Invalid(format!(
            "There's already a folder called “{name}” there"
        )));
    }
    Ok(())
}

/// The delimiter new IMAP folders use (the parent's, or the account's).
fn imap_delimiter(folders: &[FolderRow], parent: Option<&FolderRow>) -> String {
    parent
        .and_then(|p| p.delimiter.clone())
        .or_else(|| folders.iter().find_map(|f| f.delimiter.clone()))
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/".into())
}

/// Where top-level folders go: below `INBOX` on servers that keep every
/// folder there (Courier, older Cyrus), else at the root.
fn imap_top_prefix(folders: &[FolderRow], delimiter: &str, gmail: bool) -> String {
    let inbox = format!("INBOX{delimiter}");
    let others: Vec<&FolderRow> = folders
        .iter()
        .filter(|f| !f.remote_id.eq_ignore_ascii_case("INBOX"))
        .collect();
    let under_inbox = !gmail
        && !others.is_empty()
        && others
            .iter()
            .all(|f| f.remote_id.to_ascii_uppercase().starts_with(&inbox));
    if under_inbox { inbox } else { String::new() }
}

/// The folder a path sits in, as a listing would place it.
fn imap_parent(folders: &[FolderRow], path: &str, delimiter: &str) -> Option<i64> {
    let (parent, _) = path.rsplit_once(delimiter)?;
    folders.iter().find(|f| f.remote_id == parent).map(|f| f.id)
}

/// The Outlook color preset closest to a CSS color (`#rrggbb`).
pub(crate) fn nearest_preset(color: Option<&str>) -> CategoryColor {
    let rgb = |hex: &str| -> Option<(i32, i32, i32)> {
        let hex = hex.trim().trim_start_matches('#');
        if hex.len() != 6 {
            return None;
        }
        let channel = |i: usize| i32::from_str_radix(&hex[i..i + 2], 16).ok();
        Some((channel(0)?, channel(2)?, channel(4)?))
    };
    let Some((r, g, b)) = color.and_then(rgb) else {
        return CategoryColor::NoColor;
    };
    CategoryColor::ALL
        .iter()
        .filter_map(|c| Some((*c, rgb(c.hex()?)?)))
        .min_by_key(|(_, (r2, g2, b2))| (r - r2).pow(2) + (g - g2).pow(2) + (b - b2).pow(2))
        .map_or(CategoryColor::NoColor, |(c, _)| c)
}

/// The preset number Exchange stores for a color (`-1` for none).
fn preset_number(color: CategoryColor) -> i32 {
    CategoryColor::ALL
        .iter()
        .position(|c| *c == color)
        .map_or(-1, |i| i as i32 - 1)
}

/// IMAP folder commands, run on the account's interactive connection.
enum ImapCommand {
    Create(String),
    Rename(String, String),
    /// Deletes folders (deepest first), first moving their mail to `trash`.
    Delete {
        paths: Vec<String>,
        trash: Option<String>,
    },
}

impl ImapCommand {
    async fn run(&self, c: &mut envelope_imap::Client) -> Result<()> {
        match self {
            ImapCommand::Create(path) => {
                c.create(path).await?;
                let _ = c.subscribe(path).await;
            }
            ImapCommand::Rename(from, to) => {
                leave(c, from).await?;
                c.rename(from, to).await?;
                let _ = c.subscribe(to).await;
            }
            ImapCommand::Delete { paths, trash } => {
                for path in paths {
                    // Mail goes to Trash first, so it can still be found there.
                    if let Some(trash) = trash {
                        match c.select(path, None).await {
                            Ok(selected) if selected.exists > 0 => {
                                let uids = c.uid_search("ALL").await?;
                                c.uid_move(&uids, trash).await?;
                            }
                            Ok(_) => {}
                            // A folder that only holds folders.
                            Err(envelope_imap::Error::No { .. }) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    leave(c, path).await?;
                    c.delete(path).await?;
                }
            }
        }
        Ok(())
    }
}

/// Deselects a folder before it is renamed or deleted.
async fn leave(c: &mut envelope_imap::Client, path: &str) -> Result<()> {
    if c.selected() == Some(path) {
        if c.has_cap("UNSELECT") {
            c.unselect().await?;
        } else {
            c.examine("INBOX").await?;
        }
    }
    Ok(())
}

impl Engine {
    /// Runs a folder command, once more on a fresh connection when the
    /// interactive one went stale.
    async fn imap_command(&self, account_id: i64, command: &ImapCommand) -> Result<()> {
        match self
            .with_imap(account_id, async |_, c| command.run(c).await)
            .await
        {
            Err(e) if e.is_transient() => {
                self.with_imap(account_id, async |_, c| command.run(c).await)
                    .await
            }
            other => other,
        }
    }

    fn folder_row(&self, id: i64) -> Result<FolderRow> {
        self.db.read(|c| mail::get_folder(c, id))
    }

    fn folder_view(&self, id: i64) -> Result<Folder> {
        self.db.read(|c| mail::folder_view(c, id))
    }

    /// Creates a folder at the top of an account or inside `parent_id`. In
    /// Gmail it is a label; with `label` in Microsoft and Exchange accounts
    /// it is an Outlook category. `color` is a label's color.
    pub async fn create_folder(
        &self,
        account_id: i64,
        parent_id: Option<i64>,
        name: &str,
        label: bool,
        color: Option<String>,
    ) -> Result<Folder> {
        let account = self.account_row(account_id)?;
        let parent = parent_id.map(|id| self.folder_row(id)).transpose()?;
        if parent.as_ref().is_some_and(|p| p.account_id != account_id) {
            return Err(invalid("The folder belongs to another account"));
        }
        let lock = self.folder_lock(account_id);
        let guard = lock.lock().await;
        let folders = self.db.read(|c| mail::list_folder_rows(c, account_id))?;
        let id = match account.kind {
            AccountKind::Imap if label => {
                return Err(invalid("This account has folders, not labels"));
            }
            AccountKind::Imap | AccountKind::Gmail => {
                self.imap_create(&account, &folders, parent.as_ref(), name, color)
                    .await?
            }
            AccountKind::Microsoft => {
                let graph = GraphMail::for_account(self, account_id)?;
                if label {
                    self.graph_create_category(&graph, account_id, &folders, name, color)
                        .await?
                } else {
                    self.graph_create_folder(&graph, account_id, &folders, parent.as_ref(), name)
                        .await?
                }
            }
            AccountKind::Exchange if label => {
                // Categories are names on messages; the master list is optional.
                self.local_category(account_id, &folders, name, color)?
            }
            AccountKind::Exchange => {
                let ews = EwsMail::for_account(self, &account).await?;
                self.ews_create_folder(&ews, account_id, &folders, parent.as_ref(), name)
                    .await?
            }
        };
        drop(guard);
        self.folders_changed(account_id);
        self.folder_view(id)
    }

    /// Renames a folder or a Gmail label (Outlook categories keep their names).
    pub async fn rename_folder(&self, folder_id: i64, name: &str) -> Result<()> {
        let folder = self.folder_row(folder_id)?;
        if protected(&folder) {
            return Err(invalid("Special folders keep their names"));
        }
        let account = self.account_row(folder.account_id)?;
        let lock = self.folder_lock(account.id);
        let guard = lock.lock().await;
        let folders = self.db.read(|c| mail::list_folder_rows(c, account.id))?;
        match account.kind {
            AccountKind::Imap | AccountKind::Gmail => {
                let delimiter = imap_delimiter(&folders, Some(&folder));
                let name = clean_name(name, Some(&delimiter))?;
                let path = match folder.remote_id.rsplit_once(delimiter.as_str()) {
                    Some((parent, _)) => format!("{parent}{delimiter}{name}"),
                    None => name.clone(),
                };
                self.imap_rehome(&account, &folders, &folder, &name, &path)
                    .await?;
            }
            AccountKind::Microsoft | AccountKind::Exchange if folder.kind == "label" => {
                return Err(invalid("Outlook categories can't be renamed"));
            }
            AccountKind::Microsoft => {
                let graph = GraphMail::for_account(self, account.id)?;
                self.graph_rename_folder(&graph, &folders, &folder, name)
                    .await?;
            }
            AccountKind::Exchange => {
                let ews = EwsMail::for_account(self, &account).await?;
                self.ews_rename_folder(&ews, &folders, &folder, name)
                    .await?;
            }
        }
        drop(guard);
        self.folders_changed(account.id);
        Ok(())
    }

    /// Moves a folder inside another folder of the same account, or to the
    /// top with `parent_id` `None`.
    pub async fn move_folder(&self, folder_id: i64, parent_id: Option<i64>) -> Result<()> {
        let folder = self.folder_row(folder_id)?;
        if protected(&folder) {
            return Err(invalid("Special folders stay where they are"));
        }
        let account = self.account_row(folder.account_id)?;
        let lock = self.folder_lock(account.id);
        let guard = lock.lock().await;
        let subtree = self.db.read(|c| mail::folder_subtree(c, folder_id))?;
        let parent = parent_id.map(|id| self.folder_row(id)).transpose()?;
        if let Some(p) = &parent {
            if p.account_id != account.id {
                return Err(invalid("Folders can only move within their account"));
            }
            if subtree.iter().any(|f| f.id == p.id) {
                return Err(invalid("A folder can't go inside itself"));
            }
            let fits = match account.kind {
                AccountKind::Gmail => p.kind == "label" && !gmail_system(p),
                _ => folder.kind == "folder" && p.kind == "folder",
            };
            if !fits {
                return Err(invalid("The folder can't go there"));
            }
        }
        if folder.parent_id == parent_id {
            return Ok(());
        }
        let folders = self.db.read(|c| mail::list_folder_rows(c, account.id))?;
        match account.kind {
            AccountKind::Imap | AccountKind::Gmail => {
                let delimiter = imap_delimiter(&folders, Some(&folder));
                let path = match &parent {
                    Some(p) => format!("{}{delimiter}{}", p.remote_id, folder.name),
                    None => format!(
                        "{}{}",
                        imap_top_prefix(&folders, &delimiter, account.kind == AccountKind::Gmail),
                        folder.name
                    ),
                };
                self.imap_rehome(&account, &folders, &folder, &folder.name, &path)
                    .await?;
            }
            AccountKind::Microsoft => {
                let graph = GraphMail::for_account(self, account.id)?;
                self.graph_move_folder(&graph, &folders, &folder, parent.as_ref())
                    .await?;
            }
            AccountKind::Exchange => {
                let ews = EwsMail::for_account(self, &account).await?;
                self.ews_move_folder(&ews, &folders, &folder, parent.as_ref())
                    .await?;
            }
        }
        drop(guard);
        self.folders_changed(account.id);
        Ok(())
    }

    /// Deletes a folder with the folders inside it; their conversations move
    /// to Trash. A Gmail label or an Outlook category is only taken off its
    /// conversations.
    pub async fn delete_folder(&self, folder_id: i64) -> Result<()> {
        let folder = self.folder_row(folder_id)?;
        if protected(&folder) {
            return Err(invalid("Special folders can't be deleted"));
        }
        let account = self.account_row(folder.account_id)?;
        let lock = self.folder_lock(account.id);
        let guard = lock.lock().await;
        let folders = self.db.read(|c| mail::list_folder_rows(c, account.id))?;
        let trash = folders
            .iter()
            .find(|f| f.role == Some(FolderRole::Trash))
            .cloned();
        match account.kind {
            AccountKind::Imap | AccountKind::Gmail => {
                let subtree = self.db.read(|c| mail::folder_subtree(c, folder_id))?;
                let command = ImapCommand::Delete {
                    paths: subtree.iter().rev().map(|f| f.remote_id.clone()).collect(),
                    // Gmail mail stays in All Mail without the label.
                    trash: trash
                        .filter(|_| account.kind == AccountKind::Imap)
                        .map(|t| t.remote_id),
                };
                self.imap_command(account.id, &command).await?;
                self.db
                    .write(|tx| mail::delete_folder_tree(tx, folder_id))?;
            }
            AccountKind::Microsoft | AccountKind::Exchange if folder.kind == "label" => {
                let name = category_name(&folder).unwrap_or(&folder.name).to_string();
                if account.kind == AccountKind::Microsoft {
                    let graph = GraphMail::for_account(self, account.id)?;
                    graph_delete_category(&graph, &name).await?;
                } else {
                    let ews = EwsMail::for_account(self, &account).await?;
                    ews_sync::edit_category(&ews, &name, CategoryEdit::Remove).await?;
                }
                self.drop_category(account.id, &folder, &name)?;
            }
            AccountKind::Microsoft => {
                let graph = GraphMail::for_account(self, account.id)?;
                self.graph_delete_folder(&graph, &folder, trash.as_ref())
                    .await?;
            }
            AccountKind::Exchange => {
                let ews = EwsMail::for_account(self, &account).await?;
                self.ews_delete_folder(&ews, &folder, trash.as_ref())
                    .await?;
            }
        }
        drop(guard);
        self.folders_changed(account.id);
        self.emit(EngineEvent::MailChanged {
            account_id: account.id,
            folder_ids: Vec::new(),
            thread_ids: Vec::new(),
        });
        Ok(())
    }

    /// Sets (or clears) the color of a folder or label. Outlook categories
    /// change on the server too (to the nearest Outlook color); the others
    /// keep their color on this computer.
    pub async fn set_folder_color(&self, folder_id: i64, color: Option<String>) -> Result<()> {
        let folder = self.folder_row(folder_id)?;
        let account = self.account_row(folder.account_id)?;
        let mut color = color.filter(|c| !c.trim().is_empty());
        if folder.kind == "label"
            && let Some(name) = category_name(&folder)
        {
            let preset = nearest_preset(color.as_deref());
            match account.kind {
                AccountKind::Microsoft => {
                    let graph = GraphMail::for_account(self, account.id)?;
                    graph_recolor_category(&graph, name, preset).await?;
                }
                AccountKind::Exchange => {
                    let ews = EwsMail::for_account(self, &account).await?;
                    ews_sync::edit_category(
                        &ews,
                        name,
                        CategoryEdit::Recolor(preset_number(preset)),
                    )
                    .await?;
                }
                _ => {}
            }
            // What Outlook shows, so the next sync agrees.
            color = preset.hex().map(str::to_string);
        }
        self.db.write(|tx| {
            tx.execute(
                "UPDATE folders SET color = ? WHERE id = ?",
                params![color, folder_id],
            )?;
            Ok(())
        })?;
        self.emit(EngineEvent::FoldersChanged {
            account_id: account.id,
        });
        Ok(())
    }

    /// Deletes everything in Trash or Spam, with the folders inside it, for
    /// good. Returns how many conversations went.
    pub fn empty_folder(&self, folder_id: i64) -> Result<i64> {
        let folder = self.folder_row(folder_id)?;
        if !matches!(folder.role, Some(FolderRole::Trash | FolderRole::Junk)) {
            return Err(invalid("Only Trash and Spam can be emptied"));
        }
        let account = self.account_row(folder.account_id)?;
        let imap = matches!(account.kind, AccountKind::Imap | AccountKind::Gmail);
        let (threads, nested) = self.db.write(|tx| {
            let subtree = mail::folder_subtree(tx, folder_id)?;
            let mut threads = BTreeSet::new();
            for f in &subtree {
                let mut st =
                    tx.prepare_cached("SELECT thread_id FROM thread_folders WHERE folder_id = ?")?;
                for t in st.query_map([f.id], |r| r.get::<_, i64>(0))? {
                    threads.insert(t?);
                }
            }
            // Everything the server had when we last looked, synced here or not.
            let known: Option<u32> = tx.query_row(
                "SELECT max(uid) FROM message_folders WHERE folder_id = ?",
                [folder_id],
                |r| r.get(0),
            )?;
            let listed = folder.sync_state.uid_next.map(|n| n.saturating_sub(1));
            let max_uid = known.max(listed).filter(|m| imap && *m > 0);
            let messages: Vec<i64> = {
                let mut st =
                    tx.prepare("SELECT message_id FROM message_folders WHERE folder_id = ?")?;
                st.query_map([folder_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for m in messages {
                mail::remove_from_folder(tx, folder_id, m)?;
            }
            let nested: Vec<&FolderRow> = subtree[1..].iter().rev().collect();
            for f in &nested {
                mail::delete_folder(tx, f.id)?;
            }
            let mut touched = mail::refresh_threads(tx, &threads)?;
            touched.insert(folder_id);
            mail::refresh_folder_counts(tx, &touched)?;
            misc::queue_op(
                tx,
                account.id,
                &PendingOp::EmptyFolder {
                    folder: folder.remote_id.clone(),
                    max_uid,
                    subfolders: nested.iter().map(|f| f.remote_id.clone()).collect(),
                },
                0,
            )?;
            Ok((threads, !nested.is_empty()))
        })?;
        self.after_local_change(&[account.id], &threads)?;
        if nested {
            self.emit(EngineEvent::FoldersChanged {
                account_id: account.id,
            });
        }
        Ok(threads.len() as i64)
    }

    // ----- IMAP and Gmail ----------------------------------------------------

    async fn imap_create(
        &self,
        account: &AccountRow,
        folders: &[FolderRow],
        parent: Option<&FolderRow>,
        name: &str,
        color: Option<String>,
    ) -> Result<i64> {
        let gmail = account.kind == AccountKind::Gmail;
        if let Some(p) = parent
            && gmail
            && (p.kind != "label" || gmail_system(p))
        {
            return Err(invalid("Labels can only go inside other labels"));
        }
        let delimiter = imap_delimiter(folders, parent);
        let name = clean_name(name, Some(&delimiter))?;
        let path = match parent {
            Some(p) => format!("{}{delimiter}{name}", p.remote_id),
            None => format!("{}{name}", imap_top_prefix(folders, &delimiter, gmail)),
        };
        let parent_id = imap_parent(folders, &path, &delimiter);
        let kind = if gmail { "label" } else { "folder" };
        check_free(folders, parent_id, kind, &name, None)?;
        self.imap_command(account.id, &ImapCommand::Create(path.clone()))
            .await?;
        let upsert = FolderUpsert {
            remote_id: path.clone(),
            name,
            path,
            parent_remote_id: None,
            role: None,
            kind: kind.into(),
            delimiter: Some(delimiter),
            selectable: true,
            sort_order: folder_priority(None) as i64,
            color: color.filter(|_| gmail),
        };
        self.db
            .write(|tx| mail::upsert_folder(tx, account.id, &upsert, parent_id))
    }

    /// Renames or moves an IMAP folder to `path`, with the folders inside it,
    /// and points queued operations at the new place.
    async fn imap_rehome(
        &self,
        account: &AccountRow,
        folders: &[FolderRow],
        folder: &FolderRow,
        name: &str,
        path: &str,
    ) -> Result<()> {
        let delimiter = imap_delimiter(folders, Some(folder));
        let parent_id = imap_parent(folders, path, &delimiter);
        check_free(folders, parent_id, &folder.kind, name, Some(folder.id))?;
        if path == folder.remote_id {
            return Ok(());
        }
        let old = folder.remote_id.clone();
        self.imap_command(
            account.id,
            &ImapCommand::Rename(old.clone(), path.to_string()),
        )
        .await?;
        self.db.write(|tx| {
            mail::rehome_folder(tx, folder.id, path, name, path, parent_id, true)?;
            misc::rename_folder_in_ops(tx, account.id, &old, path, &delimiter)
        })
    }

    // ----- Microsoft Graph ------------------------------------------------------

    pub(crate) async fn graph_create_folder(
        &self,
        graph: &GraphMail,
        account_id: i64,
        folders: &[FolderRow],
        parent: Option<&FolderRow>,
        name: &str,
    ) -> Result<i64> {
        let name = clean_name(name, None)?;
        check_free(folders, parent.map(|p| p.id), "folder", &name, None)?;
        let created = graph
            .client()
            .create_mail_folder(parent.map(|p| p.remote_id.as_str()), &name)
            .await
            .map_err(|e| graph.err(e))?;
        let path = match parent {
            Some(p) => format!("{}/{name}", p.path),
            None => name.clone(),
        };
        let upsert = FolderUpsert {
            remote_id: created.id,
            name,
            path,
            parent_remote_id: None,
            role: None,
            kind: "folder".into(),
            delimiter: Some("/".into()),
            selectable: true,
            sort_order: folder_priority(None) as i64,
            color: None,
        };
        self.db
            .write(|tx| mail::upsert_folder(tx, account_id, &upsert, parent.map(|p| p.id)))
    }

    pub(crate) async fn graph_create_category(
        &self,
        graph: &GraphMail,
        account_id: i64,
        folders: &[FolderRow],
        name: &str,
        color: Option<String>,
    ) -> Result<i64> {
        let name = clean_name(name, None)?;
        check_free(folders, None, "label", &name, None)?;
        let created = graph
            .client()
            .create_category(&name, nearest_preset(color.as_deref()))
            .await
            .map_err(|e| graph.err(e))?;
        let upsert = category_upsert(
            created.display_name.trim(),
            created.color.hex().map(str::to_string),
        );
        self.db
            .write(|tx| mail::upsert_folder(tx, account_id, &upsert, None))
    }

    pub(crate) async fn graph_rename_folder(
        &self,
        graph: &GraphMail,
        folders: &[FolderRow],
        folder: &FolderRow,
        name: &str,
    ) -> Result<()> {
        let name = clean_name(name, None)?;
        check_free(folders, folder.parent_id, "folder", &name, Some(folder.id))?;
        let renamed = graph
            .client()
            .rename_mail_folder(&folder.remote_id, &name)
            .await
            .map_err(|e| graph.err(e))?;
        let path = match folder.path.rsplit_once('/') {
            Some((parent, _)) if folder.parent_id.is_some() => format!("{parent}/{name}"),
            _ => name.clone(),
        };
        self.db.write(|tx| {
            mail::rehome_folder(
                tx,
                folder.id,
                &renamed.id,
                &name,
                &path,
                folder.parent_id,
                false,
            )
        })
    }

    pub(crate) async fn graph_move_folder(
        &self,
        graph: &GraphMail,
        folders: &[FolderRow],
        folder: &FolderRow,
        parent: Option<&FolderRow>,
    ) -> Result<()> {
        check_free(
            folders,
            parent.map(|p| p.id),
            "folder",
            &folder.name,
            Some(folder.id),
        )?;
        let destination = parent.map_or("msgfolderroot", |p| p.remote_id.as_str());
        let moved = graph
            .client()
            .move_mail_folder(&folder.remote_id, destination)
            .await
            .map_err(|e| graph.err(e))?;
        let path = match parent {
            Some(p) => format!("{}/{}", p.path, folder.name),
            None => folder.name.clone(),
        };
        self.db.write(|tx| {
            mail::rehome_folder(
                tx,
                folder.id,
                &moved.id,
                &folder.name,
                &path,
                parent.map(|p| p.id),
                false,
            )
        })
    }

    /// Moves the folder into Deleted Items, as Outlook does; emptying it
    /// deletes the folder for good.
    pub(crate) async fn graph_delete_folder(
        &self,
        graph: &GraphMail,
        folder: &FolderRow,
        trash: Option<&FolderRow>,
    ) -> Result<()> {
        let Some(trash) = trash else {
            graph
                .client()
                .delete_mail_folder(&folder.remote_id)
                .await
                .map_err(|e| graph.err(e))?;
            return self.db.write(|tx| mail::delete_folder_tree(tx, folder.id));
        };
        let moved = graph
            .client()
            .move_mail_folder(&folder.remote_id, &trash.remote_id)
            .await
            .map_err(|e| graph.err(e))?;
        self.db.write(|tx| {
            mail::rehome_folder(
                tx,
                folder.id,
                &moved.id,
                &folder.name,
                &format!("{}/{}", trash.path, folder.name),
                Some(trash.id),
                false,
            )
        })
    }

    // ----- Exchange ---------------------------------------------------------------

    pub(crate) async fn ews_create_folder(
        &self,
        ews: &EwsMail,
        account_id: i64,
        folders: &[FolderRow],
        parent: Option<&FolderRow>,
        name: &str,
    ) -> Result<i64> {
        use envelope_ews::{DistinguishedFolder, FolderId, FolderRef};
        let name = clean_name(name, None)?;
        check_free(folders, parent.map(|p| p.id), "folder", &name, None)?;
        let target: FolderRef = match parent {
            Some(p) => FolderId::new(p.remote_id.clone()).into(),
            None => DistinguishedFolder::MsgFolderRoot.into(),
        };
        let id = ews
            .client()
            .create_folder(target, &name, envelope_ews::FOLDER_CLASS_MAIL)
            .await
            .map_err(|e| ews.err(e))?;
        let path = match parent {
            Some(p) => format!("{}/{name}", p.path),
            None => name.clone(),
        };
        let upsert = FolderUpsert {
            remote_id: id.id,
            name,
            path,
            parent_remote_id: None,
            role: None,
            kind: "folder".into(),
            delimiter: Some("/".into()),
            selectable: true,
            sort_order: folder_priority(None) as i64,
            color: None,
        };
        self.db
            .write(|tx| mail::upsert_folder(tx, account_id, &upsert, parent.map(|p| p.id)))
    }

    pub(crate) async fn ews_rename_folder(
        &self,
        ews: &EwsMail,
        folders: &[FolderRow],
        folder: &FolderRow,
        name: &str,
    ) -> Result<()> {
        let name = clean_name(name, None)?;
        check_free(folders, folder.parent_id, "folder", &name, Some(folder.id))?;
        let renamed = ews
            .client()
            .rename_folder(
                &envelope_ews::FolderId::new(folder.remote_id.clone()),
                &name,
            )
            .await
            .map_err(|e| ews.err(e))?;
        let path = match folder.path.rsplit_once('/') {
            Some((parent, _)) if folder.parent_id.is_some() => format!("{parent}/{name}"),
            _ => name.clone(),
        };
        self.db.write(|tx| {
            mail::rehome_folder(
                tx,
                folder.id,
                &renamed.id,
                &name,
                &path,
                folder.parent_id,
                false,
            )
        })
    }

    pub(crate) async fn ews_move_folder(
        &self,
        ews: &EwsMail,
        folders: &[FolderRow],
        folder: &FolderRow,
        parent: Option<&FolderRow>,
    ) -> Result<()> {
        use envelope_ews::{DistinguishedFolder, FolderId, FolderRef};
        check_free(
            folders,
            parent.map(|p| p.id),
            "folder",
            &folder.name,
            Some(folder.id),
        )?;
        let target: FolderRef = match parent {
            Some(p) => FolderId::new(p.remote_id.clone()).into(),
            None => DistinguishedFolder::MsgFolderRoot.into(),
        };
        let moved = ews
            .client()
            .move_folder(&FolderId::new(folder.remote_id.clone()), target)
            .await
            .map_err(|e| ews.err(e))?;
        let path = match parent {
            Some(p) => format!("{}/{}", p.path, folder.name),
            None => folder.name.clone(),
        };
        self.db.write(|tx| {
            mail::rehome_folder(
                tx,
                folder.id,
                &moved.id,
                &folder.name,
                &path,
                parent.map(|p| p.id),
                false,
            )
        })
    }

    /// Moves the folder into Deleted Items, as Outlook does.
    pub(crate) async fn ews_delete_folder(
        &self,
        ews: &EwsMail,
        folder: &FolderRow,
        trash: Option<&FolderRow>,
    ) -> Result<()> {
        use envelope_ews::{DeleteType, FolderId};
        ews.client()
            .delete_folder(
                FolderId::new(folder.remote_id.clone()),
                DeleteType::MoveToDeletedItems,
            )
            .await
            .map_err(|e| ews.err(e))?;
        self.db.write(|tx| match trash {
            Some(trash) => mail::rehome_folder(
                tx,
                folder.id,
                &folder.remote_id,
                &folder.name,
                &format!("{}/{}", trash.path, folder.name),
                Some(trash.id),
                false,
            ),
            None => mail::delete_folder_tree(tx, folder.id),
        })
    }

    // ----- Outlook categories -------------------------------------------------------

    /// A category that only exists on messages (Exchange keeps no list of them).
    fn local_category(
        &self,
        account_id: i64,
        folders: &[FolderRow],
        name: &str,
        color: Option<String>,
    ) -> Result<i64> {
        let name = clean_name(name, None)?;
        check_free(folders, None, "label", &name, None)?;
        let mut upsert = category_upsert(&name, None);
        upsert.color = color;
        self.db
            .write(|tx| mail::upsert_folder(tx, account_id, &upsert, None))
    }

    /// Takes a deleted category off the conversations that had it.
    pub(crate) fn drop_category(
        &self,
        account_id: i64,
        folder: &FolderRow,
        name: &str,
    ) -> Result<()> {
        let threads = self.db.write(|tx| {
            let message_ids: Vec<i64> = {
                let mut st =
                    tx.prepare("SELECT message_id FROM message_folders WHERE folder_id = ?")?;
                st.query_map([folder.id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let threads: BTreeSet<i64> = {
                let mut st =
                    tx.prepare("SELECT thread_id FROM thread_folders WHERE folder_id = ?")?;
                st.query_map([folder.id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            if !message_ids.is_empty() {
                misc::queue_op(
                    tx,
                    account_id,
                    &PendingOp::Labels {
                        message_ids,
                        add: Vec::new(),
                        remove: vec![name.to_string()],
                    },
                    0,
                )?;
            }
            mail::delete_folder_tree(tx, folder.id)?;
            Ok(threads)
        })?;
        self.after_local_change(&[account_id], &threads)
    }
}

/// Finds a category of the master list by name.
async fn graph_category(graph: &GraphMail, name: &str) -> Result<Option<String>> {
    let list = graph
        .client()
        .master_categories()
        .await
        .map_err(|e| graph.err(e))?;
    Ok(list
        .into_iter()
        .find(|c| c.display_name.trim().eq_ignore_ascii_case(name.trim()))
        .map(|c| c.id))
}

pub(crate) async fn graph_delete_category(graph: &GraphMail, name: &str) -> Result<()> {
    if let Some(id) = graph_category(graph, name).await? {
        graph
            .client()
            .delete_category(&id)
            .await
            .map_err(|e| graph.err(e))?;
    }
    Ok(())
}

pub(crate) async fn graph_recolor_category(
    graph: &GraphMail,
    name: &str,
    color: CategoryColor,
) -> Result<()> {
    match graph_category(graph, name).await? {
        Some(id) => graph
            .client()
            .set_category_color(&id, color)
            .await
            .map_err(|e| graph.err(e)),
        // Only on messages: add it to the list with its color.
        None => graph
            .client()
            .create_category(name, color)
            .await
            .map(drop)
            .map_err(|e| graph.err(e)),
    }
}

#[cfg(test)]
mod tests;

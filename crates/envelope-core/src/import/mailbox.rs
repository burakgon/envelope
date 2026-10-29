//! Messages from mbox files (Thunderbird's local folders, archives, Google
//! Takeout) and saved messages (`.eml`), uploaded into a folder of an
//! account, where they sync like any other mail.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::io::AsyncBufReadExt;

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{AccountKind, EngineEvent};

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagesImported {
    /// Messages put into the folder.
    pub added: usize,
    /// Messages the account has already (by Message-ID).
    pub skipped: usize,
    /// Messages the server refused.
    pub refused: usize,
}

/// Reads the messages of a file one at a time: an mbox file (its messages
/// start at `From ` lines, at the top or after an empty line, and `>From `
/// lines in them are unescaped) or a single saved message. Lines are bytes:
/// old mail has 8-bit text in other character sets.
struct Messages {
    reader: tokio::io::BufReader<tokio::fs::File>,
    mbox: bool,
    /// The first line of the next message, already read.
    pending: Option<Vec<u8>>,
    done: bool,
}

impl Messages {
    async fn open(path: &Path) -> Result<Messages> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| Error::NotFound(format!("{} could not be opened.", path.display())))?;
        let mut messages = Messages {
            reader: tokio::io::BufReader::new(file),
            mbox: false,
            pending: None,
            done: false,
        };
        let first = messages.line().await?;
        messages.mbox = first.as_deref().is_some_and(|l| l.starts_with(b"From "));
        // The separator line of an mbox file is not part of its message.
        messages.pending = if messages.mbox { None } else { first };
        messages.done = messages.pending.is_none() && !messages.mbox;
        Ok(messages)
    }

    /// The next line without its line end; `None` at the end of the file.
    async fn line(&mut self) -> Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        if self.reader.read_until(b'\n', &mut line).await? == 0 {
            return Ok(None);
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        Ok(Some(line))
    }

    /// The next message, with CRLF line ends; `None` at the end of the file.
    async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if self.done {
                return Ok(None);
            }
            let mut message = Vec::new();
            let mut blank_before = false;
            if let Some(line) = self.pending.take() {
                push_line(&mut message, &line, self.mbox);
            }
            loop {
                let Some(line) = self.line().await? else {
                    self.done = true;
                    break;
                };
                if self.mbox && blank_before && line.starts_with(b"From ") {
                    break;
                }
                blank_before = line.is_empty();
                push_line(&mut message, &line, self.mbox);
            }
            // The empty line that separates messages in mbox files.
            while message.ends_with(b"\r\n\r\n") {
                message.truncate(message.len() - 2);
            }
            if !message.iter().all(u8::is_ascii_whitespace) {
                return Ok(Some(message));
            }
        }
    }
}

fn push_line(message: &mut Vec<u8>, line: &[u8], mbox: bool) {
    // mboxrd: each ">From " line got one ">" more when it was written.
    let unescaped = match line.strip_prefix(b">") {
        Some(rest)
            if mbox
                && rest
                    .iter()
                    .skip_while(|b| **b == b'>')
                    .copied()
                    .take(5)
                    .eq(*b"From ") =>
        {
            rest
        }
        _ => line,
    };
    message.extend_from_slice(unescaped);
    message.extend_from_slice(b"\r\n");
}

/// The Message-ID and date of a message, from its headers.
fn identity(
    message: &[u8],
) -> (
    Option<String>,
    Option<chrono::DateTime<chrono::FixedOffset>>,
) {
    let Some(parsed) = mail_parser::MessageParser::new().parse_headers(message) else {
        return (None, None);
    };
    let id = parsed.message_id().map(str::to_string);
    let date = parsed
        .date()
        .and_then(|d| chrono::DateTime::parse_from_rfc3339(&d.to_rfc3339()).ok());
    (id, date)
}

impl Engine {
    /// Uploads the messages of mbox files and saved messages (`.eml`) into a
    /// folder, as read mail; messages the account already has (same
    /// Message-ID) are left out. Microsoft 365 accounts can't take messages
    /// from apps.
    pub async fn import_messages(
        &self,
        folder_id: i64,
        paths: &[PathBuf],
    ) -> Result<MessagesImported> {
        let folder = self
            .db
            .read(|c| crate::db::mail::get_folder(c, folder_id))?;
        let account = self.account_row(folder.account_id)?;
        let known = |id: &str| -> Result<bool> {
            self.db.read(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM messages WHERE account_id = ? AND message_id = ?)",
                    rusqlite::params![account.id, id.trim_matches(['<', '>'])],
                    |r| r.get::<_, bool>(0),
                )?)
            })
        };
        let mut result = MessagesImported::default();
        let progress = |result: &MessagesImported| {
            if (result.added + result.skipped + result.refused) % 25 == 0 {
                self.emit(EngineEvent::ImportProgress {
                    folder_id,
                    done: result.added + result.skipped + result.refused,
                });
            }
        };
        match account.kind {
            AccountKind::Imap | AccountKind::Gmail => {
                self.with_imap(account.id, async |_, c| {
                    for path in paths {
                        let mut messages = Messages::open(path).await?;
                        while let Some(message) = messages.next().await? {
                            let (id, date) = identity(&message);
                            if id.as_deref().is_some_and(|id| known(id).unwrap_or(false)) {
                                result.skipped += 1;
                            } else {
                                match c
                                    .append(&folder.remote_id, &["\\Seen"], date, &message)
                                    .await
                                {
                                    Ok(_) => result.added += 1,
                                    // Refused (too large, malformed): the next one may go.
                                    Err(
                                        envelope_imap::Error::No { .. }
                                        | envelope_imap::Error::Bad { .. },
                                    ) => result.refused += 1,
                                    Err(e) => return Err(e.into()),
                                }
                            }
                            progress(&result);
                        }
                    }
                    Ok(())
                })
                .await?;
            }
            AccountKind::Exchange => {
                let ews = crate::sync::ews::EwsMail::for_account(self, &account).await?;
                let target = envelope_ews::MimeDisposition::Import {
                    folder: envelope_ews::FolderRef::Id(envelope_ews::FolderId {
                        id: folder.remote_id.clone(),
                        change_key: None,
                    }),
                    is_read: true,
                };
                for path in paths {
                    let mut messages = Messages::open(path).await?;
                    while let Some(message) = messages.next().await? {
                        let (id, _) = identity(&message);
                        if id.as_deref().is_some_and(|id| known(id).unwrap_or(false)) {
                            result.skipped += 1;
                        } else {
                            match ews.client().create_mime_message(&message, &target).await {
                                Ok(_) => result.added += 1,
                                Err(envelope_ews::EwsError::Response(_)) => result.refused += 1,
                                Err(e) => return Err(crate::sync::ews::ews_error(e)),
                            }
                        }
                        progress(&result);
                    }
                }
            }
            AccountKind::Microsoft => {
                return Err(Error::Invalid(
                    "Microsoft 365 doesn't let apps import messages. Import them into another account, or with Outlook.".into(),
                ));
            }
        }
        // The new messages come in with the folder's next sync.
        self.wake(account.id);
        Ok(result)
    }
}

#[cfg(test)]
mod tests;

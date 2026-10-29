//! Subscriptions: the senders of newsletters and other list mail, to look
//! over in one place and unsubscribe from or clear out, sender by sender.

use std::collections::HashSet;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::error::Result;

/// A sender of list mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    /// In lowercase.
    pub email: String,
    pub name: Option<String>,
    /// Conversations with their mail, outside Trash and Spam.
    pub conversations: i64,
    /// Of those, the ones where their mail is unread.
    pub unread: i64,
    /// Of those, the ones in an inbox.
    pub in_inbox: i64,
    /// When their newest message came.
    pub last_date: i64,
    /// Their newest message that says how to unsubscribe.
    pub unsubscribe_message_id: Option<i64>,
    /// Unsubscribing takes one request (RFC 8058) instead of a web page.
    pub one_click: bool,
}

impl Engine {
    /// The senders of list mail, the one with the most conversations first.
    pub async fn subscriptions(&self) -> Result<Vec<Subscription>> {
        self.db.read_async(list).await
    }
}

/// Mail is list mail when it names its list (`List-Id`) or how to leave it
/// (`List-Unsubscribe`). Mail from the user's own addresses is not counted,
/// nor are conversations in Trash or Spam.
fn list(c: &Connection) -> Result<Vec<Subscription>> {
    let mut stmt = c.prepare(
        "SELECT m.id, m.thread_id, m.from_email, m.from_name, m.date, (m.flags & 1) = 0,
                m.list_unsubscribe IS NOT NULL, m.unsubscribe_post != 0, m.list_id,
                EXISTS (SELECT 1 FROM thread_folders y JOIN folders yf ON yf.id = y.folder_id
                        WHERE y.thread_id = m.thread_id AND yf.role = 'inbox')
         FROM messages m JOIN accounts a ON a.id = m.account_id AND a.enabled = 1
         WHERE m.from_email IS NOT NULL AND m.from_email != ''
           AND (m.list_unsubscribe IS NOT NULL OR m.list_id IS NOT NULL)
           AND m.from_email NOT IN (SELECT lower(trim(email)) FROM identities
                                    UNION SELECT lower(trim(email)) FROM accounts)
           AND NOT EXISTS (SELECT 1 FROM thread_folders z JOIN folders zf ON zf.id = z.folder_id
                           WHERE z.thread_id = m.thread_id AND zf.role IN ('trash', 'junk'))
         ORDER BY m.from_email, m.date DESC",
    )?;
    let mut rows = stmt.query([])?;
    let mut out: Vec<Subscription> = Vec::new();
    // The conversations counted for the sender being read, and the names and lists of its mail.
    let (mut all, mut unread, mut inbox) = (HashSet::new(), HashSet::new(), HashSet::new());
    let (mut names, mut lists) = (Vec::<String>::new(), Vec::<Option<String>>::new());
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let thread: i64 = r.get(1)?;
        let email: String = r.get(2)?;
        let name: Option<String> = r.get(3)?;
        let date: i64 = r.get(4)?;
        let (is_unread, can_leave, one_click, list_id, in_inbox): (
            bool,
            bool,
            bool,
            Option<String>,
            bool,
        ) = (r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?);
        if out.last().is_none_or(|s| s.email != email) {
            if let Some(s) = out.last_mut() {
                s.name = name_of(&names, &lists);
            }
            all.clear();
            unread.clear();
            inbox.clear();
            names.clear();
            lists.clear();
            // Newest first: the first row of a sender is their newest message.
            out.push(Subscription {
                email,
                name: None,
                conversations: 0,
                unread: 0,
                in_inbox: 0,
                last_date: date,
                unsubscribe_message_id: None,
                one_click: false,
            });
        }
        let s = out.last_mut().expect("a sender was just added");
        if let Some(name) = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
            && !names.contains(&name)
        {
            names.push(name);
        }
        if !lists.contains(&list_id) {
            lists.push(list_id);
        }
        if can_leave && s.unsubscribe_message_id.is_none() {
            s.unsubscribe_message_id = Some(id);
            s.one_click = one_click;
        }
        all.insert(thread);
        if is_unread {
            unread.insert(thread);
        }
        if in_inbox {
            inbox.insert(thread);
        }
        s.conversations = all.len() as i64;
        s.unread = unread.len() as i64;
        s.in_inbox = inbox.len() as i64;
    }
    if let Some(s) = out.last_mut() {
        s.name = name_of(&names, &lists);
    }
    out.sort_by(|a, b| {
        b.conversations
            .cmp(&a.conversations)
            .then(b.last_date.cmp(&a.last_date))
    });
    Ok(out)
}

/// What a sender is called: the name on its mail when that is always the
/// same, else the name of its list when all of its mail comes from one
/// (notification services put the name of whoever acted on each message).
fn name_of(names: &[String], lists: &[Option<String>]) -> Option<String> {
    match (names, lists) {
        ([name], _) => Some(name.clone()),
        (_, [Some(list)]) => {
            // `List-Id: lumen/envelope <envelope.lumen.gitforge.test>`
            let phrase = list
                .split('<')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .trim();
            (!phrase.is_empty()).then(|| phrase.to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;

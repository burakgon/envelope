//! VIP senders: the people whose mail matters most. Their conversations have
//! a mailbox of their own, `is:vip` finds them, and an account can notify for
//! their mail only.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::Result;
use crate::model::ThreadSummary;

/// A VIP sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vip {
    /// In lowercase.
    pub email: String,
    pub name: Option<String>,
}

/// A message (`m`) from a VIP.
pub(crate) const FROM_VIP: &str = "m.from_email IN (SELECT email FROM vips)";

impl Engine {
    /// The VIPs, by name.
    pub fn vips(&self) -> Result<Vec<Vip>> {
        self.db.read(list)
    }

    /// Makes the sender at `email` a VIP, or no longer one.
    pub fn set_vip(&self, email: &str, name: Option<&str>, vip: bool) -> Result<()> {
        let email = email.trim().to_lowercase();
        if email.is_empty() {
            return Ok(());
        }
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        self.db.write(|tx| {
            if vip {
                tx.execute(
                    "INSERT INTO vips (email, name, added_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(email) DO UPDATE SET name = coalesce(?2, name)",
                    params![email, name, now_ms()],
                )?;
            } else {
                tx.execute("DELETE FROM vips WHERE email = ?", [&email])?;
            }
            Ok(())
        })
    }

    /// Whether the newest message of a conversation is from a VIP.
    pub(crate) fn sent_by_vip(&self, thread: &ThreadSummary) -> bool {
        thread
            .last_sender
            .as_ref()
            .is_some_and(|sender| self.db.read(|c| is_vip(c, &sender.email)).unwrap_or(false))
    }
}

fn list(c: &Connection) -> Result<Vec<Vip>> {
    let mut stmt =
        c.prepare_cached("SELECT email, name FROM vips ORDER BY lower(coalesce(name, email))")?;
    let vips = stmt
        .query_map([], |r| {
            Ok(Vip {
                email: r.get(0)?,
                name: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(vips)
}

fn is_vip(c: &Connection, email: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM vips WHERE email = ?",
        [email.trim().to_lowercase()],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

#[cfg(test)]
mod tests;

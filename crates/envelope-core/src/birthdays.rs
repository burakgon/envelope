//! Birthdays from the address books, for the calendar.

use std::collections::HashSet;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::contacts::format_birthday;
use crate::engine::Engine;
use crate::error::Result;

/// A person's birthday.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Birthday {
    pub contact_id: i64,
    pub name: String,
    /// `YYYY-MM-DD`, or `--MM-DD` when the year is not known.
    pub date: String,
}

impl Engine {
    /// The birthdays in the address books of enabled accounts and of this
    /// computer; someone in two books is listed once.
    pub fn birthdays(&self) -> Result<Vec<Birthday>> {
        self.db.read(list)
    }
}

fn list(c: &Connection) -> Result<Vec<Birthday>> {
    let mut stmt = c.prepare(
        "SELECT c.id, c.name, json_extract(c.data, '$.birthday')
         FROM contacts c JOIN address_books b ON b.id = c.book_id
         LEFT JOIN accounts a ON a.id = b.account_id
         WHERE c.local_state != 3 AND c.is_group = 0 AND (b.account_id IS NULL OR a.enabled = 1)
           AND json_extract(c.data, '$.birthday') IS NOT NULL
         ORDER BY c.sort_key, c.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let (contact_id, name, date) = row?;
        let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else {
            continue;
        };
        let date = format_birthday(date);
        if seen.insert((name.to_lowercase(), date.clone())) {
            out.push(Birthday {
                contact_id,
                name,
                date,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

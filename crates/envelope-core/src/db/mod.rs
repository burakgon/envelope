//! SQLite storage: one writer connection plus a small pool of readers (WAL mode
//! lets them run concurrently), migrations, and typed query modules.

pub mod calendar;
pub mod contacts;
pub mod crypto;
pub mod mail;
pub mod misc;
pub mod smime;
pub mod tasks;

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags, Transaction};

use crate::error::{Error, Result};

const SCHEMA: &str = concat!(
    include_str!("schema.sql"),
    include_str!("sort.sql"),
    include_str!("contacts.sql"),
    include_str!("crypto.sql"),
    include_str!("tasks.sql"),
    include_str!("vips.sql"),
    include_str!("smime.sql")
);
const SCHEMA_VERSION: i64 = 14;

/// Steps from one schema version to the next, applied in order to older databases.
const MIGRATIONS: &[(i64, &str)] = &[
    (
        2,
        "CREATE INDEX IF NOT EXISTS messages_reply ON messages(account_id, in_reply_to) WHERE in_reply_to IS NOT NULL;
         CREATE INDEX IF NOT EXISTS thread_folders_unread ON thread_folders(folder_id, last_date DESC) WHERE unread > 0;",
    ),
    // Address books. The draft `contacts` table of version 1 was never filled.
    (
        3,
        concat!(
            "DROP TABLE IF EXISTS contacts;\n",
            include_str!("contacts.sql")
        ),
    ),
    // Follow-up reminders.
    (
        4,
        "CREATE TABLE IF NOT EXISTS followups (
             message_id TEXT PRIMARY KEY,
             account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
             subject TEXT NOT NULL DEFAULT '',
             since INTEGER NOT NULL,
             due_at INTEGER NOT NULL,
             notified INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS followups_due ON followups(notified, due_at);",
    ),
    // OpenPGP keys, Autocrypt peers and encrypted or signed messages.
    (5, include_str!("crypto.sql")),
    // Task lists and tasks.
    (7, include_str!("tasks.sql")),
    // VIP senders.
    (10, include_str!("vips.sql")),
    // S/MIME certificates.
    (12, include_str!("smime.sql")),
];

/// Columns added to existing tables: (version, table, column, declaration).
/// SQLite cannot add a column only if it is missing, so they are checked.
const NEW_COLUMNS: &[(i64, &str, &str, &str)] = &[
    // Muted conversations.
    (6, "threads", "muted", "INTEGER NOT NULL DEFAULT 0"),
    // Task deletions that can be undone, and creates whose answer got lost.
    (8, "tasks", "deleted_state", "INTEGER"),
    (8, "tasks", "deleted_with", "INTEGER"),
    (8, "tasks", "sent_at", "INTEGER"),
    // Drafts know their server copy by Message-ID, and which version it holds.
    (9, "drafts", "message_id", "TEXT"),
    (9, "drafts", "synced_at", "INTEGER"),
    // Lists by sender, and of messages one by one (filled in by `mail::fill_sort_keys`).
    (13, "threads", "sender", "TEXT"),
    (13, "thread_folders", "sender", "TEXT"),
    (13, "message_folders", "date", "INTEGER"),
    (13, "message_folders", "sender", "TEXT"),
    // How important the sender marked a message.
    (14, "messages", "importance", "INTEGER NOT NULL DEFAULT 0"),
];
const MAX_READERS: usize = 6;

/// Handle to the local database. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: String,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;
         PRAGMA cache_size = -32768;
         PRAGMA busy_timeout = 5000;",
    )?;
    // How a sender sorts, for the queries and triggers of sort.sql.
    conn.create_scalar_function(
        "sort_key",
        2,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| {
            let name = ctx.get::<Option<String>>(0)?;
            let email = ctx.get::<Option<String>>(1)?;
            Ok(mail::sort_key(name.as_deref(), email.as_deref()))
        },
    )?;
    Ok(())
}

impl Db {
    /// Opens (and migrates) the database at `path`. Use `:memory:` for tests.
    pub fn open(path: &Path) -> Result<Db> {
        let path_str = path.to_string_lossy().to_string();
        let in_memory = path_str == ":memory:";
        let uri = if in_memory {
            // A shared in-memory database so readers see the writer's data.
            format!(
                "file:envelope-{}?mode=memory&cache=shared",
                uuid::Uuid::new_v4()
            )
        } else {
            path_str.clone()
        };
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let writer = Connection::open_with_flags(&uri, flags)?;
        configure(&writer)?;
        migrate(&writer)?;
        Ok(Db {
            inner: Arc::new(Inner {
                path: uri,
                writer: Mutex::new(writer),
                readers: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Runs `f` inside a write transaction.
    pub fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.inner.writer.lock();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Runs `f` with a read-only connection from the pool.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = {
            let mut pool = self.inner.readers.lock();
            pool.pop()
        };
        let conn = match conn {
            Some(c) => c,
            None => {
                let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX;
                let c = Connection::open_with_flags(&self.inner.path, flags)?;
                configure(&c)?;
                c
            }
        };
        let out = f(&conn);
        let mut pool = self.inner.readers.lock();
        if pool.len() < MAX_READERS {
            pool.push(conn);
        }
        out
    }

    /// Runs a write on the blocking thread pool (for large batches).
    pub async fn write_async<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.write(f))
            .await
            .map_err(|e| Error::Other(format!("db task failed: {e}")))?
    }

    /// Runs a read on the blocking thread pool.
    pub async fn read_async<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.read(f))
            .await
            .map_err(|e| Error::Other(format!("db task failed: {e}")))?
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(Error::Other(format!(
            "database was created by a newer version of Envelope (schema {version})"
        )));
    }
    if version < 1 {
        conn.execute_batch(SCHEMA)?;
    } else {
        for (to, sql) in MIGRATIONS {
            if version < *to {
                conn.execute_batch(sql)?;
            }
        }
        for (to, table, column, declaration) in NEW_COLUMNS {
            let exists: bool = conn.query_row(
                "SELECT count(*) > 0 FROM pragma_table_info(?1) WHERE name = ?2",
                [table, column],
                |r| r.get(0),
            )?;
            if version < *to && !exists {
                conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {declaration};"
                ))?;
            }
        }
        // The sort keys of the mail already here, then their indexes and triggers.
        if version < 13 {
            let tx = conn.unchecked_transaction()?;
            mail::fill_sort_keys(&tx)?;
            tx.execute_batch(include_str!("sort.sql"))?;
            tx.commit()?;
        }
        if version < 14 {
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS messages_important ON messages(thread_id) WHERE importance > 0;",
            )?;
        }
    }
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    Ok(())
}

/// Compresses text for storage.
pub fn pack(text: &str) -> Vec<u8> {
    zstd::bulk::compress(text.as_bytes(), 3).unwrap_or_else(|_| text.as_bytes().to_vec())
}

/// Reverses [`pack`].
pub fn unpack(data: &[u8]) -> String {
    // zstd frames start with the magic number 0xFD2FB528 (little endian).
    if data.len() >= 4
        && data[..4] == [0x28, 0xB5, 0x2F, 0xFD]
        && let Ok(raw) = zstd::stream::decode_all(data)
    {
        return String::from_utf8_lossy(&raw).into_owned();
    }
    String::from_utf8_lossy(data).into_owned()
}

/// Current time in unix milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_migrates() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        let n: i64 = db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM accounts", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn adds_new_columns_to_older_databases() {
        let path = std::env::temp_dir().join(format!("envelope-v5-{}.db", uuid::Uuid::new_v4()));
        {
            // A version 5 database: threads without the muted column.
            let db = Db::open(&path).unwrap();
            db.write(|tx| {
                tx.execute_batch(
                    "ALTER TABLE threads DROP COLUMN muted; PRAGMA user_version = 5;",
                )?;
                Ok(())
            })
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let muted: i64 = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM pragma_table_info('threads') WHERE name = 'muted'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(muted, 1);
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn migrates_version_2_contacts() {
        let path = std::env::temp_dir().join(format!("envelope-v2-{}.db", uuid::Uuid::new_v4()));
        {
            // A version 2 database, with the draft contacts table.
            let db = Db::open(&path).unwrap();
            db.write(|tx| {
                tx.execute_batch(
                    "DROP TABLE contacts; DROP TABLE address_books;
                     CREATE TABLE contacts (id INTEGER PRIMARY KEY, account_id INTEGER, book TEXT NOT NULL DEFAULT '',
                       remote_id TEXT NOT NULL, etag TEXT, name TEXT NOT NULL DEFAULT '', emails TEXT NOT NULL DEFAULT '',
                       data TEXT NOT NULL, raw TEXT, UNIQUE (account_id, book, remote_id));
                     PRAGMA user_version = 2;",
                )?;
                Ok(())
            })
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let (version, books, has_book_id): (i64, i64, i64) = db
            .read(|c| {
                Ok((
                    c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                    c.query_row("SELECT count(*) FROM address_books", [], |r| r.get(0))?,
                    c.query_row(
                        "SELECT count(*) FROM pragma_table_info('contacts') WHERE name = 'book_id'",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!((version, books, has_book_id), (SCHEMA_VERSION, 0, 1));
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn migrates_version_6_to_tasks() {
        let path = std::env::temp_dir().join(format!("envelope-v6-{}.db", uuid::Uuid::new_v4()));
        {
            // A version 6 database: everything but tasks.
            let db = Db::open(&path).unwrap();
            db.write(|tx| {
                tx.execute_batch(
                    "DROP TABLE tasks; DROP TABLE task_lists; PRAGMA user_version = 6;",
                )?;
                Ok(())
            })
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let (version, lists, columns): (i64, i64, i64) = db
            .read(|c| {
                Ok((
                    c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                    c.query_row("SELECT count(*) FROM task_lists", [], |r| r.get(0))?,
                    c.query_row(
                        "SELECT count(*) FROM pragma_table_info('tasks')
                         WHERE name IN ('due_at', 'local_state', 'push_error', 'source', 'notified')",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!((version, lists, columns), (SCHEMA_VERSION, 0, 5));
        let list = db
            .write(|tx| tasks::create_local_list(tx, "Tasks", true))
            .unwrap();
        assert_eq!(db.read(tasks::local_default_list).unwrap(), Some(list));
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn adds_task_columns_to_version_7_databases() {
        let path = std::env::temp_dir().join(format!("envelope-v7-{}.db", uuid::Uuid::new_v4()));
        let added = ["deleted_state", "deleted_with", "sent_at"];
        {
            // A version 7 database: tasks without the columns added since.
            let db = Db::open(&path).unwrap();
            db.write(|tx| {
                for column in added {
                    tx.execute_batch(&format!("ALTER TABLE tasks DROP COLUMN {column};"))?;
                }
                tx.execute_batch("PRAGMA user_version = 7;")?;
                Ok(())
            })
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let (version, columns): (i64, i64) = db
            .read(|c| {
                let mut columns = 0;
                for column in added {
                    columns += c.query_row(
                        "SELECT count(*) FROM pragma_table_info('tasks') WHERE name = ?",
                        [column],
                        |r| r.get::<_, i64>(0),
                    )?;
                }
                Ok((
                    c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                    columns,
                ))
            })
            .unwrap();
        assert_eq!((version, columns), (SCHEMA_VERSION, added.len() as i64));
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn adds_draft_columns_to_version_8_databases() {
        let path = std::env::temp_dir().join(format!("envelope-v8-{}.db", uuid::Uuid::new_v4()));
        {
            // A version 8 database, with a draft saved before drafts knew their Message-ID.
            let db = Db::open(&path).unwrap();
            db.write(|tx| {
                tx.execute_batch(
                    "ALTER TABLE drafts DROP COLUMN message_id;
                     ALTER TABLE drafts DROP COLUMN synced_at;
                     INSERT INTO accounts (id, kind, provider, email, name, color, config, created_at)
                       VALUES (1, 'imap', 'generic', 'me@example.com', 'Me', '#000', '{}', 0);
                     INSERT INTO drafts (account_id, data, remote_uid, updated_at) VALUES (1, '{}', 7, 5);
                     PRAGMA user_version = 8;",
                )?;
                Ok(())
            })
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let (version, row) = db
            .read(|c| {
                let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                Ok((version, misc::draft_list(c)?))
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(row.len(), 1);
        // On the server already (a UID), so not listed as only on this computer.
        assert!(!row[0].local_only);
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn adds_sort_keys_to_version_12_databases() {
        use crate::db::mail;
        use crate::model::{AccountConfig, AccountKind, Address, FolderRole};

        let path = std::env::temp_dir().join(format!("envelope-v12-{}.db", uuid::Uuid::new_v4()));
        let folder = |remote: &str, role| mail::FolderUpsert {
            remote_id: remote.into(),
            name: remote.into(),
            path: remote.into(),
            parent_remote_id: None,
            role,
            kind: "folder".into(),
            delimiter: None,
            selectable: true,
            sort_order: 0,
            color: None,
        };
        let (message, work) = {
            // A version 12 database with a message, before lists could be sorted.
            let db = Db::open(&path).unwrap();
            db.write(|tx| {
                let account = mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    "me@example.com",
                    "Me",
                    "#000",
                    &AccountConfig::default(),
                )?;
                let folders = mail::sync_folders(
                    tx,
                    account,
                    &[
                        folder("INBOX", Some(FolderRole::Inbox)),
                        folder("Work", None),
                    ],
                )?;
                let meta = mail::MessageMeta {
                    message_id: Some("z@x".into()),
                    subject: "Hello".into(),
                    from: Some(Address::new(Some("Zoë Adams".into()), "zoe@example.com")),
                    date: 1_000,
                    ..Default::default()
                };
                let stored = mail::upsert_message(tx, account, folders["INBOX"], Some(1), &meta)?;
                mail::refresh_threads(tx, &[stored.thread_id].into())?;
                tx.execute_batch(
                    "DROP TRIGGER messages_sort;
                     DROP INDEX thread_folders_sender; DROP INDEX message_folders_date; DROP INDEX message_folders_sender;
                     ALTER TABLE threads DROP COLUMN sender; ALTER TABLE thread_folders DROP COLUMN sender;
                     ALTER TABLE message_folders DROP COLUMN date; ALTER TABLE message_folders DROP COLUMN sender;
                     PRAGMA user_version = 12;",
                )?;
                Ok((stored.message_id, folders["Work"]))
            })
            .unwrap()
        };
        let db = Db::open(&path).unwrap();
        let keys = |db: &Db| {
            db.read(|c| {
                let mut st = c.prepare(
                    "SELECT mf.date, mf.sender, tf.sender, t.sender FROM message_folders mf
                     JOIN messages m ON m.id = mf.message_id JOIN threads t ON t.id = m.thread_id
                     JOIN thread_folders tf ON tf.thread_id = t.id AND tf.folder_id = mf.folder_id
                     ORDER BY mf.folder_id",
                )?;
                let rows = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<rusqlite::Result<Vec<(i64, String, String, String)>>>()?;
                Ok(rows)
            })
            .unwrap()
        };
        let zoe = |date: i64| {
            (
                date,
                "zoe adams".to_string(),
                "zoe adams".to_string(),
                "zoe adams".to_string(),
            )
        };
        assert_eq!(keys(&db), [zoe(1_000)]);
        // A message put in another folder takes its date and sender along, and
        // with the trigger back they follow a change.
        db.write(|tx| {
            assert!(mail::add_location(tx, work, message)?);
            tx.execute("UPDATE messages SET date = 2000 WHERE id = ?", [message])?;
            let thread: i64 = tx.query_row(
                "SELECT thread_id FROM messages WHERE id = ?",
                [message],
                |r| r.get(0),
            )?;
            mail::refresh_threads(tx, &[thread].into())?;
            Ok(())
        })
        .unwrap();
        assert_eq!(keys(&db), [zoe(2_000), zoe(2_000)]);
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn muted_conversations_stay_muted_as_replies_arrive() {
        use crate::db::mail;
        use crate::model::{AccountConfig, AccountKind, Address, FolderRole};

        let db = Db::open(Path::new(":memory:")).unwrap();
        let (account, inbox) = db
            .write(|tx| {
                let account = mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    "me@example.com",
                    "Me",
                    "#000",
                    &AccountConfig::default(),
                )?;
                let folders = mail::sync_folders(
                    tx,
                    account,
                    &[mail::FolderUpsert {
                        remote_id: "INBOX".into(),
                        name: "Inbox".into(),
                        path: "INBOX".into(),
                        parent_remote_id: None,
                        role: Some(FolderRole::Inbox),
                        kind: "folder".into(),
                        delimiter: None,
                        selectable: true,
                        sort_order: 0,
                        color: None,
                    }],
                )?;
                Ok((account, folders["INBOX"]))
            })
            .unwrap();
        let message = |id: &str, date: i64, reply_to: Option<&str>| mail::MessageMeta {
            message_id: Some(id.into()),
            in_reply_to: reply_to.map(Into::into),
            references: reply_to.map(|r| vec![r.to_string()]).unwrap_or_default(),
            subject: "Lunch plans".into(),
            from: Some(Address::new(None, "team@example.com")),
            date,
            ..Default::default()
        };
        let thread = db
            .write(|tx| {
                let stored = mail::upsert_message(
                    tx,
                    account,
                    inbox,
                    Some(1),
                    &message("a@x", 1_000, None),
                )?;
                mail::refresh_threads(tx, &[stored.thread_id].into())?;
                mail::set_muted(tx, &[stored.thread_id], true)?;
                Ok(stored.thread_id)
            })
            .unwrap();
        assert!(db.read(|c| mail::thread_summary(c, thread)).unwrap().muted);

        // A reply joins the conversation and finds it muted.
        let reply = db
            .write(|tx| {
                let stored = mail::upsert_message(
                    tx,
                    account,
                    inbox,
                    Some(2),
                    &message("b@x", 2_000, Some("a@x")),
                )?;
                mail::refresh_threads(tx, &[stored.thread_id].into())?;
                Ok(stored.thread_id)
            })
            .unwrap();
        assert_eq!(reply, thread);
        assert_eq!(
            db.read(|c| mail::muted_among(c, &[reply, reply, 999]))
                .unwrap(),
            [thread]
        );

        db.write(|tx| mail::set_muted(tx, &[thread], false))
            .unwrap();
        assert!(
            db.read(|c| mail::muted_among(c, &[thread]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn follow_ups_wait_for_a_reply() {
        use crate::db::{mail, misc};
        use crate::model::{AccountConfig, AccountKind, Address, FolderRole};

        let db = Db::open(Path::new(":memory:")).unwrap();
        let (account, sent) = db
            .write(|tx| {
                let account = mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    "me@example.com",
                    "Me",
                    "#000",
                    &AccountConfig::default(),
                )?;
                let folders = mail::sync_folders(
                    tx,
                    account,
                    &[mail::FolderUpsert {
                        remote_id: "Sent".into(),
                        name: "Sent".into(),
                        path: "Sent".into(),
                        parent_remote_id: None,
                        role: Some(FolderRole::Sent),
                        kind: "folder".into(),
                        delimiter: None,
                        selectable: true,
                        sort_order: 0,
                        color: None,
                    }],
                )?;
                Ok((account, folders["Sent"]))
            })
            .unwrap();
        let message = |id: &str, from: &str, date: i64, reply_to: Option<&str>| mail::MessageMeta {
            message_id: Some(id.into()),
            in_reply_to: reply_to.map(Into::into),
            references: reply_to.map(|r| vec![r.to_string()]).unwrap_or_default(),
            subject: "Proposal".into(),
            from: Some(Address::new(None, from)),
            date,
            ..Default::default()
        };
        let thread = db
            .write(|tx| {
                misc::followup_add(
                    tx,
                    "<sent-1@example.com>",
                    account,
                    "Proposal",
                    1_000,
                    2_000,
                )?;
                let stored = mail::upsert_message(
                    tx,
                    account,
                    sent,
                    Some(1),
                    &message("sent-1@example.com", "me@example.com", 1_000, None),
                )?;
                mail::refresh_threads(tx, &[stored.thread_id].into())?;
                Ok(stored.thread_id)
            })
            .unwrap();
        // Not due yet.
        assert!(
            db.read(|c| misc::followups_to_check(c, 1_500))
                .unwrap()
                .is_empty()
        );
        // Due without a reply (the user's own follow-up doesn't count).
        db.write(|tx| {
            mail::upsert_message(
                tx,
                account,
                sent,
                Some(2),
                &message(
                    "own@example.com",
                    "ME@example.com",
                    1_500,
                    Some("sent-1@example.com"),
                ),
            )?;
            Ok(())
        })
        .unwrap();
        let due = db.read(|c| misc::followups_to_check(c, 2_500)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(
            (due[0].thread_id, due[0].replied, due[0].notified),
            (Some(thread), false, false)
        );
        db.write(|tx| misc::followup_mark_notified(tx, &due[0].message_id))
            .unwrap();
        let listed = db
            .read(|c| {
                let sql = mail::list_sql(
                    c,
                    &crate::model::Mailbox::FollowUp,
                    &Default::default(),
                    None,
                    crate::model::SortOrder::Newest,
                    false,
                )?;
                mail::query_threads(c, &sql, 0, 10)
            })
            .unwrap();
        assert_eq!(
            listed.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![thread]
        );
        // A reply from someone else ends the reminder.
        db.write(|tx| {
            mail::upsert_message(
                tx,
                account,
                sent,
                Some(3),
                &message(
                    "reply@other.example",
                    "them@other.example",
                    3_000,
                    Some("sent-1@example.com"),
                ),
            )?;
            Ok(())
        })
        .unwrap();
        let after = db.read(|c| misc::followups_to_check(c, 3_500)).unwrap();
        assert!(after[0].replied);
    }

    #[test]
    fn pack_round_trip() {
        let s = "hello ".repeat(100);
        let p = pack(&s);
        assert!(p.len() < s.len());
        assert_eq!(unpack(&p), s);
        assert_eq!(unpack(b"plain"), "plain");
    }
}

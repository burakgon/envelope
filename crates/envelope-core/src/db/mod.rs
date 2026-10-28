//! SQLite storage: one writer connection plus a small pool of readers (WAL mode
//! lets them run concurrently), migrations, and typed query modules.

pub mod calendar;
pub mod mail;
pub mod misc;

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, Transaction};

use crate::error::{Error, Result};

const SCHEMA: &str = include_str!("schema.sql");
const SCHEMA_VERSION: i64 = 2;

/// Steps from one schema version to the next, applied in order to older databases.
const MIGRATIONS: &[(i64, &str)] = &[(
    2,
    "CREATE INDEX IF NOT EXISTS messages_reply ON messages(account_id, in_reply_to) WHERE in_reply_to IS NOT NULL;
     CREATE INDEX IF NOT EXISTS thread_folders_unread ON thread_folders(folder_id, last_date DESC) WHERE unread > 0;",
)];
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
    fn pack_round_trip() {
        let s = "hello ".repeat(100);
        let p = pack(&s);
        assert!(p.len() < s.len());
        assert_eq!(unpack(&p), s);
        assert_eq!(unpack(b"plain"), "plain");
    }
}

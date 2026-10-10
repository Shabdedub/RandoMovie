//! The SQLite connection and schema.
//!
//! The connection lives behind a mutex so the store can be shared by the worker's tasks.
//! Every call holds the lock only for one short statement batch and never across an await.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS http_cache (
    key        TEXT PRIMARY KEY NOT NULL,
    body       BLOB NOT NULL,
    fetched_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS image_cache (
    key        TEXT PRIMARY KEY NOT NULL,
    body       BLOB NOT NULL,
    bytes      INTEGER NOT NULL,
    fetched_at INTEGER NOT NULL,
    last_used  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS image_cache_last_used ON image_cache (last_used);
CREATE TABLE IF NOT EXISTS pick_history (
    seq       INTEGER PRIMARY KEY AUTOINCREMENT,
    media     TEXT NOT NULL,
    tmdb_id   INTEGER NOT NULL,
    picked_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS exclusions (
    media      TEXT NOT NULL,
    tmdb_id    INTEGER NOT NULL,
    reason     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (media, tmdb_id)
);
";

pub struct Store {
    inner: Mutex<Inner>,
}

pub(crate) struct Inner {
    pub conn: Connection,
    /// Running total of image bytes, computed on first use.
    pub image_bytes: Option<u64>,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Store::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> rusqlite::Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Store> {
        conn.busy_timeout(Duration::from_secs(2))?;
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Negative means KiB: a 1 MB page cache keeps memory small.
        conn.pragma_update(None, "cache_size", -1024)?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Store {
            inner: Mutex::new(Inner {
                conn,
                image_bytes: None,
            }),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

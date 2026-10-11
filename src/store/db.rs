//! The SQLite connection and schema.
//!
//! The connection lives behind a mutex so the store can be shared by the worker's tasks.
//! Every call holds the lock only for one short statement batch and never across an await.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

const SCHEMA_VERSION: i32 = 2;

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
    media       TEXT NOT NULL,
    tmdb_id     INTEGER NOT NULL,
    reason      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    name        TEXT,
    year        INTEGER,
    poster_path TEXT,
    imdb_id     TEXT,
    PRIMARY KEY (media, tmdb_id)
);
";

/// Columns version 2 added to `exclusions`: what the Library shows for a marked title, so it
/// can list them without the network.
const TITLE_COLUMNS: [(&str, &str); 4] = [
    ("name", "TEXT"),
    ("year", "INTEGER"),
    ("poster_path", "TEXT"),
    ("imdb_id", "TEXT"),
];

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
        add_missing_columns(&conn)?;
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

/// Brings a version 1 database up to date. Columns are added only when missing rather than by
/// `user_version`, because an older build that opens the database sets it back to 1.
fn add_missing_columns(conn: &Connection) -> rusqlite::Result<()> {
    let mut existing = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(exclusions)")?;
        let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for name in names {
            existing.push(name?);
        }
    }
    for (column, kind) in TITLE_COLUMNS {
        if !existing.iter().any(|name| name == column) {
            conn.execute_batch(&format!("ALTER TABLE exclusions ADD COLUMN {column} {kind}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaType, TitleRef};
    use crate::store::{ExclusionReason, TitleSummary};

    #[test]
    fn upgrades_a_version_1_database() {
        let dir = std::env::temp_dir().join(format!("randomovie-db-{}", fastrand::u64(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE exclusions (
                    media TEXT NOT NULL, tmdb_id INTEGER NOT NULL, reason TEXT NOT NULL,
                    created_at INTEGER NOT NULL, PRIMARY KEY (media, tmdb_id));
                 INSERT INTO exclusions VALUES ('movie', 603, 'seen', 7);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let matrix = TitleRef::new(MediaType::Movie, 603);
        {
            let store = Store::open(&path).unwrap();
            let marked = store.marked_titles().unwrap();
            assert_eq!(marked.len(), 1);
            assert_eq!((marked[0].title, marked[0].reason, marked[0].marked_at), (matrix, ExclusionReason::Seen, 7));
            assert_eq!(marked[0].summary, None, "marked before the Library existed");
            let summary = TitleSummary {
                name: "The Matrix".into(),
                year: Some(1999),
                poster_path: None,
                imdb_id: Some("tt0133093".into()),
            };
            store.set_title_summary(matrix, &summary).unwrap();
        }
        // An older build resets the version; opening again must still work and keep the data.
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 1).unwrap();
        let store = Store::open(&path).unwrap();
        let marked = store.marked_titles().unwrap();
        assert_eq!(marked[0].summary.as_ref().map(|s| s.name.as_str()), Some("The Matrix"));
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

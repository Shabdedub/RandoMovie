//! Cached HTTP responses and images.
//!
//! API responses are stored raw with their fetch time; the caller decides freshness per
//! endpoint. Images expire after 30 days and are evicted least-recently-used first once the
//! table passes its size cap.

use std::time::Duration;

use rusqlite::{OptionalExtension, params};

use super::db::{Inner, Store};

/// A cached response body and when it was fetched (unix seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedBody {
    pub body: Vec<u8>,
    pub fetched_at: i64,
}

impl CachedBody {
    pub fn age(&self, now: i64) -> i64 {
        now - self.fetched_at
    }

    /// Fresh while younger than `ttl`. An entry from the future (the clock moved back)
    /// counts as stale, which errs towards refetching.
    pub fn is_fresh(&self, now: i64, ttl: Duration) -> bool {
        let age = self.age(now);
        age >= 0 && age < ttl.as_secs() as i64
    }
}

impl Store {
    pub fn http_get(&self, key: &str) -> rusqlite::Result<Option<CachedBody>> {
        self.lock()
            .conn
            .query_row(
                "SELECT body, fetched_at FROM http_cache WHERE key = ?1",
                [key],
                |row| {
                    Ok(CachedBody {
                        body: row.get(0)?,
                        fetched_at: row.get(1)?,
                    })
                },
            )
            .optional()
    }

    pub fn http_put(&self, key: &str, body: &[u8], fetched_at: i64) -> rusqlite::Result<()> {
        self.lock().conn.execute(
            "INSERT INTO http_cache (key, body, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET body = excluded.body, fetched_at = excluded.fetched_at",
            params![key, body, fetched_at],
        )?;
        Ok(())
    }

    /// Deletes responses older than `max_age_secs`. Returns how many were removed.
    pub fn http_purge(&self, now: i64, max_age_secs: i64) -> rusqlite::Result<usize> {
        self.lock().conn.execute(
            "DELETE FROM http_cache WHERE fetched_at < ?1",
            [now - max_age_secs],
        )
    }

    /// Returns a cached image younger than `max_age_secs` and marks it as recently used.
    pub fn image_get(
        &self,
        key: &str,
        now: i64,
        max_age_secs: i64,
    ) -> rusqlite::Result<Option<Vec<u8>>> {
        let inner = self.lock();
        let row: Option<(Vec<u8>, i64)> = inner
            .conn
            .query_row(
                "SELECT body, fetched_at FROM image_cache WHERE key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            Some((body, fetched_at)) if (0..max_age_secs).contains(&(now - fetched_at)) => {
                inner.conn.execute(
                    "UPDATE image_cache SET last_used = ?1 WHERE key = ?2",
                    params![now, key],
                )?;
                Ok(Some(body))
            }
            _ => Ok(None),
        }
    }

    /// Stores an image, then evicts the least recently used images if the total passes
    /// `cap_bytes` (down to 90 % of the cap, so eviction doesn't run on every insert).
    pub fn image_put(
        &self,
        key: &str,
        body: &[u8],
        now: i64,
        cap_bytes: u64,
    ) -> rusqlite::Result<()> {
        let mut inner = self.lock();
        let old: Option<i64> = inner
            .conn
            .query_row("SELECT bytes FROM image_cache WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        inner.conn.execute(
            "INSERT INTO image_cache (key, body, bytes, fetched_at, last_used)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(key) DO UPDATE SET body = excluded.body, bytes = excluded.bytes,
                 fetched_at = excluded.fetched_at, last_used = excluded.last_used",
            params![key, body, body.len() as i64, now],
        )?;
        let total = match inner.image_bytes {
            Some(total) => (total + body.len() as u64).saturating_sub(old.unwrap_or(0) as u64),
            None => image_total(&inner)?,
        };
        inner.image_bytes = Some(total);
        if total > cap_bytes {
            evict_lru(&mut inner, cap_bytes / 10 * 9)?;
        }
        Ok(())
    }

    /// Removes expired images, then trims to the cap. Run at startup.
    pub fn image_maintain(&self, now: i64, max_age_secs: i64, cap_bytes: u64) -> rusqlite::Result<()> {
        let mut inner = self.lock();
        inner.conn.execute(
            "DELETE FROM image_cache WHERE fetched_at < ?1",
            [now - max_age_secs],
        )?;
        inner.image_bytes = Some(image_total(&inner)?);
        if inner.image_bytes.unwrap_or(0) > cap_bytes {
            evict_lru(&mut inner, cap_bytes / 10 * 9)?;
        }
        Ok(())
    }

    pub fn image_cache_bytes(&self) -> rusqlite::Result<u64> {
        let mut inner = self.lock();
        let total = image_total(&inner)?;
        inner.image_bytes = Some(total);
        Ok(total)
    }

    /// Size of everything "Clear cache" would remove: API responses plus images.
    pub fn cache_bytes(&self) -> rusqlite::Result<u64> {
        let http: i64 = self.lock().conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(body)), 0) FROM http_cache",
            [],
            |r| r.get(0),
        )?;
        Ok(http.max(0) as u64 + self.image_cache_bytes()?)
    }

    /// Deletes cached responses and images and returns the space to the OS.
    /// History and exclusions are kept.
    pub fn clear_cache(&self) -> rusqlite::Result<()> {
        let mut inner = self.lock();
        inner
            .conn
            .execute_batch("DELETE FROM http_cache; DELETE FROM image_cache; VACUUM;")?;
        inner.image_bytes = Some(0);
        Ok(())
    }
}

fn image_total(inner: &Inner) -> rusqlite::Result<u64> {
    let total: i64 = inner.conn.query_row(
        "SELECT COALESCE(SUM(bytes), 0) FROM image_cache",
        [],
        |r| r.get(0),
    )?;
    Ok(total.max(0) as u64)
}

/// Deletes least recently used images until the total is at most `target` bytes.
fn evict_lru(inner: &mut Inner, target: u64) -> rusqlite::Result<()> {
    let mut total = image_total(inner)?;
    if total <= target {
        inner.image_bytes = Some(total);
        return Ok(());
    }
    let tx = inner.conn.transaction()?;
    let doomed: Vec<(String, i64)> = {
        let mut stmt = tx.prepare("SELECT key, bytes FROM image_cache ORDER BY last_used ASC")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut doomed = Vec::new();
        let mut remaining = total;
        for row in rows {
            if remaining <= target {
                break;
            }
            let (key, bytes): (String, i64) = row?;
            remaining = remaining.saturating_sub(bytes.max(0) as u64);
            doomed.push((key, bytes));
        }
        doomed
    };
    for (key, bytes) in &doomed {
        tx.execute("DELETE FROM image_cache WHERE key = ?1", [key])?;
        total = total.saturating_sub((*bytes).max(0) as u64);
    }
    tx.commit()?;
    inner.image_bytes = Some(total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::time::{DAY, HOUR};

    #[test]
    fn freshness_follows_ttl() {
        let now = 1_000_000;
        let entry = |age: i64| CachedBody {
            body: vec![],
            fetched_at: now - age,
        };
        let day = Duration::from_secs(DAY as u64);
        let week = Duration::from_secs(7 * DAY as u64);
        assert!(entry(0).is_fresh(now, day));
        assert!(entry(23 * HOUR).is_fresh(now, day));
        assert!(!entry(24 * HOUR).is_fresh(now, day), "exactly 24 h is expired");
        assert!(!entry(25 * HOUR).is_fresh(now, day));
        assert!(entry(25 * HOUR).is_fresh(now, week));
        assert!(!entry(8 * DAY).is_fresh(now, week));
        assert!(!entry(-60).is_fresh(now, day), "entries from the future are stale");
    }

    #[test]
    fn http_cache_round_trip_and_purge() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.http_get("/movie/1").unwrap(), None);
        store.http_put("/movie/1", b"{\"id\":1}", 100).unwrap();
        store.http_put("/movie/2", b"{\"id\":2}", 900).unwrap();
        let hit = store.http_get("/movie/1").unwrap().unwrap();
        assert_eq!(hit.body, b"{\"id\":1}");
        assert_eq!(hit.fetched_at, 100);

        // Replacing updates both body and time.
        store.http_put("/movie/1", b"{\"id\":1,\"v\":2}", 500).unwrap();
        assert_eq!(store.http_get("/movie/1").unwrap().unwrap().fetched_at, 500);

        assert_eq!(store.http_purge(1000, 200).unwrap(), 1);
        assert!(store.http_get("/movie/1").unwrap().is_none());
        assert!(store.http_get("/movie/2").unwrap().is_some());
    }

    #[test]
    fn images_expire_and_evict_least_recently_used() {
        let store = Store::open_in_memory().unwrap();
        let kb = vec![7u8; 1000];
        store.image_put("a", &kb, 10, 10_000).unwrap();
        store.image_put("b", &kb, 20, 10_000).unwrap();
        store.image_put("c", &kb, 30, 10_000).unwrap();
        // Touch "a" so "b" becomes the least recently used.
        assert!(store.image_get("a", 40, 1000).unwrap().is_some());
        // A cap of 2500 bytes forces eviction down to 2250: two images must go.
        store.image_put("d", &kb, 50, 2500).unwrap();
        assert!(store.image_get("b", 60, 1000).unwrap().is_none());
        assert!(store.image_get("c", 60, 1000).unwrap().is_none());
        assert!(store.image_get("d", 60, 1000).unwrap().is_some());
        assert_eq!(store.image_cache_bytes().unwrap(), 2000);

        // Expiry: "d" was fetched at 50, so a max age of 30 s at time 90 rejects it.
        assert!(store.image_get("d", 90, 30).unwrap().is_none());
        store.image_maintain(90, 30, 10_000).unwrap();
        assert_eq!(store.image_cache_bytes().unwrap(), 0);
    }

    #[test]
    fn clear_cache_keeps_history() {
        let store = Store::open_in_memory().unwrap();
        store.http_put("k", b"body", 1).unwrap();
        store.image_put("i", b"img", 1, 1000).unwrap();
        store
            .record_pick(
                crate::media::TitleRef::new(crate::media::MediaType::Movie, 603),
                5,
            )
            .unwrap();
        assert!(store.cache_bytes().unwrap() > 0);
        store.clear_cache().unwrap();
        assert_eq!(store.cache_bytes().unwrap(), 0);
        assert_eq!(store.recent_picks(10).unwrap().len(), 1);
    }
}

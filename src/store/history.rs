//! Pick history and the "Seen it" / "Not interested" exclusions.

use std::collections::HashSet;

use rusqlite::params;

use super::db::Store;
use crate::media::{MediaType, TitleRef};

/// History rows kept on disk; the picker only looks at the newest 50.
const HISTORY_KEEP: i64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionReason {
    Seen,
    NotInterested,
}

impl ExclusionReason {
    fn as_str(self) -> &'static str {
        match self {
            ExclusionReason::Seen => "seen",
            ExclusionReason::NotInterested => "not_interested",
        }
    }
}

impl Store {
    pub fn record_pick(&self, title: TitleRef, now: i64) -> rusqlite::Result<()> {
        let inner = self.lock();
        inner.conn.execute(
            "INSERT INTO pick_history (media, tmdb_id, picked_at) VALUES (?1, ?2, ?3)",
            params![title.media.as_str(), title.id, now],
        )?;
        inner.conn.execute(
            "DELETE FROM pick_history WHERE seq <= (SELECT MAX(seq) FROM pick_history) - ?1",
            [HISTORY_KEEP],
        )?;
        Ok(())
    }

    /// The newest picks first.
    pub fn recent_picks(&self, limit: usize) -> rusqlite::Result<Vec<TitleRef>> {
        let inner = self.lock();
        let mut stmt = inner
            .conn
            .prepare("SELECT media, tmdb_id FROM pick_history ORDER BY seq DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
        })?;
        let mut picks = Vec::new();
        for row in rows {
            let (media, id) = row?;
            if let Some(media) = MediaType::parse(&media) {
                picks.push(TitleRef::new(media, id));
            }
        }
        Ok(picks)
    }

    pub fn exclude(&self, title: TitleRef, reason: ExclusionReason, now: i64) -> rusqlite::Result<()> {
        self.lock().conn.execute(
            "INSERT INTO exclusions (media, tmdb_id, reason, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(media, tmdb_id) DO UPDATE SET reason = excluded.reason,
                 created_at = excluded.created_at",
            params![title.media.as_str(), title.id, reason.as_str(), now],
        )?;
        Ok(())
    }

    pub fn unexclude(&self, title: TitleRef) -> rusqlite::Result<()> {
        self.lock().conn.execute(
            "DELETE FROM exclusions WHERE media = ?1 AND tmdb_id = ?2",
            params![title.media.as_str(), title.id],
        )?;
        Ok(())
    }

    pub fn excluded_titles(&self) -> rusqlite::Result<HashSet<TitleRef>> {
        let inner = self.lock();
        let mut stmt = inner.conn.prepare("SELECT media, tmdb_id FROM exclusions")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
        let mut set = HashSet::new();
        for row in rows {
            let (media, id) = row?;
            if let Some(media) = MediaType::parse(&media) {
                set.insert(TitleRef::new(media, id));
            }
        }
        Ok(set)
    }

    pub fn exclusion_reason(&self, title: TitleRef) -> rusqlite::Result<Option<ExclusionReason>> {
        use rusqlite::OptionalExtension;
        let reason: Option<String> = self
            .lock()
            .conn
            .query_row(
                "SELECT reason FROM exclusions WHERE media = ?1 AND tmdb_id = ?2",
                params![title.media.as_str(), title.id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(reason.map(|r| {
            if r == "seen" {
                ExclusionReason::Seen
            } else {
                ExclusionReason::NotInterested
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_newest_first_and_trimmed() {
        let store = Store::open_in_memory().unwrap();
        for id in 0..(HISTORY_KEEP as u32 + 20) {
            store
                .record_pick(TitleRef::new(MediaType::Movie, id), i64::from(id))
                .unwrap();
        }
        let recent = store.recent_picks(3).unwrap();
        let last = HISTORY_KEEP as u32 + 19;
        assert_eq!(
            recent,
            vec![
                TitleRef::new(MediaType::Movie, last),
                TitleRef::new(MediaType::Movie, last - 1),
                TitleRef::new(MediaType::Movie, last - 2),
            ]
        );
        assert_eq!(store.recent_picks(10_000).unwrap().len(), HISTORY_KEEP as usize);
    }

    #[test]
    fn exclusions_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let movie = TitleRef::new(MediaType::Movie, 603);
        let show = TitleRef::new(MediaType::Tv, 603);
        store.exclude(movie, ExclusionReason::Seen, 1).unwrap();
        store.exclude(show, ExclusionReason::NotInterested, 2).unwrap();
        let set = store.excluded_titles().unwrap();
        assert!(set.contains(&movie) && set.contains(&show));
        assert_eq!(store.exclusion_reason(movie).unwrap(), Some(ExclusionReason::Seen));
        store.unexclude(movie).unwrap();
        assert_eq!(store.exclusion_reason(movie).unwrap(), None);
        assert_eq!(store.excluded_titles().unwrap().len(), 1);
    }
}

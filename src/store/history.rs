//! Pick history and the "Seen it" / "Not interested" exclusions.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};

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

    fn parse(s: &str) -> ExclusionReason {
        if s == "seen" {
            ExclusionReason::Seen
        } else {
            ExclusionReason::NotInterested
        }
    }
}

/// What the Library shows for a marked title. It's saved when the title is marked, so the
/// Library needs no network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleSummary {
    pub name: String,
    pub year: Option<u16>,
    pub poster_path: Option<String>,
    pub imdb_id: Option<String>,
}

/// A title marked "Seen it" or "Not interested".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedTitle {
    pub title: TitleRef,
    pub reason: ExclusionReason,
    /// When it was marked, in unix seconds.
    pub marked_at: i64,
    /// `None` for a title marked before the Library existed, until its details are saved.
    pub summary: Option<TitleSummary>,
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
        let reason: Option<String> = self
            .lock()
            .conn
            .query_row(
                "SELECT reason FROM exclusions WHERE media = ?1 AND tmdb_id = ?2",
                params![title.media.as_str(), title.id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(reason.as_deref().map(ExclusionReason::parse))
    }

    /// Every marked title, the most recently marked first.
    pub fn marked_titles(&self) -> rusqlite::Result<Vec<MarkedTitle>> {
        let inner = self.lock();
        let mut stmt = inner.conn.prepare(
            "SELECT media, tmdb_id, reason, created_at, name, year, poster_path, imdb_id
             FROM exclusions ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let media: String = r.get(0)?;
            let reason: String = r.get(2)?;
            let name: Option<String> = r.get(4)?;
            let summary = match name {
                Some(name) => Some(TitleSummary {
                    name,
                    year: r.get(5)?,
                    poster_path: r.get(6)?,
                    imdb_id: r.get(7)?,
                }),
                None => None,
            };
            let Some(media) = MediaType::parse(&media) else {
                return Ok(None);
            };
            Ok(Some(MarkedTitle {
                title: TitleRef::new(media, r.get(1)?),
                reason: ExclusionReason::parse(&reason),
                marked_at: r.get(3)?,
                summary,
            }))
        })?;
        let mut marked = Vec::new();
        for row in rows {
            if let Some(title) = row? {
                marked.push(title);
            }
        }
        Ok(marked)
    }

    /// Saves what the Library shows for a marked title. A title that isn't marked is left out.
    pub fn set_title_summary(&self, title: TitleRef, summary: &TitleSummary) -> rusqlite::Result<()> {
        self.lock().conn.execute(
            "UPDATE exclusions SET name = ?3, year = ?4, poster_path = ?5, imdb_id = ?6
             WHERE media = ?1 AND tmdb_id = ?2",
            params![
                title.media.as_str(),
                title.id,
                summary.name,
                summary.year,
                summary.poster_path,
                summary.imdb_id
            ],
        )?;
        Ok(())
    }

    /// The title is marked but its summary isn't saved yet.
    pub fn needs_title_summary(&self, title: TitleRef) -> rusqlite::Result<bool> {
        let missing: Option<bool> = self
            .lock()
            .conn
            .query_row(
                "SELECT name IS NULL FROM exclusions WHERE media = ?1 AND tmdb_id = ?2",
                params![title.media.as_str(), title.id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(missing.unwrap_or(false))
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

    #[test]
    fn marked_titles_are_newest_first_with_their_summaries() {
        let store = Store::open_in_memory().unwrap();
        let matrix = TitleRef::new(MediaType::Movie, 603);
        let bluey = TitleRef::new(MediaType::Tv, 82_728);
        store.exclude(matrix, ExclusionReason::Seen, 10).unwrap();
        store.exclude(bluey, ExclusionReason::NotInterested, 20).unwrap();
        assert!(store.needs_title_summary(matrix).unwrap());
        let summary = TitleSummary {
            name: "The Matrix".into(),
            year: Some(1999),
            poster_path: Some("/f89U3ADr1oiB1s9GkdPOEpXUk5H.jpg".into()),
            imdb_id: Some("tt0133093".into()),
        };
        store.set_title_summary(matrix, &summary).unwrap();
        assert!(!store.needs_title_summary(matrix).unwrap());

        let marked = store.marked_titles().unwrap();
        assert_eq!(marked.iter().map(|m| m.title).collect::<Vec<_>>(), [bluey, matrix]);
        assert_eq!((marked[0].reason, marked[0].summary.as_ref()), (ExclusionReason::NotInterested, None));
        assert_eq!((marked[1].reason, marked[1].marked_at), (ExclusionReason::Seen, 10));
        assert_eq!(marked[1].summary.as_ref(), Some(&summary));

        // Moving a title to the other list keeps its summary and dates it anew.
        store.exclude(matrix, ExclusionReason::NotInterested, 30).unwrap();
        let moved = &store.marked_titles().unwrap()[0];
        assert_eq!((moved.title, moved.reason, moved.marked_at), (matrix, ExclusionReason::NotInterested, 30));
        assert_eq!(moved.summary.as_ref(), Some(&summary));

        // A title that isn't marked has nothing to fill in, and saving a summary skips it.
        let other = TitleRef::new(MediaType::Movie, 1);
        assert!(!store.needs_title_summary(other).unwrap());
        store.set_title_summary(other, &summary).unwrap();
        assert_eq!(store.marked_titles().unwrap().len(), 2);
    }
}

//! The Library on the worker: marked titles from the database, the name, year and poster
//! saved for each one, and Undo of a removal.

use std::rc::Rc;

use randomovie::imdb::ImdbStatus;
use randomovie::media::TitleRef;
use randomovie::store::{MarkedTitle, TitleSummary};
use randomovie::tmdb::model::TitleDetails;
use randomovie::tmdb::{Freshness, TmdbClient};
use randomovie::{log_error, log_info, log_warn};

use super::{LibraryItem, Update, Worker};

/// Titles whose details load at the same time while the Library fills in old entries. The
/// client allows six requests, so two stay free for thumbnails.
const SUMMARY_BATCH: usize = 4;

fn summary_of(details: &TitleDetails) -> TitleSummary {
    TitleSummary {
        name: details.title.clone(),
        year: details.year,
        poster_path: details.poster_path.clone(),
        imdb_id: details.imdb_id.clone(),
        genres: Some(details.genres.clone()),
    }
}

/// Titles marked before the Library existed have no summary; those saved by version 0.6.0
/// have one without genres.
fn needs_details(marked: &MarkedTitle) -> bool {
    marked.summary.as_ref().is_none_or(|summary| summary.genres.is_none())
}

impl Worker {
    /// Every marked title at once from the database. Titles saved without their details or
    /// genres then get them, once.
    pub(super) async fn load_library(self: &Rc<Self>, id: u64) {
        let marked = self.store.marked_titles().unwrap_or_else(|e| {
            log_error!("the Library couldn't be read: {e}");
            Vec::new()
        });
        let items = marked.iter().map(|m| self.library_item(m, false)).collect();
        self.emit(Update::Library { id, items });
        let missing: Vec<MarkedTitle> = marked.into_iter().filter(needs_details).collect();
        if missing.is_empty() {
            return;
        }
        let client = self.client();
        let region = self.effective_region();
        for batch in missing.chunks(SUMMARY_BATCH) {
            let one = |i: usize| {
                let marked = batch.get(i).cloned();
                let (client, region) = (client.as_deref(), &region);
                async move {
                    if let Some(marked) = marked {
                        self.fill_summary(id, marked, client, region).await;
                    }
                }
            };
            const { assert!(SUMMARY_BATCH == 4, "four futures are joined below") };
            tokio::join!(one(0), one(1), one(2), one(3));
        }
    }

    fn library_item(&self, marked: &MarkedTitle, unavailable: bool) -> LibraryItem {
        let imdb = match &marked.summary {
            Some(summary) => self.imdb.lookup(summary.imdb_id.as_deref()),
            None => ImdbStatus::Unavailable,
        };
        LibraryItem {
            title: marked.title,
            reason: marked.reason,
            marked_at: marked.marked_at,
            summary: marked.summary.clone(),
            imdb,
            unavailable,
        }
    }

    async fn fill_summary(&self, id: u64, mut marked: MarkedTitle, client: Option<&TmdbClient>, region: &str) {
        let fetched = match client {
            Some(client) => self.remember_summary(client, region, marked.title).await,
            None => None,
        };
        // A title that already had its name keeps it when its genres can't be loaded.
        if fetched.is_some() {
            marked.summary = fetched;
        }
        let unavailable = marked.summary.is_none();
        self.emit(Update::LibraryItem {
            id,
            item: self.library_item(&marked, unavailable),
        });
    }

    /// Saves what the Library shows for a marked title, from its details. Those are cached
    /// when the title was just on screen, so marking a title needs no extra request.
    pub(super) async fn remember_summary(
        &self,
        client: &TmdbClient,
        region: &str,
        title: TitleRef,
    ) -> Option<TitleSummary> {
        match client.details(title, region, Freshness::AllowStale).await {
            Ok(details) => {
                let summary = summary_of(&details);
                if let Err(e) = self.store.set_title_summary(title, &summary) {
                    log_warn!("the Library couldn't save a title's details: {e}");
                }
                Some(summary)
            }
            Err(e) => {
                log_info!("details for {}/{} unavailable for the Library: {e}", title.media.as_str(), title.id);
                None
            }
        }
    }

    /// Undo of a removal or a move in the Library: marked again with its first date and
    /// details.
    pub(super) fn restore(&self, item: LibraryItem) {
        let saved = self
            .store
            .exclude(item.title, item.reason, item.marked_at)
            .and_then(|()| match &item.summary {
                Some(summary) => self.store.set_title_summary(item.title, summary),
                None => Ok(()),
            });
        let reason = match saved {
            Ok(()) => Some(item.reason),
            Err(e) => {
                log_error!("the title couldn't be restored to the Library: {e}");
                self.excluded(item.title)
            }
        };
        self.emit(Update::Excluded {
            title: item.title,
            reason,
        });
    }
}

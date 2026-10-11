//! Search on the worker: result pages, each row's rating and availability, result
//! thumbnails, and the title card for an opened result.

use std::rc::Rc;
use std::time::Instant;

use randomovie::error::ApiError;
use randomovie::media::TitleRef;
use randomovie::search;
use randomovie::tmdb::images::{ImageKind, size_for};
use randomovie::tmdb::{Freshness, TmdbClient};
use randomovie::{log_info, log_warn};

use super::images;
use super::{ImageSpec, Update, Worker};

/// Rows whose details load at the same time. The client allows six requests, so two stay
/// free for thumbnails and a prepared pick.
const ROW_INFO_BATCH: usize = 4;

impl Worker {
    /// One page of results. Rows follow at once; their rating and availability load next.
    pub(super) async fn search(self: &Rc<Self>, id: u64, query: String, page: u32) {
        self.search_latest.set(id);
        let Some(client) = self.client() else {
            self.emit(Update::SearchFailed {
                id,
                page,
                error: ApiError::Unauthorized,
            });
            return;
        };
        let started = Instant::now();
        let result = client.search(&query, page).await;
        if self.search_latest.get() != id {
            return; // a newer search started
        }
        match result {
            Ok(found) => {
                let hits = {
                    let mut seen = self.search_seen.borrow_mut();
                    if page == 1 {
                        seen.clear();
                    }
                    search::hits(&found, &mut seen)
                };
                log_info!(
                    "search page {page}: {} titles in {} ms",
                    hits.len(),
                    started.elapsed().as_millis()
                );
                let titles: Vec<TitleRef> = hits.iter().map(|hit| hit.title).collect();
                self.emit(Update::SearchResults {
                    id,
                    page,
                    total_pages: found.total_pages,
                    hits,
                });
                self.load_row_info(id, titles).await;
            }
            Err(error) => {
                log_warn!("search failed: {error}");
                self.emit(Update::SearchFailed { id, page, error });
            }
        }
    }

    /// Rating and availability for each row, in order, a few at a time. It stops as soon as
    /// a newer search starts, so typing never leaves a queue of stale requests.
    pub(super) async fn load_row_info(self: &Rc<Self>, id: u64, titles: Vec<TitleRef>) {
        let Some(client) = self.client() else { return };
        let region = self.effective_region();
        self.ensure_catalog(&region).await;
        for batch in titles.chunks(ROW_INFO_BATCH) {
            if self.search_latest.get() != id {
                return;
            }
            let one = |i: usize| {
                let title = batch.get(i).copied();
                let (client, region) = (&client, &region);
                async move {
                    if let Some(title) = title {
                        self.emit_row_info(id, title, client, region).await;
                    }
                }
            };
            const { assert!(ROW_INFO_BATCH == 4, "four futures are joined below") };
            tokio::join!(one(0), one(1), one(2), one(3));
        }
    }

    async fn emit_row_info(&self, id: u64, title: TitleRef, client: &TmdbClient, region: &str) {
        if self.search_latest.get() != id {
            return;
        }
        let info = match client.details(title, region, Freshness::AllowStale).await {
            Ok(details) => {
                let imdb = self.imdb.lookup(details.imdb_id.as_deref());
                Some(search::hit_info(&self.build_card(&details, &imdb)))
            }
            Err(e) => {
                log_info!("details for {}/{} unavailable: {e}", title.media.as_str(), title.id);
                None
            }
        };
        if self.search_latest.get() == id {
            self.emit(Update::SearchHitInfo { id, title, info });
        }
    }

    /// The region's services must be known before a verdict can say "Included with": after a
    /// region change they may still be loading. Without services there's nothing to wait for.
    async fn ensure_catalog(self: &Rc<Self>, region: &str) {
        if self.settings.borrow().services.is_empty() {
            return;
        }
        if let Err(e) = self.catalog_for(region).await {
            log_info!("services for {region} unavailable, so verdicts can't name them: {e}");
        }
    }

    /// The title card for a result. Its details are usually cached already by the row.
    pub(super) async fn open_title(self: &Rc<Self>, id: u64, title: TitleRef) {
        let Some(client) = self.client() else {
            self.emit(Update::TitleCardFailed {
                id,
                error: ApiError::Unauthorized,
            });
            return;
        };
        let region = self.effective_region();
        self.ensure_catalog(&region).await;
        match client.details(title, &region, Freshness::AllowStale).await {
            Ok(details) => {
                let imdb = self.imdb.lookup(details.imdb_id.as_deref());
                let card = self.build_card(&details, &imdb);
                self.emit(Update::TitleCard {
                    id,
                    card: Box::new(card),
                });
            }
            Err(error) => {
                log_warn!("title {}/{} unavailable: {error}", title.media.as_str(), title.id);
                self.emit(Update::TitleCardFailed { id, error });
            }
        }
    }

    /// Thumbnails scaled to exact pixels with rounded corners, from TMDB's smallest poster
    /// size that covers them.
    pub(super) fn load_thumbs(self: &Rc<Self>, specs: Vec<ImageSpec>) {
        let Some(client) = self.client() else {
            for spec in specs {
                self.emit(Update::ThumbFailed(spec));
            }
            return;
        };
        for spec in specs {
            let worker = self.clone();
            let client = client.clone();
            tokio::task::spawn_local(async move {
                let size = size_for(ImageKind::Poster, spec.width);
                let decoded = match client.image(size, &spec.path).await {
                    Ok(bytes) => images::decode_poster(&bytes, spec.width, spec.height, spec.radius),
                    Err(e) => Err(e.to_string()),
                };
                match decoded {
                    Ok(pixels) => worker.emit(Update::Thumb { spec, pixels }),
                    Err(e) => {
                        log_info!("thumbnail {} unavailable: {e}", spec.path);
                        worker.emit(Update::ThumbFailed(spec));
                    }
                }
            });
        }
    }
}

//! Picks on the worker: the request built from the settings, the next pick prepared in the
//! background, this session's history behind "← Previous", exclusions, and the card's
//! poster and backdrop.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use randomovie::card::{self, ImdbBadge, TitleCard};
use randomovie::error::ApiError;
use randomovie::format;
use randomovie::imdb::ImdbStatus;
use randomovie::media::{MediaChoice, MediaType, TitleRef};
use randomovie::picker::genres::unify;
use randomovie::picker::{
    Pick, PickError, PickRequest, RECENT_PICKS_EXCLUDED, SkipList, UnifiedGenre,
    passes_imdb_minimums, reverify,
};
use randomovie::services::ServiceCatalog;
use randomovie::store::ExclusionReason;
use randomovie::tmdb::images::{ImageKind, size_for};
use randomovie::tmdb::model::TitleDetails;
use randomovie::tmdb::{Freshness, TmdbClient};
use randomovie::util::time::{DAY, now_unix};
use randomovie::{log_error, log_info, log_warn};
use tokio::task::JoinHandle;

use super::images;
use super::{CardImageKind, ImageSpec, Update, Worker};

/// Picks kept for "← Previous" in one session.
const HISTORY_LIMIT: usize = 100;
/// The TMDB backdrop size the soft background is made from.
const BACKDROP_SIZE: &str = "w300";

/// This session's picks and the card on screen.
#[derive(Default)]
pub(super) struct Session {
    /// Picks shown this session, oldest first.
    history: Vec<TitleRef>,
    /// The entry of `history` on screen.
    cursor: usize,
    /// The newest Pick or Previous request; an older one is dropped when it finishes.
    latest: u64,
    /// The filter bar's Movies / Series / Both choice, as last sent by the UI.
    media: Option<MediaChoice>,
    shown: Option<Shown>,
    /// Physical width of the poster on screen, so a prepared pick downloads that size.
    poster_width: Option<u32>,
}

/// The card on screen, so its IMDb badge can be filled in once the ratings finish loading.
struct Shown {
    id: u64,
    imdb_id: Option<String>,
    waiting_for_imdb: bool,
}

/// The next pick, prepared in the background for one request.
pub(super) struct Prefetch {
    request: PickRequest,
    task: JoinHandle<Result<Pick, PickError>>,
    /// Set when the task failed, so the next refresh tries again.
    failed: Rc<Cell<bool>>,
}

impl Worker {
    /// "Pick for me" / "Pick again".
    pub(super) async fn pick(self: &Rc<Self>, id: u64, media: MediaChoice) {
        {
            let mut session = self.session.borrow_mut();
            session.latest = id;
            session.media = Some(media);
        }
        let started = Instant::now();
        let result = self.next_pick(media).await;
        if self.session.borrow().latest != id {
            return; // replaced by a newer request
        }
        match result {
            Ok((pick, request, prepared)) => {
                let title = pick.details.title_ref();
                log_info!(
                    "pick {}/{} ready {} ms after the click{}",
                    title.media.as_str(),
                    title.id,
                    started.elapsed().as_millis(),
                    if prepared { " (prepared in the background)" } else { "" }
                );
                self.record(title);
                self.show(id, &pick.details, None);
                self.start_prefetch(request);
            }
            Err(error) => {
                log_warn!("pick failed: {error}");
                self.emit(Update::PickFailed { id, error });
            }
        }
    }

    /// The prepared pick if it was made for this request and still holds, else a new one.
    /// The flag tells whether the prepared pick was used.
    async fn next_pick(
        self: &Rc<Self>,
        media: MediaChoice,
    ) -> Result<(Pick, PickRequest, bool), PickError> {
        let request = self.pick_request(media).await?;
        if let Some(pick) = self.take_prefetched(&request).await {
            return Ok((pick, request, true));
        }
        let client = self.client().ok_or(PickError::Api(ApiError::Unauthorized))?;
        let skip = self.skip_list();
        let pick = self
            .picker
            .pick(client.as_ref(), &self.imdb, &request, &skip)
            .await?;
        Ok((pick, request, false))
    }

    async fn take_prefetched(self: &Rc<Self>, request: &PickRequest) -> Option<Pick> {
        let prefetch = self.prefetch.borrow_mut().take()?;
        if prefetch.request != *request {
            prefetch.task.abort();
            return None;
        }
        let pick = match prefetch.task.await {
            Ok(Ok(pick)) => pick,
            Ok(Err(e)) => {
                log_info!("the prepared pick failed ({e}); picking again");
                return None;
            }
            Err(_) => return None, // cancelled
        };
        self.still_holds(pick, request).await
    }

    /// A prepared pick may have waited a while. A title excluded since then is dropped,
    /// ratings that have finished loading are applied, and availability older than 24 hours
    /// is verified again.
    async fn still_holds(self: &Rc<Self>, pick: Pick, request: &PickRequest) -> Option<Pick> {
        let title = pick.details.title_ref();
        if self.excluded(title).is_some() {
            return None;
        }
        if now_unix() - pick.details.fetched_at > DAY {
            let client = self.client()?;
            return reverify(client.as_ref(), &self.imdb, request, title)
                .await
                .ok()
                .flatten();
        }
        let imdb = self.imdb.lookup(pick.details.imdb_id.as_deref());
        passes_imdb_minimums(&imdb, request).then_some(Pick { imdb, ..pick })
    }

    /// Everything that decides the next pick, from the current settings. Waits for the
    /// services or genres when the settings need them and they haven't loaded yet.
    async fn pick_request(self: &Rc<Self>, media: MediaChoice) -> Result<PickRequest, PickError> {
        if self.client().is_none() {
            return Err(PickError::Api(ApiError::Unauthorized));
        }
        let region = self.effective_region();
        let (wants_services, wants_genres) = {
            let settings = self.settings.borrow();
            (!settings.services.is_empty(), !settings.filters.genres.is_empty())
        };
        let services = if wants_services {
            let catalog = self.catalog_for(&region).await.map_err(PickError::Api)?;
            self.settings.borrow().services.expand(&catalog)
        } else {
            Vec::new()
        };
        let genres = if wants_genres {
            self.genre_list().await.map_err(PickError::Api)?
        } else {
            Rc::default()
        };
        let settings = self.settings.borrow();
        Ok(PickRequest::from_settings(&settings, &region, media, services, &genres))
    }

    async fn catalog_for(self: &Rc<Self>, region: &str) -> Result<Rc<ServiceCatalog>, ApiError> {
        let current = self.catalog.borrow().clone();
        match current {
            Some(catalog) if catalog.region == region => Ok(catalog),
            _ => self.load_catalog().await,
        }
    }

    /// The unified genre list, loaded once per session (TMDB's lists are cached for a week).
    pub(super) async fn genre_list(self: &Rc<Self>) -> Result<Rc<Vec<UnifiedGenre>>, ApiError> {
        let loaded = self.genres.borrow().clone();
        if let Some(genres) = loaded {
            return Ok(genres);
        }
        let client = self.client().ok_or(ApiError::Unauthorized)?;
        let (movie, tv) = tokio::join!(
            client.genres(MediaType::Movie),
            client.genres(MediaType::Tv)
        );
        let genres = Rc::new(unify(&movie?, &tv?));
        *self.genres.borrow_mut() = Some(genres.clone());
        self.emit(Update::Genres(genres.iter().map(|g| g.name.clone()).collect()));
        Ok(genres)
    }

    fn excluded(&self, title: TitleRef) -> Option<ExclusionReason> {
        self.store.exclusion_reason(title).unwrap_or_else(|e| {
            log_warn!("exclusion lookup failed: {e}");
            None
        })
    }

    /// "Seen it", "Not interested" and the last 50 picks.
    fn skip_list(&self) -> SkipList {
        let excluded = self.store.excluded_titles().unwrap_or_else(|e| {
            log_warn!("exclusions unavailable: {e}");
            HashSet::new()
        });
        let recent = match self.store.recent_picks(RECENT_PICKS_EXCLUDED) {
            Ok(picks) => picks.into_iter().collect(),
            Err(e) => {
                log_warn!("pick history unavailable: {e}");
                HashSet::new()
            }
        };
        SkipList { excluded, recent }
    }

    // ---- The prepared next pick ------------------------------------------------------

    /// Prepares the next pick for `request`, replacing any other one in progress.
    fn start_prefetch(self: &Rc<Self>, request: PickRequest) {
        let Some(client) = self.client() else { return };
        if let Some(old) = self.prefetch.borrow_mut().take() {
            old.task.abort();
        }
        let skip = self.skip_list();
        let failed = Rc::new(Cell::new(false));
        let worker = self.clone();
        let task_request = request.clone();
        let task_failed = failed.clone();
        let task = tokio::task::spawn_local(async move {
            let result = worker
                .picker
                .pick(client.as_ref(), &worker.imdb, &task_request, &skip)
                .await;
            match &result {
                Ok(pick) => worker.warm_images(&client, &pick.details).await,
                Err(_) => task_failed.set(true),
            }
            result
        });
        *self.prefetch.borrow_mut() = Some(Prefetch {
            request,
            task,
            failed,
        });
    }

    /// Prepares the next pick unless one is ready, or on its way, for the same request.
    /// Nothing is prepared before the first-run setup is finished.
    pub(super) async fn refresh_prefetch(self: &Rc<Self>, media: Option<MediaChoice>) {
        if !self.settings.borrow().setup_complete || self.client().is_none() {
            return;
        }
        let default_media = self.settings.borrow().default_media;
        let media = {
            let mut session = self.session.borrow_mut();
            if media.is_some() {
                session.media = media;
            }
            session.media.unwrap_or(default_media)
        };
        let request = match self.pick_request(media).await {
            Ok(request) => request,
            Err(e) => {
                log_info!("no pick prepared: {e}");
                return;
            }
        };
        let prepared = self
            .prefetch
            .borrow()
            .as_ref()
            .is_some_and(|p| p.request == request && !p.failed.get());
        if !prepared {
            self.start_prefetch(request);
        }
    }

    /// Stops preparing a pick (the token changed, so the old client must not be used).
    pub(super) fn cancel_prefetch(&self) {
        if let Some(prefetch) = self.prefetch.borrow_mut().take() {
            prefetch.task.abort();
        }
    }

    /// Downloads a prepared pick's poster and backdrop into the image cache, so showing it
    /// needs no network.
    async fn warm_images(&self, client: &TmdbClient, details: &TitleDetails) {
        let poster_width = self.session.borrow().poster_width;
        let poster = async {
            if let (Some(width), Some(path)) = (poster_width, details.poster_path.as_deref())
                && let Err(e) = client.image(size_for(ImageKind::Poster, width), path).await
            {
                log_info!("poster for the prepared pick not downloaded: {e}");
            }
        };
        let backdrop = async {
            if let Some(path) = details.backdrop_path.as_deref()
                && let Err(e) = client.image(BACKDROP_SIZE, path).await
            {
                log_info!("backdrop for the prepared pick not downloaded: {e}");
            }
        };
        tokio::join!(poster, backdrop);
    }

    // ---- History, the card on screen and exclusions --------------------------------

    /// "← Previous": the pick before the one on screen, with availability for the current
    /// region (from the cache while it's under 24 hours old).
    pub(super) async fn previous(self: &Rc<Self>, id: u64) {
        let target = {
            let mut session = self.session.borrow_mut();
            session.latest = id;
            let index = session.cursor.saturating_sub(1);
            session.history.get(index).map(|&title| (index, title))
        };
        let (Some((index, title)), Some(client)) = (target, self.client()) else {
            return;
        };
        let region = self.effective_region();
        let result = client.details(title, &region, Freshness::AllowStale).await;
        if self.session.borrow().latest != id {
            return;
        }
        match result {
            Ok(details) => {
                self.session.borrow_mut().cursor = index;
                let excluded = self.excluded(title);
                self.show(id, &details, excluded);
            }
            Err(error) => self.emit(Update::PickFailed {
                id,
                error: PickError::Api(error),
            }),
        }
    }

    fn record(&self, title: TitleRef) {
        if let Err(e) = self.store.record_pick(title, now_unix()) {
            log_warn!("pick history couldn't be saved: {e}");
        }
        let mut session = self.session.borrow_mut();
        session.history.push(title);
        if session.history.len() > HISTORY_LIMIT {
            session.history.remove(0);
        }
        session.cursor = session.history.len() - 1;
    }

    fn show(&self, id: u64, details: &TitleDetails, excluded: Option<ExclusionReason>) {
        let imdb = self.imdb.lookup(details.imdb_id.as_deref());
        let card = self.build_card(details, &imdb);
        let can_go_back = {
            let mut session = self.session.borrow_mut();
            session.shown = Some(Shown {
                id,
                imdb_id: details.imdb_id.clone(),
                waiting_for_imdb: imdb == ImdbStatus::Loading,
            });
            session.cursor > 0
        };
        self.emit(Update::Card {
            id,
            card: Box::new(card),
            can_go_back,
            excluded,
        });
    }

    /// The card for the current region, services and catalog.
    fn build_card(&self, details: &TitleDetails, imdb: &ImdbStatus) -> TitleCard {
        let region = self.effective_region();
        let catalog = self
            .catalog
            .borrow()
            .clone()
            .filter(|c| c.region == region)
            .unwrap_or_default();
        let services = self.settings.borrow().services.expand_set(&catalog);
        card::build(details, imdb, &services, &catalog, &self.region_name(&region))
    }

    pub(super) fn region_name(&self, code: &str) -> String {
        self.regions
            .borrow()
            .iter()
            .find(|r| r.code == code)
            .map_or_else(|| code.to_owned(), |r| r.english_name.clone())
    }

    /// Fills in the IMDb badge of the card on screen once the ratings have loaded.
    pub(super) fn refresh_card_imdb(&self) {
        let (id, imdb_id) = match &self.session.borrow().shown {
            Some(shown) if shown.waiting_for_imdb => (shown.id, shown.imdb_id.clone()),
            _ => return,
        };
        let status = self.imdb.lookup(imdb_id.as_deref());
        if status == ImdbStatus::Loading {
            return;
        }
        if let Some(shown) = self.session.borrow_mut().shown.as_mut() {
            shown.waiting_for_imdb = false;
        }
        self.emit(Update::CardImdb {
            id,
            text: format::imdb_badge(&status),
            badge: ImdbBadge::of(&status),
        });
    }

    /// "Seen it" / "Not interested" (`Some`) or their undo (`None`).
    pub(super) fn set_exclusion(&self, title: TitleRef, reason: Option<ExclusionReason>) {
        let saved = match reason {
            Some(reason) => self.store.exclude(title, reason, now_unix()),
            None => self.store.unexclude(title),
        };
        let reason = match saved {
            Ok(()) => reason,
            Err(e) => {
                log_error!("the exclusion couldn't be saved: {e}");
                self.excluded(title)
            }
        };
        self.emit(Update::Excluded { title, reason });
    }

    // ---- Card images -----------------------------------------------------------------

    pub(super) fn set_poster_width(&self, width: u32) {
        self.session.borrow_mut().poster_width = Some(width);
    }

    /// Decodes the poster and backdrop at the exact physical sizes the card shows them.
    pub(super) fn load_card_images(
        self: &Rc<Self>,
        id: u64,
        poster: Option<ImageSpec>,
        backdrop: Option<ImageSpec>,
    ) {
        let Some(client) = self.client() else { return };
        if let Some(spec) = &poster {
            self.session.borrow_mut().poster_width = Some(spec.width);
        }
        for (kind, spec) in [
            (CardImageKind::Poster, poster),
            (CardImageKind::Backdrop, backdrop),
        ] {
            let Some(spec) = spec else { continue };
            let worker = self.clone();
            let client = client.clone();
            tokio::task::spawn_local(async move {
                let size = match kind {
                    CardImageKind::Poster => size_for(ImageKind::Poster, spec.width),
                    CardImageKind::Backdrop => BACKDROP_SIZE,
                };
                let decoded = match client.image(size, &spec.path).await {
                    Ok(bytes) => match kind {
                        CardImageKind::Poster => {
                            images::decode_poster(&bytes, spec.width, spec.height, spec.radius)
                        }
                        CardImageKind::Backdrop => {
                            images::decode_backdrop(&bytes, spec.width, spec.height, spec.radius)
                        }
                    },
                    Err(e) => Err(e.to_string()),
                };
                match decoded {
                    Ok(pixels) => worker.emit(Update::CardImage { id, kind, pixels }),
                    Err(e) => log_warn!("{kind:?} {} unavailable: {e}", spec.path),
                }
            });
        }
    }
}

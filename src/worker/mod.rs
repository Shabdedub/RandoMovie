//! The worker thread: one current-thread Tokio runtime for all network, disk, parsing and
//! image decoding. The UI sends `Command`s over a channel and never waits; results come
//! back as `Update`s through a sink that posts them to the UI thread's event loop.

mod images;
mod library;
mod links;
mod messages;
mod pick;
mod search;

pub use messages::{
    CardImageKind, Command, ImageSpec, ImdbInfo, LibraryItem, LogoRequest, ServiceLinkRequest,
    TokenSource, TokenStatus, Update,
};

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use randomovie::error::ApiError;
use randomovie::imdb::{ImdbError, ImdbRatings, RefreshOutcome};
use randomovie::media::{MediaType, TitleRef};
use randomovie::paths::AppPaths;
use randomovie::picker::{Picker, UnifiedGenre};
use randomovie::platform::{credentials, region};
use randomovie::services::ServiceCatalog;
use randomovie::store::{Settings, Store};
use randomovie::tmdb::model::Region;
use randomovie::tmdb::images::{ImageKind, size_for};
use randomovie::tmdb::{Credential, IMAGE_CACHE_CAP_BYTES, TmdbClient, build_http_client, ttl};
use randomovie::util::time::{DAY, now_unix};
use randomovie::{log_error, log_info, log_warn};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Delivers updates to the UI thread.
pub type Sink = Box<dyn Fn(Update) + Send>;

/// Responses older than this are deleted at startup; younger stale ones still serve offline.
const HTTP_CACHE_KEEP_SECS: i64 = 30 * DAY;

pub struct WorkerHandle {
    sender: UnboundedSender<Command>,
    done: std::sync::mpsc::Receiver<()>,
}

impl WorkerHandle {
    pub fn send(&self, command: Command) {
        if self.sender.send(command).is_err() {
            log_warn!("the worker thread has stopped");
        }
    }

    pub fn sender(&self) -> UnboundedSender<Command> {
        self.sender.clone()
    }

    /// Lets the worker finish the commands already queued, then waits for it to stop.
    pub fn shutdown(self, timeout: Duration) {
        let _ = self.sender.send(Command::Shutdown);
        if self.done.recv_timeout(timeout).is_err() {
            log_warn!("the worker didn't stop within {timeout:?}");
        }
    }
}

pub fn spawn(paths: AppPaths, sink: Sink) -> std::io::Result<WorkerHandle> {
    let (sender, receiver) = unbounded_channel();
    let (done_sender, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("worker".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    let local = tokio::task::LocalSet::new();
                    local.block_on(&runtime, run(receiver, paths, sink));
                }
                Err(e) => log_error!("the worker runtime failed to start: {e}"),
            }
            let _ = done_sender.send(());
        })?;
    Ok(WorkerHandle { sender, done })
}

async fn run(mut receiver: UnboundedReceiver<Command>, paths: AppPaths, sink: Sink) {
    let worker = Rc::new(Worker::new(paths, sink));
    while let Some(command) = receiver.recv().await {
        match command {
            Command::Shutdown => break,
            // Saved in order, so a later command always sees the new settings.
            Command::SaveSettings(settings) => worker.save_settings(*settings),
            command => {
                let worker = worker.clone();
                tokio::task::spawn_local(async move { worker.handle(command).await });
            }
        }
    }
}

struct Worker {
    paths: AppPaths,
    sink: Sink,
    http: Option<reqwest::Client>,
    store: Arc<Store>,
    imdb: ImdbRatings,
    settings: RefCell<Settings>,
    /// Windows' region guesses in order; the first TMDB supports becomes `detected_region`.
    region_candidates: RefCell<Vec<String>>,
    detected_region: RefCell<String>,
    client: RefCell<Option<Rc<TmdbClient>>>,
    /// TMDB's region list, for region names.
    regions: RefCell<Vec<Region>>,
    /// The services of the current region, for expanding the selection and naming chips.
    catalog: RefCell<Option<Rc<ServiceCatalog>>>,
    genres: RefCell<Option<Rc<Vec<UnifiedGenre>>>>,
    /// Session cache of pool sizes and the random generator.
    picker: Picker,
    session: RefCell<pick::Session>,
    prefetch: RefCell<Option<pick::Prefetch>>,
    /// The newest search; work for older ones stops.
    search_latest: Cell<u64>,
    /// Titles listed so far for that search, so "Load more" never repeats one.
    search_seen: RefCell<HashSet<TitleRef>>,
    /// Each title's ids on streaming services, by IMDb id, shared by the lookup a card starts
    /// and a button press.
    service_id_lookups: RefCell<HashMap<String, Rc<tokio::sync::OnceCell<randomovie::watch_links::ServiceIds>>>>,
}

impl Worker {
    fn new(paths: AppPaths, sink: Sink) -> Worker {
        let store = Store::open(&paths.database_file()).unwrap_or_else(|e| {
            log_error!("the cache database couldn't be opened, using memory instead: {e}");
            Store::open_in_memory().expect("in-memory SQLite always opens")
        });
        let http = build_http_client()
            .inspect_err(|e| log_error!("no HTTP client: {e}"))
            .ok();
        Worker {
            imdb: ImdbRatings::open(&paths.imdb_dir()),
            paths,
            sink,
            http,
            store: Arc::new(store),
            settings: RefCell::new(Settings::default()),
            region_candidates: RefCell::new(Vec::new()),
            detected_region: RefCell::new(region::FALLBACK_REGION.to_owned()),
            client: RefCell::new(None),
            regions: RefCell::new(Vec::new()),
            catalog: RefCell::new(None),
            genres: RefCell::new(None),
            picker: Picker::new(),
            session: RefCell::new(pick::Session::default()),
            prefetch: RefCell::new(None),
            search_latest: Cell::new(0),
            search_seen: RefCell::new(HashSet::new()),
            service_id_lookups: RefCell::new(HashMap::new()),
        }
    }

    fn emit(&self, update: Update) {
        (self.sink)(update);
    }

    fn client(&self) -> Option<Rc<TmdbClient>> {
        self.client.borrow().clone()
    }

    fn effective_region(&self) -> String {
        let chosen = self.settings.borrow().region.clone();
        chosen.unwrap_or_else(|| self.detected_region.borrow().clone())
    }

    async fn handle(self: Rc<Self>, command: Command) {
        match command {
            Command::Startup(settings) => self.startup(*settings).await,
            Command::SubmitToken(token) => self.submit_token(token).await,
            Command::RemoveToken => self.remove_token().await,
            Command::LoadCatalog => {
                // A new region or a retry: the prepared pick must follow the new catalog.
                if self.load_catalog().await.is_ok() {
                    self.refresh_prefetch(None).await;
                }
            }
            Command::LoadRegions => self.load_regions().await,
            Command::LoadLogos(requests) => self.load_logos(requests),
            Command::RefreshImdb => self.refresh_imdb(true).await,
            Command::ClearCache => self.clear_cache(),
            Command::NetworkChanged(online) => self.network_changed(online).await,
            Command::Pick { id, media } => self.pick(id, media).await,
            Command::Prefetch { media } => self.refresh_prefetch(Some(media)).await,
            Command::Previous { id } => self.previous(id).await,
            Command::Exclude { title, reason } => self.set_exclusion(title, Some(reason)).await,
            Command::Unexclude { title } => self.set_exclusion(title, None).await,
            Command::PosterWidth(width) => self.set_poster_width(width),
            Command::LoadCardImages {
                id,
                poster,
                backdrop,
            } => self.load_card_images(id, poster, backdrop),
            Command::Search { id, query, page } => self.search(id, query, page).await,
            Command::SearchInfo { id, titles } => self.load_row_info(id, titles).await,
            Command::LoadThumbs(specs) => self.load_thumbs(specs),
            Command::OpenTitle { id, title } => self.open_title(id, title).await,
            Command::PrepareServiceLinks { imdb_id } => self.prepare_service_links(imdb_id).await,
            Command::OpenServiceLink(request) => self.open_service_link(request).await,
            Command::LoadLibrary { id } => self.load_library(id).await,
            Command::Restore(item) => self.restore(item),
            Command::SaveSettings(_) | Command::Shutdown => {}
        }
    }

    async fn startup(self: &Rc<Self>, settings: Settings) {
        *self.settings.borrow_mut() = settings;
        let candidates = region::candidates();
        // The built-in list decides until TMDB's live list loads.
        let detected = region::choose(&candidates, &region::known_tmdb_regions());
        *self.region_candidates.borrow_mut() = candidates;
        *self.detected_region.borrow_mut() = detected.clone();
        self.emit(Update::DetectedRegion(detected));
        self.maintain_cache();
        self.emit_cache_size();
        self.emit_imdb(false, None);

        // IMDb ratings need no token, so they download in parallel with everything else.
        let worker = self.clone();
        tokio::task::spawn_local(async move { worker.refresh_imdb(false).await });

        match resolve_token() {
            Some((token, source)) => self.connect(&token, source).await,
            None => self.emit(Update::Token(TokenStatus::Missing)),
        }
    }

    fn maintain_cache(&self) {
        let now = now_unix();
        if let Err(e) = self.store.http_purge(now, HTTP_CACHE_KEEP_SECS) {
            log_warn!("cache cleanup failed: {e}");
        }
        let image_max_age = ttl::IMAGES.as_secs() as i64;
        if let Err(e) = self
            .store
            .image_maintain(now, image_max_age, IMAGE_CACHE_CAP_BYTES)
        {
            log_warn!("image cache cleanup failed: {e}");
        }
    }

    /// Starts using a token: a fresh client, then the region list and services.
    async fn connect(self: &Rc<Self>, token: &str, source: TokenSource) {
        let (Some(credential), Some(http)) = (Credential::from_secret(token), self.http.clone())
        else {
            self.emit(Update::Token(TokenStatus::Missing));
            return;
        };
        let client = Rc::new(TmdbClient::with_http(http, credential, self.store.clone()));
        *self.client.borrow_mut() = Some(client.clone());
        self.emit(Update::Token(TokenStatus::Ready(source)));
        self.watch_online(&client);
        // Regions first: they settle which country "Automatic" means.
        self.load_regions().await;
        let _ = self.load_catalog().await;
        if let Err(e) = self.genre_list().await {
            log_warn!("genre list unavailable: {e}");
        }
        // With everything loaded, the first "Pick for me" can be answered at once.
        self.refresh_prefetch(None).await;
    }

    fn watch_online(self: &Rc<Self>, client: &Rc<TmdbClient>) {
        let mut online = client.subscribe_online();
        let worker = self.clone();
        tokio::task::spawn_local(async move {
            // Ends when this client is replaced and its sender dropped.
            while online.changed().await.is_ok() {
                let now_online = *online.borrow_and_update();
                worker.emit(Update::Online(now_online));
            }
        });
    }

    async fn submit_token(self: &Rc<Self>, token: String) {
        let token = token.trim().to_owned();
        let (Some(credential), Some(http)) = (Credential::from_secret(&token), self.http.clone())
        else {
            let message = "Paste your TMDB API Read Access Token first.".to_owned();
            self.emit(Update::Token(TokenStatus::Rejected(message)));
            return;
        };
        self.emit(Update::Token(TokenStatus::Checking));
        let probe = TmdbClient::with_http(http, credential, self.store.clone());
        match probe.validate().await {
            Ok(()) => {
                if let Err(e) = credentials::write(credentials::TMDB_TARGET, &token) {
                    log_error!("Credential Manager write failed: {e}");
                    let message = format!(
                        "The token works, but Windows Credential Manager couldn't store it: {e}"
                    );
                    self.emit(Update::Token(TokenStatus::Rejected(message)));
                    return;
                }
                log_info!("TMDB token validated and stored in Credential Manager");
                self.connect(&token, TokenSource::CredentialManager).await;
            }
            Err(e) => {
                let message = match e {
                    ApiError::Unauthorized => "TMDB didn't accept this token. Copy the API Read Access Token, the long one, from your TMDB API settings.".to_owned(),
                    e if e.is_connectivity() => format!("Couldn't reach TMDB ({e}). Check your connection and try again."),
                    e => e.to_string(),
                };
                self.emit(Update::Token(TokenStatus::Rejected(message)));
            }
        }
    }

    async fn remove_token(self: &Rc<Self>) {
        if let Err(e) = credentials::delete(credentials::TMDB_TARGET) {
            log_warn!("Credential Manager delete failed: {e}");
        }
        self.cancel_prefetch();
        *self.client.borrow_mut() = None;
        log_info!("TMDB token removed from Credential Manager");
        match resolve_token() {
            Some((token, source)) => self.connect(&token, source).await,
            None => self.emit(Update::Token(TokenStatus::Missing)),
        }
    }

    async fn load_regions(self: &Rc<Self>) {
        let Some(client) = self.client() else { return };
        match client.regions().await {
            Ok(regions) => {
                let supported: Vec<String> = regions.iter().map(|r| r.code.clone()).collect();
                let best = region::choose(&self.region_candidates.borrow(), &supported);
                if best != *self.detected_region.borrow() {
                    log_info!("TMDB has no streaming data for the Windows region; using {best}");
                    *self.detected_region.borrow_mut() = best.clone();
                    self.emit(Update::DetectedRegion(best));
                }
                *self.regions.borrow_mut() = regions.clone();
                self.emit(Update::Regions(regions));
            }
            Err(ApiError::Unauthorized) => self.emit(Update::Unauthorized),
            Err(e) => log_warn!("region list unavailable: {e}"),
        }
    }

    /// The services of the current region. The worker keeps a copy for expanding the
    /// selection and naming chips; the UI gets its own for the tiles.
    async fn load_catalog(self: &Rc<Self>) -> Result<Rc<ServiceCatalog>, ApiError> {
        let Some(client) = self.client() else {
            return Err(ApiError::Unauthorized);
        };
        let region = self.effective_region();
        self.emit(Update::CatalogLoading);
        let (movie, tv) = tokio::join!(
            client.providers(MediaType::Movie, &region),
            client.providers(MediaType::Tv, &region)
        );
        match movie.and_then(|movie| tv.map(|tv| (movie, tv))) {
            Ok((movie, tv)) => {
                let catalog = Rc::new(ServiceCatalog::from_lists(&region, &movie, &tv));
                log_info!("{} services listed in {region}", catalog.groups.len());
                *self.catalog.borrow_mut() = Some(catalog.clone());
                self.emit(Update::Catalog(Box::new((*catalog).clone())));
                Ok(catalog)
            }
            Err(e) => {
                log_warn!("services for {region} unavailable: {e}");
                if e == ApiError::Unauthorized {
                    self.emit(Update::Unauthorized);
                }
                self.emit(Update::CatalogFailed(e.to_string()));
                Err(e)
            }
        }
    }

    /// Fetches and decodes logos concurrently; the client limits network concurrency.
    fn load_logos(self: &Rc<Self>, requests: Vec<LogoRequest>) {
        let Some(client) = self.client() else {
            for request in requests {
                self.emit(Update::LogoFailed {
                    path: request.path,
                    px: request.px,
                });
            }
            return;
        };
        for request in requests {
            let worker = self.clone();
            let client = client.clone();
            tokio::task::spawn_local(async move {
                let size = size_for(ImageKind::Logo, request.px);
                let decoded = match client.image(size, &request.path).await {
                    Ok(bytes) => images::decode_logo(&bytes, request.px),
                    Err(e) => Err(e.to_string()),
                };
                match decoded {
                    Ok(pixels) => worker.emit(Update::Logo {
                        path: request.path,
                        px: request.px,
                        pixels,
                    }),
                    Err(e) => {
                        log_warn!("logo {} unavailable: {e}", request.path);
                        worker.emit(Update::LogoFailed {
                            path: request.path,
                            px: request.px,
                        });
                    }
                }
            });
        }
    }

    /// Downloads IMDb ratings when the local copy is missing or a week old, or when the user
    /// asks (`requested`); the download is conditional, so an unchanged file costs one request.
    async fn refresh_imdb(self: &Rc<Self>, requested: bool) {
        if !requested && !self.imdb.needs_refresh(now_unix()) {
            return;
        }
        let Some(http) = self.http.clone() else { return };
        self.emit_imdb(true, None);
        let message = match self.imdb.refresh(&http, false).await {
            Ok(RefreshOutcome::Updated { .. }) => "Updated just now".to_owned(),
            Ok(RefreshOutcome::NotModified) => "Already up to date".to_owned(),
            Err(ImdbError::Busy) => return,
            Err(e) => e.to_string(),
        };
        self.emit_imdb(false, Some(message));
        self.refresh_card_imdb();
    }

    fn emit_imdb(&self, refreshing: bool, message: Option<String>) {
        let meta = self.imdb.meta();
        self.emit(Update::Imdb(ImdbInfo {
            ready: self.imdb.is_ready(),
            refreshing,
            data_date: meta.data_date(),
            records: meta.records,
            message,
        }));
    }

    fn clear_cache(&self) {
        if let Err(e) = self.store.clear_cache() {
            log_warn!("clearing the cache failed: {e}");
        }
        self.emit_cache_size();
    }

    fn emit_cache_size(&self) {
        match self.store.cache_bytes() {
            Ok(bytes) => self.emit(Update::CacheSize(bytes)),
            Err(e) => log_warn!("cache size unavailable: {e}"),
        }
    }

    /// Back online: one small request clears the offline flag, then anything that failed
    /// while offline is retried.
    async fn network_changed(self: &Rc<Self>, online: bool) {
        if !online {
            return;
        }
        log_info!("network connectivity changed: online");
        if let Some(client) = self.client()
            && client.validate().await.is_ok()
        {
            self.load_regions().await;
            let _ = self.load_catalog().await;
            if let Err(e) = self.genre_list().await {
                log_warn!("genre list unavailable: {e}");
            }
            // A pick that failed to prepare while offline is tried again.
            self.refresh_prefetch(None).await;
        }
        self.refresh_imdb(false).await;
    }

    fn save_settings(&self, settings: Settings) {
        if let Err(e) = settings.save(&self.paths.settings_file()) {
            log_error!("settings couldn't be saved: {e}");
        }
        *self.settings.borrow_mut() = settings;
    }
}

/// The token from Windows Credential Manager, else the `TMDB_TOKEN` environment variable.
fn resolve_token() -> Option<(String, TokenSource)> {
    match credentials::read(credentials::TMDB_TARGET) {
        Ok(Some(token)) => return Some((token, TokenSource::CredentialManager)),
        Ok(None) => {}
        Err(e) => log_warn!("Credential Manager read failed: {e}"),
    }
    std::env::var("TMDB_TOKEN")
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .map(|token| (token, TokenSource::Environment))
}

//! Messages between the UI thread and the worker thread.

use randomovie::card::{ImdbBadge, TitleCard};
use randomovie::error::ApiError;
use randomovie::imdb::ImdbStatus;
use randomovie::media::{MediaChoice, TitleRef};
use randomovie::picker::PickError;
use randomovie::search::{HitInfo, SearchHit};
use randomovie::services::ServiceCatalog;
use randomovie::store::{ExclusionReason, Settings, TitleSummary};
use randomovie::tmdb::model::Region;
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// UI to worker.
pub enum Command {
    /// First command: settings as loaded at launch.
    Startup(Box<Settings>),
    /// Persist settings; handled in order, before any later command.
    SaveSettings(Box<Settings>),
    /// Validate a token and keep it in Credential Manager.
    SubmitToken(String),
    RemoveToken,
    /// Services for the current region.
    LoadCatalog,
    LoadRegions,
    /// Logos scaled to exact physical pixels.
    LoadLogos(Vec<LogoRequest>),
    /// "Refresh now" for IMDb ratings.
    RefreshImdb,
    ClearCache,
    /// From Windows' connectivity notifications.
    NetworkChanged(bool),
    /// "Pick for me" / "Pick again". `id` identifies the answer.
    Pick { id: u64, media: MediaChoice },
    /// Something that shapes picks changed (filters, services, region): prepare the next
    /// pick in the background so "Pick again" stays instant.
    Prefetch { media: MediaChoice },
    /// "← Previous": the pick before the one on screen.
    Previous { id: u64 },
    /// "Seen it" / "Not interested".
    Exclude { title: TitleRef, reason: ExclusionReason },
    /// Undo of an exclusion.
    Unexclude { title: TitleRef },
    /// The poster's width on screen in physical pixels, so a prepared pick downloads that
    /// size before any card has asked for images.
    PosterWidth(u32),
    /// Poster and backdrop for the card answering `id`, at exact physical sizes.
    LoadCardImages {
        id: u64,
        poster: Option<ImageSpec>,
        backdrop: Option<ImageSpec>,
    },
    /// One page of results for `query`. Every page of one query shares its `id`; a new id
    /// makes older searches stop.
    Search { id: u64, query: String, page: u32 },
    /// Rating and availability again for rows already shown (services, region or IMDb
    /// ratings changed).
    SearchInfo { id: u64, titles: Vec<TitleRef> },
    /// Result thumbnails at exact physical sizes.
    LoadThumbs(Vec<ImageSpec>),
    /// The title card for a search result or a Library title.
    OpenTitle { id: u64, title: TitleRef },
    /// Every title in the Library. `id` identifies the answer.
    LoadLibrary { id: u64 },
    /// Undo in the Library: the title marked again exactly as it was.
    Restore(LibraryItem),
    Shutdown,
}

/// A title in the Library: marked "Seen it" or "Not interested".
#[derive(Debug, Clone)]
pub struct LibraryItem {
    pub title: TitleRef,
    pub reason: ExclusionReason,
    /// When it was marked, in unix seconds.
    pub marked_at: i64,
    /// `None` while the details of a title marked before the Library existed load.
    pub summary: Option<TitleSummary>,
    pub imdb: ImdbStatus,
    /// Its details couldn't be loaded: offline, or TMDB no longer has it.
    pub unavailable: bool,
}

/// An image path and the exact physical pixels it will be drawn at.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImageSpec {
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// Corner radius in physical pixels.
    pub radius: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardImageKind {
    Poster,
    Backdrop,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogoRequest {
    pub path: String,
    pub px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    CredentialManager,
    Environment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStatus {
    Missing,
    Checking,
    Rejected(String),
    Ready(TokenSource),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImdbInfo {
    pub ready: bool,
    pub refreshing: bool,
    pub data_date: Option<i64>,
    pub records: u64,
    pub message: Option<String>,
}

/// Worker to UI.
pub enum Update {
    DetectedRegion(String),
    Token(TokenStatus),
    /// TMDB answered 401: ask for a new token.
    Unauthorized,
    Regions(Vec<Region>),
    CatalogLoading,
    Catalog(Box<ServiceCatalog>),
    CatalogFailed(String),
    Logo {
        path: String,
        px: u32,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
    },
    LogoFailed {
        path: String,
        px: u32,
    },
    Online(bool),
    Imdb(ImdbInfo),
    CacheSize(u64),
    /// The unified genre names for the filter bar.
    Genres(Vec<String>),
    /// A pick (or the previous one) to show.
    Card {
        id: u64,
        card: Box<TitleCard>,
        can_go_back: bool,
        excluded: Option<ExclusionReason>,
    },
    PickFailed {
        id: u64,
        error: PickError,
    },
    CardImage {
        id: u64,
        kind: CardImageKind,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
    },
    /// The IMDb badge of the card on screen, once ratings finish loading.
    CardImdb {
        id: u64,
        text: String,
        badge: ImdbBadge,
    },
    /// A title's exclusion changed.
    Excluded {
        title: TitleRef,
        reason: Option<ExclusionReason>,
    },
    /// A page of search results; rating and availability follow per row.
    SearchResults {
        id: u64,
        page: u32,
        total_pages: u32,
        hits: Vec<SearchHit>,
    },
    SearchFailed {
        id: u64,
        page: u32,
        error: ApiError,
    },
    /// A row's rating and availability; `None` when its details couldn't be loaded.
    SearchHitInfo {
        id: u64,
        title: TitleRef,
        info: Option<HitInfo>,
    },
    Thumb {
        spec: ImageSpec,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
    },
    ThumbFailed(ImageSpec),
    /// The card for an opened search result or Library title.
    TitleCard {
        id: u64,
        card: Box<TitleCard>,
    },
    TitleCardFailed {
        id: u64,
        error: ApiError,
    },
    /// The Library's titles, the most recently marked first.
    Library {
        id: u64,
        items: Vec<LibraryItem>,
    },
    /// A Library title whose details have just loaded, or couldn't be.
    LibraryItem {
        id: u64,
        item: LibraryItem,
    },
}

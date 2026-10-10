//! Messages between the UI thread and the worker thread.

use randomovie::card::{ImdbBadge, TitleCard};
use randomovie::media::{MediaChoice, TitleRef};
use randomovie::picker::PickError;
use randomovie::services::ServiceCatalog;
use randomovie::store::{ExclusionReason, Settings};
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
    Shutdown,
}

/// An image path and the exact physical pixels it will be drawn at.
#[derive(Debug, Clone, PartialEq)]
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
}

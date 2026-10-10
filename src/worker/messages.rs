//! Messages between the UI thread and the worker thread.

use randomovie::services::ServiceCatalog;
use randomovie::store::Settings;
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
    Shutdown,
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
}

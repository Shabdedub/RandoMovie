//! Local storage: one SQLite database for cached responses, images, pick history and
//! exclusions, plus the settings file.

mod cache;
mod db;
mod history;
pub mod settings;

pub use cache::CachedBody;
pub use db::Store;
pub use history::{ExclusionReason, MarkedTitle, TitleSummary};
pub use settings::{Filters, Settings, Theme, WindowGeometry};

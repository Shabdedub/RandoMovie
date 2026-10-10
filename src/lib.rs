//! RandoMovie data layer: the TMDB client, the IMDb ratings index, local storage and the
//! random picker. Everything here runs on the worker thread and is testable without a window.

pub mod card;
pub mod error;
pub mod format;
pub mod imdb;
pub mod media;
pub mod paths;
pub mod picker;
#[cfg(windows)]
pub mod platform;
pub mod services;
pub mod store;
pub mod tmdb;
pub mod util;

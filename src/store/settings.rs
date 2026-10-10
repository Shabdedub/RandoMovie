//! `%APPDATA%\RandoMovie\settings.json`: versioned, migrated on load, sanitised, and saved
//! atomically. The TMDB token is never stored here; it lives in Windows Credential Manager.
//!
//! Version history:
//! - 0: no `version` field. This is the hand-written shorthand that the README documents:
//!   `"services": ["Netflix", "Stan"]` as plain names, and the IMDb minimums at the top level
//!   (`"min_imdb_rating"`, `"min_imdb_votes"`). Any field may be missing.
//! - 1: the current layout written by the app.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::media::MediaChoice;
use crate::services::{ServiceSelection, group_key};
use crate::{log_info, log_warn};

pub const SETTINGS_VERSION: u32 = 1;
pub const DEFAULT_MIN_IMDB_RATING: f32 = 6.0;
pub const DEFAULT_MIN_IMDB_VOTES: u32 = 1000;
pub const DEFAULT_OBSCURITY_FLOOR: u32 = 50;
pub const MIN_WINDOW_WIDTH: u32 = 800;
pub const MIN_WINDOW_HEIGHT: u32 = 560;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    #[default]
    Dark,
    Light,
}

/// The filter bar. `None` means "Any".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    /// Unified genre names (see `picker::genres`); empty means any genre.
    pub genres: Vec<String>,
    pub min_imdb_rating: Option<f32>,
    pub min_imdb_votes: Option<u32>,
    pub year_from: Option<u16>,
    pub year_to: Option<u16>,
    /// Maximum runtime in minutes; applies to movies only.
    pub max_runtime: Option<u16>,
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            genres: Vec::new(),
            min_imdb_rating: Some(DEFAULT_MIN_IMDB_RATING),
            min_imdb_votes: Some(DEFAULT_MIN_IMDB_VOTES),
            year_from: None,
            year_to: None,
            max_runtime: None,
        }
    }
}

/// Window position and size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// ISO 3166-1 code chosen by the user; `None` means "detect from Windows".
    pub region: Option<String>,
    pub services: ServiceSelection,
    pub include_free: bool,
    pub theme: Theme,
    pub default_media: MediaChoice,
    /// Minimum TMDB vote count for random picks.
    pub obscurity_floor: u32,
    pub filters: Filters,
    pub window: Option<WindowGeometry>,
    /// True once the first-run flow (token, region, services) has been completed.
    pub setup_complete: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            region: None,
            services: ServiceSelection::default(),
            include_free: true,
            theme: Theme::Dark,
            default_media: MediaChoice::Both,
            obscurity_floor: DEFAULT_OBSCURITY_FLOOR,
            filters: Filters::default(),
            window: None,
            setup_complete: false,
        }
    }
}

impl Settings {
    /// Parses settings JSON of any known version. Returns the settings and the version the
    /// text was written in.
    pub fn from_json(text: &str) -> Result<(Settings, u32), serde_json::Error> {
        let mut value: Value = serde_json::from_str(text)?;
        let from = value
            .get("version")
            .and_then(Value::as_u64)
            .map_or(0, |v| v.min(u64::from(u32::MAX)) as u32);
        if from == 0
            && let Value::Object(map) = &mut value
        {
            migrate_v0(map);
        }
        let mut settings: Settings = serde_json::from_value(value)?;
        settings.version = SETTINGS_VERSION;
        settings.sanitize();
        Ok((settings, from))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Loads settings, falling back to defaults when the file is missing. An unreadable file
    /// is kept as `settings.json.bak` before defaults are used, so nothing is lost silently.
    pub fn load(path: &Path) -> Settings {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Settings::default(),
            Err(e) => {
                log_warn!("couldn't read settings, using defaults: {e}");
                return Settings::default();
            }
        };
        match Settings::from_json(&text) {
            Ok((settings, from)) => {
                if from != SETTINGS_VERSION {
                    log_info!("migrated settings from version {from} to {SETTINGS_VERSION}");
                }
                settings
            }
            Err(e) => {
                let backup = backup_path(path);
                let _ = fs::copy(path, &backup);
                log_warn!(
                    "settings file was invalid ({e}); kept a copy at {} and reset to defaults",
                    backup.display()
                );
                Settings::default()
            }
        }
    }

    /// Writes to a temporary file and renames it over the old one, so a crash mid-write
    /// never leaves a truncated settings file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, self.to_json())?;
        fs::rename(&tmp, path)
    }

    /// Clamps values to sensible ranges so a hand-edited file can't break the app.
    pub fn sanitize(&mut self) {
        self.region = self
            .region
            .take()
            .map(|r| r.trim().to_ascii_uppercase())
            .filter(|r| r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase()));
        self.obscurity_floor = self.obscurity_floor.min(100_000);

        let f = &mut self.filters;
        f.min_imdb_rating = f
            .min_imdb_rating
            .filter(|r| r.is_finite())
            .map(|r| (r.clamp(0.0, 10.0) * 10.0).round() / 10.0);
        f.min_imdb_votes = f.min_imdb_votes.map(|v| v.min(10_000_000));
        f.year_from = f.year_from.map(|y| y.clamp(1870, 2100));
        f.year_to = f.year_to.map(|y| y.clamp(1870, 2100));
        if let (Some(from), Some(to)) = (f.year_from, f.year_to)
            && from > to
        {
            f.year_from = Some(to);
            f.year_to = Some(from);
        }
        f.max_runtime = f.max_runtime.filter(|&m| m > 0).map(|m| m.min(600));
        let mut seen = std::collections::HashSet::new();
        f.genres = f
            .genres
            .iter()
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty() && seen.insert(g.to_lowercase()))
            .collect();

        if let Some(w) = &mut self.window {
            w.width = w.width.max(MIN_WINDOW_WIDTH);
            w.height = w.height.max(MIN_WINDOW_HEIGHT);
        }
    }
}

/// Version 0 shorthand to version 1 layout.
fn migrate_v0(map: &mut Map<String, Value>) {
    // "services": ["Netflix", "Paramount+"]  ->  {"groups": ["netflix", "paramountplus"]}
    if let Some(Value::Array(names)) = map.get("services") {
        let groups: Vec<String> = names
            .iter()
            .filter_map(Value::as_str)
            .map(group_key)
            .filter(|k| !k.is_empty())
            .collect();
        map.insert("services".to_owned(), json!({ "groups": groups }));
    }
    // Top-level IMDb minimums move into "filters".
    for key in ["min_imdb_rating", "min_imdb_votes"] {
        if let Some(v) = map.remove(key) {
            let filters = map
                .entry("filters")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(filters) = filters {
                filters.entry(key).or_insert(v);
            }
        }
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_gives_defaults() {
        let (s, from) = Settings::from_json("{}").unwrap();
        assert_eq!(from, 0);
        assert_eq!(s, Settings::default());
        assert_eq!(s.version, SETTINGS_VERSION);
        assert!(s.include_free);
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.filters.min_imdb_rating, Some(6.0));
        assert_eq!(s.filters.min_imdb_votes, Some(1000));
        assert_eq!(s.obscurity_floor, 50);
    }

    #[test]
    fn migrates_version_0_shorthand() {
        let text = r#"{
            "region": "au",
            "services": ["Netflix", "Paramount+", "Stan"],
            "min_imdb_rating": 7.0,
            "min_imdb_votes": null,
            "theme": "light"
        }"#;
        let (s, from) = Settings::from_json(text).unwrap();
        assert_eq!(from, 0);
        assert_eq!(s.version, 1);
        assert_eq!(s.region.as_deref(), Some("AU"));
        let groups: Vec<&str> = s.services.groups.iter().map(String::as_str).collect();
        assert_eq!(groups, ["netflix", "paramountplus", "stan"]);
        assert_eq!(s.filters.min_imdb_rating, Some(7.0));
        assert_eq!(s.filters.min_imdb_votes, None, "explicit null means Any");
        assert_eq!(s.theme, Theme::Light);
        assert!(s.include_free, "missing fields take defaults");
    }

    #[test]
    fn current_version_round_trips() {
        let mut s = Settings {
            region: Some("NZ".into()),
            window: Some(WindowGeometry {
                x: 10,
                y: 20,
                width: 1200,
                height: 800,
                maximized: false,
            }),
            ..Settings::default()
        };
        s.services.groups.insert("netflix".into());
        s.services.disabled_variants.insert(1796);
        s.filters.genres = vec!["Comedy".into()];
        let (back, from) = Settings::from_json(&s.to_json()).unwrap();
        assert_eq!(from, SETTINGS_VERSION);
        assert_eq!(back, s);
    }

    #[test]
    fn sanitises_out_of_range_values() {
        let text = r#"{
            "version": 1,
            "region": "Australia",
            "obscurity_floor": 999999999,
            "filters": {
                "min_imdb_rating": 12.34,
                "year_from": 2030, "year_to": 1990,
                "max_runtime": 0,
                "genres": [" Drama ", "drama", "", "Comedy"]
            },
            "window": { "x": 0, "y": 0, "width": 300, "height": 200 }
        }"#;
        let (s, _) = Settings::from_json(text).unwrap();
        assert_eq!(s.region, None, "not a two-letter code");
        assert_eq!(s.obscurity_floor, 100_000);
        assert_eq!(s.filters.min_imdb_rating, Some(10.0));
        assert_eq!((s.filters.year_from, s.filters.year_to), (Some(1990), Some(2030)));
        assert_eq!(s.filters.max_runtime, None);
        assert_eq!(s.filters.genres, ["Drama", "Comedy"]);
        let w = s.window.unwrap();
        assert_eq!((w.width, w.height), (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT));
    }

    #[test]
    fn newer_versions_keep_known_fields() {
        let text = r#"{ "version": 7, "include_free": false, "some_future_field": [1, 2] }"#;
        let (s, from) = Settings::from_json(text).unwrap();
        assert_eq!(from, 7);
        assert!(!s.include_free);
        assert_eq!(s.version, SETTINGS_VERSION);
    }

    #[test]
    fn invalid_file_is_backed_up_and_defaults_used() {
        let dir = std::env::temp_dir().join(format!("randomovie-settings-{}", fastrand::u64(..)));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert_eq!(fs::read_to_string(dir.join("settings.json.bak")).unwrap(), "{ not json");

        let s = Settings {
            include_free: false,
            ..Settings::default()
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert!(!dir.join("settings.json.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}

//! Where RandoMovie keeps its files.
//!
//! Settings live in `%APPDATA%\RandoMovie`. The cache, images, IMDb data and logs live in
//! `%LOCALAPPDATA%\RandoMovie`. Setting `RANDOMOVIE_HOME` puts both under one folder, which
//! tests and acceptance runs use to stay away from real user data.

use std::env;
use std::io;
use std::path::PathBuf;

const APP_DIR: &str = "RandoMovie";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Roaming settings folder.
    pub config_dir: PathBuf,
    /// Local data folder: cache database, IMDb index, logs.
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn from_env() -> AppPaths {
        if let Some(home) = env::var_os("RANDOMOVIE_HOME") {
            let home = PathBuf::from(home);
            return AppPaths::under(home);
        }
        let profile = env::var_os("USERPROFILE").map(PathBuf::from);
        let roaming = env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| profile.as_ref().map(|p| p.join("AppData").join("Roaming")))
            .unwrap_or_else(env::temp_dir);
        let local = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| profile.as_ref().map(|p| p.join("AppData").join("Local")))
            .unwrap_or_else(env::temp_dir);
        AppPaths {
            config_dir: roaming.join(APP_DIR),
            data_dir: local.join(APP_DIR),
        }
    }

    /// Both folders under one root (tests, acceptance runs).
    pub fn under(root: PathBuf) -> AppPaths {
        AppPaths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join("cache.sqlite3")
    }

    pub fn imdb_dir(&self) -> PathBuf {
        self.data_dir.join("imdb")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Creates every folder the app writes to.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.imdb_dir())?;
        std::fs::create_dir_all(self.log_dir())?;
        Ok(())
    }
}

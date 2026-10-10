//! A small size-capped log file.
//!
//! The current log rotates to `randomovie.old.log` when it passes 512 KB, so the two files
//! together never exceed about 1 MB. Before `init` runs (tests, examples), logging is a no-op.
//! Never pass credentials to these macros.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::time::{format_utc, now_unix};

const MAX_LOG_BYTES: u64 = 512 * 1024;
const LOG_FILE: &str = "randomovie.log";
const OLD_LOG_FILE: &str = "randomovie.old.log";

struct LogFile {
    path: PathBuf,
    old_path: PathBuf,
    file: Option<File>,
    written: u64,
}

static LOG: OnceLock<Mutex<LogFile>> = OnceLock::new();

/// Opens the log in `dir` and installs a panic hook that records panics there.
pub fn init(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(LOG_FILE);
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let written = file.metadata().map(|m| m.len()).unwrap_or(0);
    let log = LogFile {
        old_path: dir.join(OLD_LOG_FILE),
        path,
        file: Some(file),
        written,
    };
    if LOG.set(Mutex::new(log)).is_ok() {
        install_panic_hook();
    }
    Ok(())
}

/// Path of the current log file, if logging was initialised.
pub fn current_path() -> Option<PathBuf> {
    LOG.get()
        .and_then(|l| l.lock().ok().map(|l| l.path.clone()))
}

pub fn write(level: &str, args: fmt::Arguments<'_>) {
    let Some(log) = LOG.get() else { return };
    let Ok(mut log) = log.lock() else { return };
    let line = format!("{} {level:<5} {args}\n", format_utc(now_unix()));
    if log.written + line.len() as u64 > MAX_LOG_BYTES {
        log.rotate();
    }
    let len = line.len() as u64;
    if let Some(file) = log.file.as_mut()
        && file.write_all(line.as_bytes()).is_ok()
    {
        log.written += len;
    }
}

impl LogFile {
    fn rotate(&mut self) {
        self.file = None;
        let _ = fs::rename(&self.path, &self.old_path);
        self.file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .ok();
        self.written = 0;
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write("PANIC", format_args!("{info}"));
        default_hook(info);
    }));
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::util::log::write("ERROR", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::util::log::write("WARN", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::util::log::write("INFO", format_args!($($arg)*)) };
}

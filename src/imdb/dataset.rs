//! Keeps the local ratings index current and answers lookups.
//!
//! The dataset is downloaded on first run and re-checked once the last check is over a week
//! old, with `If-None-Match` / `If-Modified-Since` so an unchanged file costs one small
//! request. A new file streams through gzip decompression into the index builder while it
//! downloads; the finished index replaces the old one under the lock in one step.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use flate2::write::MultiGzDecoder;
use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use serde::{Deserialize, Serialize};

use super::index::{IndexBuilder, IndexReader, Rating, parse_tconst};
use crate::error::ApiError;
use crate::util::lru::Lru;
use crate::util::time::{DAY, now_unix, parse_http_date};
use crate::{log_info, log_warn};

pub const DATASET_URL: &str = "https://datasets.imdbws.com/title.ratings.tsv.gz";
pub const REFRESH_AFTER_SECS: i64 = 7 * DAY;

const INDEX_FILE: &str = "ratings.idx";
const NEW_INDEX_FILE: &str = "ratings.idx.new";
const META_FILE: &str = "ratings.meta.json";
const BUILD_DIR: &str = "build";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// The real dataset has about 1.6 million rows; far fewer means a broken download.
const MIN_RECORDS: u64 = 100_000;
const LOOKUP_CACHE_ENTRIES: usize = 512;

/// What we know about the local copy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DatasetMeta {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Last time the server was asked whether the dataset changed (unix seconds).
    pub checked_at: i64,
    /// When the local index was built (unix seconds).
    pub built_at: i64,
    pub records: u64,
}

impl DatasetMeta {
    /// When IMDb published the data, falling back to when the index was built.
    pub fn data_date(&self) -> Option<i64> {
        self.last_modified
            .as_deref()
            .and_then(parse_http_date)
            .or((self.built_at > 0).then_some(self.built_at))
    }
}

/// The answer for one title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImdbStatus {
    Rated(Rating),
    /// No IMDb id, or IMDb has no rating for it.
    Unavailable,
    /// The ratings index isn't ready yet.
    Loading,
}

#[derive(Debug)]
pub enum ImdbError {
    Network(ApiError),
    Io(io::Error),
    BadData(String),
    /// Another refresh is already running.
    Busy,
}

impl fmt::Display for ImdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImdbError::Network(e) => write!(f, "IMDb download failed: {e}"),
            ImdbError::Io(e) => write!(f, "IMDb data couldn't be saved: {e}"),
            ImdbError::BadData(detail) => write!(f, "IMDb data looked wrong: {detail}"),
            ImdbError::Busy => write!(f, "IMDb ratings are already being updated"),
        }
    }
}

impl std::error::Error for ImdbError {}

impl From<io::Error> for ImdbError {
    fn from(e: io::Error) -> Self {
        ImdbError::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    Updated { records: u64 },
    NotModified,
}

pub struct ImdbRatings {
    dir: PathBuf,
    state: Mutex<State>,
    refreshing: AtomicBool,
}

struct State {
    reader: Option<IndexReader>,
    meta: DatasetMeta,
    cache: Lru<u32, Option<Rating>>,
}

impl ImdbRatings {
    /// Opens the local index if there is a valid one. Never fails: without an index every
    /// lookup answers `Loading` until `refresh` succeeds.
    pub fn open(dir: &Path) -> ImdbRatings {
        let _ = fs::create_dir_all(dir);
        let reader = match IndexReader::open(&dir.join(INDEX_FILE)) {
            Ok(reader) => Some(reader),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                log_warn!("IMDb index unusable, it will be rebuilt: {e}");
                None
            }
        };
        ImdbRatings {
            dir: dir.to_owned(),
            state: Mutex::new(State {
                reader,
                meta: load_meta(dir),
                cache: Lru::new(LOOKUP_CACHE_ENTRIES),
            }),
            refreshing: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_ready(&self) -> bool {
        self.lock().reader.is_some()
    }

    pub fn is_refreshing(&self) -> bool {
        self.refreshing.load(Ordering::SeqCst)
    }

    pub fn meta(&self) -> DatasetMeta {
        self.lock().meta.clone()
    }

    /// True when there is no index or the last check is over 7 days old.
    pub fn needs_refresh(&self, now: i64) -> bool {
        let state = self.lock();
        let age = now - state.meta.checked_at;
        state.reader.is_none() || !(0..REFRESH_AFTER_SECS).contains(&age)
    }

    /// Looks up a rating by IMDb id such as `tt0133093`.
    pub fn lookup(&self, imdb_id: Option<&str>) -> ImdbStatus {
        let mut state = self.lock();
        if state.reader.is_none() {
            return ImdbStatus::Loading;
        }
        let Some(id) = imdb_id.and_then(parse_tconst) else {
            return ImdbStatus::Unavailable;
        };
        if let Some(cached) = state.cache.get(&id) {
            return cached.map_or(ImdbStatus::Unavailable, ImdbStatus::Rated);
        }
        let found = match state.reader.as_ref().map(|r| r.get(id)) {
            Some(Ok(found)) => found,
            Some(Err(e)) => {
                log_warn!("IMDb lookup failed: {e}");
                return ImdbStatus::Unavailable;
            }
            None => return ImdbStatus::Loading,
        };
        state.cache.insert(id, found);
        found.map_or(ImdbStatus::Unavailable, ImdbStatus::Rated)
    }

    /// Downloads the dataset if it changed (or unconditionally with `force`) and swaps in
    /// the new index.
    pub async fn refresh(
        &self,
        http: &reqwest::Client,
        force: bool,
    ) -> Result<RefreshOutcome, ImdbError> {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return Err(ImdbError::Busy);
        }
        let _guard = ResetOnDrop(&self.refreshing);
        let result = self.download_and_build(http, force).await;
        let _ = fs::remove_dir_all(self.dir.join(BUILD_DIR));
        let _ = fs::remove_file(self.dir.join(NEW_INDEX_FILE));
        match &result {
            Ok(RefreshOutcome::Updated { records }) => {
                log_info!("IMDb ratings updated: {records} titles");
            }
            Ok(RefreshOutcome::NotModified) => log_info!("IMDb ratings unchanged"),
            Err(e) => log_warn!("{e}"),
        }
        result
    }

    async fn download_and_build(
        &self,
        http: &reqwest::Client,
        force: bool,
    ) -> Result<RefreshOutcome, ImdbError> {
        let (meta, have_index) = {
            let state = self.lock();
            (state.meta.clone(), state.reader.is_some())
        };
        let mut request = http.get(DATASET_URL).timeout(DOWNLOAD_TIMEOUT);
        if have_index && !force {
            if let Some(etag) = &meta.etag {
                request = request.header(IF_NONE_MATCH, etag.as_str());
            }
            if let Some(modified) = &meta.last_modified {
                request = request.header(IF_MODIFIED_SINCE, modified.as_str());
            }
        }
        let mut response = request.send().await.map_err(network_error)?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED {
            let mut state = self.lock();
            state.meta.checked_at = now_unix();
            save_meta(&self.dir, &state.meta)?;
            return Ok(RefreshOutcome::NotModified);
        }
        if !status.is_success() {
            return Err(ImdbError::Network(ApiError::Status(status.as_u16())));
        }
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let etag = header(ETAG);
        let last_modified = header(LAST_MODIFIED);

        let builder = IndexBuilder::new(&self.dir.join(BUILD_DIR))?;
        let mut sink = Decompressor::new(builder);
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            sink.write_all(&chunk)?;
        }
        let builder = sink.finish()?;
        let skipped = builder.skipped_rows();
        let new_index = self.dir.join(NEW_INDEX_FILE);
        let records = builder.finish(&new_index)?;
        if records < MIN_RECORDS {
            return Err(ImdbError::BadData(format!("only {records} ratings were readable")));
        }
        if skipped > 0 {
            log_warn!("skipped {skipped} malformed IMDb rows");
        }
        let now = now_unix();
        self.install(
            &new_index,
            DatasetMeta {
                etag,
                last_modified,
                checked_at: now,
                built_at: now,
                records,
            },
        )?;
        Ok(RefreshOutcome::Updated { records })
    }

    /// Replaces the current index with `new_index` in one step: lookups see either the old
    /// or the new file, never a mix. The old handle is closed first so the rename can't be
    /// blocked by it.
    pub fn install(&self, new_index: &Path, meta: DatasetMeta) -> io::Result<()> {
        IndexReader::open(new_index)?;
        let final_path = self.dir.join(INDEX_FILE);
        let mut state = self.lock();
        state.reader = None;
        if let Err(e) = fs::rename(new_index, &final_path) {
            state.reader = IndexReader::open(&final_path).ok();
            return Err(e);
        }
        state.reader = Some(IndexReader::open(&final_path)?);
        state.cache.clear();
        state.meta = meta;
        save_meta(&self.dir, &state.meta)
    }
}

struct ResetOnDrop<'a>(&'a AtomicBool);

impl Drop for ResetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn network_error(e: reqwest::Error) -> ImdbError {
    let error = if e.is_timeout() {
        ApiError::Timeout
    } else {
        ApiError::Offline
    };
    ImdbError::Network(error)
}

fn load_meta(dir: &Path) -> DatasetMeta {
    fs::read_to_string(dir.join(META_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_meta(dir: &Path, meta: &DatasetMeta) -> io::Result<()> {
    let path = dir.join(META_FILE);
    let tmp = dir.join(format!("{META_FILE}.tmp"));
    let json = serde_json::to_string_pretty(meta).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// Feeds the index builder, gunzipping when the bytes start with the gzip magic number.
/// The dataset is a `.gz` file, but if a proxy ever served it with `Content-Encoding: gzip`
/// reqwest would already have decompressed it.
enum Decompressor {
    Sniffing(Vec<u8>, IndexBuilder),
    Gzip(Box<MultiGzDecoder<IndexBuilder>>),
    Plain(IndexBuilder),
    Failed,
}

impl Decompressor {
    fn new(builder: IndexBuilder) -> Decompressor {
        Decompressor::Sniffing(Vec::new(), builder)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Decompressor::Gzip(decoder) => decoder.write_all(data),
            Decompressor::Plain(builder) => builder.write_all(data),
            Decompressor::Sniffing(head, _) if head.len() + data.len() < 2 => {
                head.extend_from_slice(data);
                Ok(())
            }
            Decompressor::Sniffing(..) => {
                let Decompressor::Sniffing(mut head, builder) =
                    std::mem::replace(self, Decompressor::Failed)
                else {
                    unreachable!("matched Sniffing above");
                };
                head.extend_from_slice(data);
                *self = if head.starts_with(&[0x1f, 0x8b]) {
                    Decompressor::Gzip(Box::new(MultiGzDecoder::new(builder)))
                } else {
                    Decompressor::Plain(builder)
                };
                self.write_all(&head)
            }
            Decompressor::Failed => Err(io::Error::other("decompressor already failed")),
        }
    }

    fn finish(self) -> io::Result<IndexBuilder> {
        match self {
            Decompressor::Gzip(decoder) => decoder.finish(),
            Decompressor::Plain(builder) => Ok(builder),
            Decompressor::Sniffing(head, mut builder) => {
                builder.write_all(&head)?;
                Ok(builder)
            }
            Decompressor::Failed => Err(io::Error::other("decompressor already failed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("randomovie-{name}-{}", fastrand::u64(..)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const ROWS: &str = "tconst\taverageRating\tnumVotes\n\
        tt0133093\t8.7\t2200000\n\
        tt10872600\t8.2\t900000\n\
        tt7678620\t9.3\t21000\n";

    fn build_through(dir: &Path, data: &[u8], piece: usize) -> PathBuf {
        let mut sink = Decompressor::new(IndexBuilder::new(&dir.join(BUILD_DIR)).unwrap());
        for chunk in data.chunks(piece) {
            sink.write_all(chunk).unwrap();
        }
        let out = dir.join(NEW_INDEX_FILE);
        assert_eq!(sink.finish().unwrap().finish(&out).unwrap(), 3);
        out
    }

    #[test]
    fn gzip_and_plain_input_build_the_same_index() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(ROWS.as_bytes()).unwrap();
        let gz = encoder.finish().unwrap();
        for (data, piece) in [(gz.as_slice(), 1), (gz.as_slice(), 1000), (ROWS.as_bytes(), 1)] {
            let dir = temp_dir("imdb-build");
            let out = build_through(&dir, data, piece);
            let reader = IndexReader::open(&out).unwrap();
            assert_eq!(reader.get(7_678_620).unwrap().unwrap().rating_x10, 93);
            drop(reader);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn lookups_report_loading_then_ratings_after_install() {
        let dir = temp_dir("imdb-install");
        let ratings = ImdbRatings::open(&dir);
        assert!(!ratings.is_ready());
        assert!(ratings.needs_refresh(now_unix()));
        assert_eq!(ratings.lookup(Some("tt0133093")), ImdbStatus::Loading);

        let new_index = build_through(&dir, ROWS.as_bytes(), 64);
        let meta = DatasetMeta {
            etag: Some("\"abc\"".into()),
            last_modified: Some("Fri, 09 Oct 2026 12:51:19 GMT".into()),
            checked_at: now_unix(),
            built_at: now_unix(),
            records: 3,
        };
        ratings.install(&new_index, meta.clone()).unwrap();
        assert!(ratings.is_ready());
        assert!(!ratings.needs_refresh(now_unix()));
        assert!(ratings.needs_refresh(now_unix() + REFRESH_AFTER_SECS));
        assert_eq!(
            ratings.lookup(Some("tt0133093")),
            ImdbStatus::Rated(Rating {
                rating_x10: 87,
                votes: 2_200_000
            })
        );
        // Cached answers come back the same, including misses.
        assert_eq!(ratings.lookup(Some("tt0133093")), ratings.lookup(Some("tt0133093")));
        assert_eq!(ratings.lookup(Some("tt0000002")), ImdbStatus::Unavailable);
        assert_eq!(ratings.lookup(None), ImdbStatus::Unavailable);
        assert_eq!(ratings.lookup(Some("garbage")), ImdbStatus::Unavailable);
        assert_eq!(
            crate::util::time::format_date(ratings.meta().data_date().unwrap()),
            "2026-10-09"
        );

        // A second install replaces the open index in place.
        let replacement = build_through(&dir, ROWS.replace("8.7", "8.8").as_bytes(), 7);
        ratings.install(&replacement, meta).unwrap();
        assert_eq!(
            ratings.lookup(Some("tt0133093")),
            ImdbStatus::Rated(Rating {
                rating_x10: 88,
                votes: 2_200_000
            })
        );

        // Reopening finds the installed index and its metadata.
        drop(ratings);
        let reopened = ImdbRatings::open(&dir);
        assert!(reopened.is_ready());
        assert_eq!(reopened.meta().etag.as_deref(), Some("\"abc\""));
        drop(reopened);
        fs::remove_dir_all(dir).unwrap();
    }
}

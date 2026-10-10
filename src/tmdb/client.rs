//! The one shared HTTP client.
//!
//! - TMDB calls go through the SQLite cache with a lifetime per endpoint.
//! - At most six requests are in flight; identical concurrent requests share one fetch.
//! - 429, 5xx and timeouts are retried with exponential backoff (honouring `Retry-After`).
//! - Connection failures flip the online flag, which the UI shows as an offline banner, and
//!   browsing calls fall back to expired cache entries.
//! - The credential is sent only to `api.themoviedb.org`, never to the image CDN, and never
//!   appears in cache keys, errors or logs.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use reqwest::{StatusCode, Url};
use tokio::sync::{OnceCell, Semaphore, watch};

use super::images::IMAGE_BASE;
use super::model::{
    Genre, GenreList, Page, ProviderEntry, ProviderList, Region, RegionList, TitleDetails,
    parse_details,
};
use super::query::DiscoverQuery;
use crate::error::ApiError;
use crate::log_warn;
use crate::media::{MediaType, TitleRef};
use crate::store::Store;
use crate::util::time::now_unix;

pub const API_BASE: &str = "https://api.themoviedb.org/3";
pub const LANGUAGE: &str = "en-US";
/// Images older than 30 days are refetched; the image table is capped at 150 MB.
pub const IMAGE_CACHE_CAP_BYTES: u64 = 150 * 1024 * 1024;

const MAX_CONCURRENT_REQUESTS: usize = 6;
const MAX_ATTEMPTS: u32 = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

/// How long each kind of response stays fresh.
pub mod ttl {
    use std::time::Duration;

    const DAY: u64 = 24 * 60 * 60;
    pub const PROVIDERS: Duration = Duration::from_secs(7 * DAY);
    pub const REGIONS: Duration = Duration::from_secs(7 * DAY);
    pub const GENRES: Duration = Duration::from_secs(7 * DAY);
    pub const DETAILS: Duration = Duration::from_secs(DAY);
    pub const DISCOVER: Duration = Duration::from_secs(DAY);
    pub const SEARCH: Duration = Duration::from_secs(DAY);
    pub const IMAGES: Duration = Duration::from_secs(30 * DAY);
}

/// A TMDB credential: the v4 read access token (preferred) or a v3 API key.
#[derive(Clone)]
pub enum Credential {
    /// Sent as `Authorization: Bearer …`.
    Bearer(String),
    /// Sent as the `api_key` query parameter.
    ApiKey(String),
}

impl Credential {
    /// Accepts either value from TMDB's API settings page: a 32-character hex v3 key or a
    /// v4 read access token.
    pub fn from_secret(secret: &str) -> Option<Credential> {
        let secret = secret.trim();
        if secret.is_empty() {
            None
        } else if secret.len() == 32 && secret.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(Credential::ApiKey(secret.to_owned()))
        } else {
            Some(Credential::Bearer(secret.to_owned()))
        }
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Credential::Bearer(_) => "Credential::Bearer(<redacted>)",
            Credential::ApiKey(_) => "Credential::ApiKey(<redacted>)",
        })
    }
}

/// Whether a call may fall back to expired cached data when the network fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Only data within its lifetime. Used to verify picks.
    Fresh,
    /// Expired data is acceptable when offline. Used for browsing.
    AllowStale,
}

type Body = Arc<[u8]>;
type SharedFetch = Arc<OnceCell<Result<Body, ApiError>>>;

enum Failure {
    /// Worth retrying, optionally after a server-specified delay.
    Retry(ApiError, Option<Duration>),
    Fatal(ApiError),
}

pub struct TmdbClient {
    http: reqwest::Client,
    credential: Credential,
    store: Arc<Store>,
    limiter: Semaphore,
    inflight: Mutex<HashMap<String, SharedFetch>>,
    online: watch::Sender<bool>,
}

impl TmdbClient {
    pub fn new(credential: Credential, store: Arc<Store>) -> Result<TmdbClient, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("RandoMovie/", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            .read_timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ApiError::Decode(format!("HTTP client setup failed: {}", e.without_url())))?;
        Ok(TmdbClient {
            http,
            credential,
            store,
            limiter: Semaphore::new(MAX_CONCURRENT_REQUESTS),
            inflight: Mutex::new(HashMap::new()),
            online: watch::channel(true).0,
        })
    }

    /// The shared HTTP client, also used for the IMDb dataset download.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn is_online(&self) -> bool {
        *self.online.borrow()
    }

    /// Notifies on every online/offline change.
    pub fn subscribe_online(&self) -> watch::Receiver<bool> {
        self.online.subscribe()
    }

    // ---- Endpoints -------------------------------------------------------------------

    /// Checks the credential with `/authentication`, bypassing the cache.
    pub async fn validate(&self) -> Result<(), ApiError> {
        let url = self.api_url("/authentication", &[])?;
        self.fetch(url, true).await.map(|_| ())
    }

    /// Countries with streaming data, sorted by English name.
    pub async fn regions(&self) -> Result<Vec<Region>, ApiError> {
        let params = [("language", LANGUAGE.to_owned())];
        let (body, _) = self
            .get_api("/watch/providers/regions", &params, ttl::REGIONS, Freshness::AllowStale)
            .await?;
        let mut regions = serde_json::from_slice::<RegionList>(&body)?.results;
        regions.sort_by(|a, b| a.english_name.cmp(&b.english_name));
        Ok(regions)
    }

    pub async fn providers(&self, media: MediaType, region: &str) -> Result<Vec<ProviderEntry>, ApiError> {
        let path = format!("/watch/providers/{}", media.as_str());
        let params = [("language", LANGUAGE.to_owned()), ("watch_region", region.to_owned())];
        let (body, _) = self
            .get_api(&path, &params, ttl::PROVIDERS, Freshness::AllowStale)
            .await?;
        Ok(serde_json::from_slice::<ProviderList>(&body)?.results)
    }

    pub async fn genres(&self, media: MediaType) -> Result<Vec<Genre>, ApiError> {
        let path = format!("/genre/{}/list", media.as_str());
        let params = [("language", LANGUAGE.to_owned())];
        let (body, _) = self
            .get_api(&path, &params, ttl::GENRES, Freshness::AllowStale)
            .await?;
        Ok(serde_json::from_slice::<GenreList>(&body)?.genres)
    }

    pub async fn discover(&self, query: &DiscoverQuery, page: u32) -> Result<Page, ApiError> {
        let mut params: Vec<(&str, String)> =
            query.params().iter().map(|(k, v)| (*k, v.clone())).collect();
        params.push(("page", page.to_string()));
        let (body, _) = self
            .get_api(&query.path(), &params, ttl::DISCOVER, Freshness::AllowStale)
            .await?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// Details, availability in `region` and the IMDb id in one call.
    pub async fn details(
        &self,
        title: TitleRef,
        region: &str,
        freshness: Freshness,
    ) -> Result<TitleDetails, ApiError> {
        let path = format!("/{}/{}", title.media.as_str(), title.id);
        let params = [
            ("append_to_response", "watch/providers,external_ids".to_owned()),
            ("language", LANGUAGE.to_owned()),
        ];
        let (body, fetched_at) = self.get_api(&path, &params, ttl::DETAILS, freshness).await?;
        parse_details(title.media, &body, region, fetched_at)
    }

    /// Multi search, keeping only movies and series.
    pub async fn search(&self, query: &str, page: u32) -> Result<Page, ApiError> {
        let params = [
            ("include_adult", "false".to_owned()),
            ("language", LANGUAGE.to_owned()),
            ("page", page.to_string()),
            ("query", query.trim().to_owned()),
        ];
        let (body, _) = self
            .get_api("/search/multi", &params, ttl::SEARCH, Freshness::AllowStale)
            .await?;
        let mut page: Page = serde_json::from_slice(&body)?;
        page.results.retain(|item| item.media().is_some());
        Ok(page)
    }

    /// Image bytes for a TMDB image path at one of TMDB's sizes (see `images::size_for`).
    pub async fn image(&self, size: &str, path: &str) -> Result<Body, ApiError> {
        let key = format!("{size}{path}");
        let max_age = ttl::IMAGES.as_secs() as i64;
        if let Some(body) = self.store.image_get(&key, now_unix(), max_age)? {
            return Ok(Arc::from(body));
        }
        let url = Url::parse(&format!("{IMAGE_BASE}{key}"))
            .map_err(|e| ApiError::Decode(format!("bad image path: {e}")))?;
        let body = self.fetch_shared(&format!("image:{key}"), url, false).await?;
        if let Err(e) = self
            .store
            .image_put(&key, &body, now_unix(), IMAGE_CACHE_CAP_BYTES)
        {
            log_warn!("image cache write failed: {e}");
        }
        Ok(body)
    }

    // ---- Core ------------------------------------------------------------------------

    /// GET a TMDB path through the cache. Returns the body and when it was fetched.
    async fn get_api(
        &self,
        path: &str,
        params: &[(&str, String)],
        ttl: Duration,
        freshness: Freshness,
    ) -> Result<(Body, i64), ApiError> {
        let key = cache_key(path, params);
        let cached = self.store.http_get(&key)?;
        if let Some(entry) = &cached
            && entry.is_fresh(now_unix(), ttl)
        {
            return Ok((Arc::from(entry.body.as_slice()), entry.fetched_at));
        }
        let url = self.api_url(path, params)?;
        match self.fetch_shared(&key, url, true).await {
            Ok(body) => {
                let fetched_at = now_unix();
                if let Err(e) = self.store.http_put(&key, &body, fetched_at) {
                    log_warn!("cache write failed for {path}: {e}");
                }
                Ok((body, fetched_at))
            }
            Err(e) if freshness == Freshness::AllowStale && e.is_connectivity() => match cached {
                Some(entry) => Ok((Arc::from(entry.body), entry.fetched_at)),
                None => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    fn api_url(&self, path: &str, params: &[(&str, String)]) -> Result<Url, ApiError> {
        let mut url = Url::parse(&format!("{API_BASE}{path}"))
            .map_err(|e| ApiError::Decode(format!("bad API path: {e}")))?;
        let api_key = match &self.credential {
            Credential::ApiKey(key) => Some(key.as_str()),
            Credential::Bearer(_) => None,
        };
        if !params.is_empty() || api_key.is_some() {
            let mut query = url.query_pairs_mut();
            for (k, v) in params {
                query.append_pair(k, v);
            }
            if let Some(key) = api_key {
                query.append_pair("api_key", key);
            }
        }
        Ok(url)
    }

    /// Fetches `url`, sharing the result with any identical request already in flight.
    async fn fetch_shared(&self, key: &str, url: Url, authorized: bool) -> Result<Body, ApiError> {
        let cell: SharedFetch = {
            let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            inflight.entry(key.to_owned()).or_default().clone()
        };
        let result = cell.get_or_init(|| self.fetch(url, authorized)).await.clone();
        let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if inflight.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            inflight.remove(key);
        }
        result
    }

    /// One logical request: limited to six at a time, retried with exponential backoff.
    async fn fetch(&self, url: Url, authorized: bool) -> Result<Body, ApiError> {
        let mut attempt = 1;
        loop {
            let outcome = {
                let _permit = self
                    .limiter
                    .acquire()
                    .await
                    .map_err(|_| ApiError::Offline)?;
                self.send_once(&url, authorized).await
            };
            match outcome {
                Ok(body) => return Ok(body),
                Err(Failure::Retry(error, wait)) if attempt < MAX_ATTEMPTS => {
                    let wait = wait.unwrap_or_else(|| backoff(attempt));
                    log_warn!("{} {}: {error}; retry {attempt} in {wait:?}", url.host_str().unwrap_or("?"), url.path());
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                Err(Failure::Retry(error, _) | Failure::Fatal(error)) => {
                    if error == ApiError::Offline {
                        self.set_online(false);
                    }
                    return Err(error);
                }
            }
        }
    }

    async fn send_once(&self, url: &Url, authorized: bool) -> Result<Body, Failure> {
        let mut request = self.http.get(url.clone());
        if authorized && let Credential::Bearer(token) = &self.credential {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(classify)?;
        // Any HTTP answer means the network works.
        self.set_online(true);
        let status = response.status();
        if status.is_success() {
            let body = response.bytes().await.map_err(classify)?;
            return Ok(Arc::from(&body[..]));
        }
        Err(match status {
            StatusCode::UNAUTHORIZED => Failure::Fatal(ApiError::Unauthorized),
            StatusCode::NOT_FOUND => Failure::Fatal(ApiError::NotFound),
            StatusCode::TOO_MANY_REQUESTS => {
                Failure::Retry(ApiError::RateLimited, retry_after(&response))
            }
            s if s.is_server_error() => Failure::Retry(ApiError::Status(s.as_u16()), None),
            s => Failure::Fatal(ApiError::Status(s.as_u16())),
        })
    }

    fn set_online(&self, online: bool) {
        self.online.send_if_modified(|current| {
            let changed = *current != online;
            *current = online;
            changed
        });
    }
}

/// Cache key: path plus parameters in call order. Credentials are never part of it.
fn cache_key(path: &str, params: &[(&str, String)]) -> String {
    let mut key = path.to_owned();
    for (i, (k, v)) in params.iter().enumerate() {
        key.push(if i == 0 { '?' } else { '&' });
        key.push_str(k);
        key.push('=');
        key.push_str(v);
    }
    key
}

/// Maps transport errors without keeping the URL (which may hold a v3 key).
fn classify(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        Failure::Retry(ApiError::Timeout, None)
    } else if error.is_connect() {
        Failure::Fatal(ApiError::Offline)
    } else {
        Failure::Retry(ApiError::Offline, None)
    }
}

/// 0.5 s, 1 s, 2 s, ... plus up to 250 ms of jitter.
fn backoff(attempt: u32) -> Duration {
    let base = 500u64 << attempt.saturating_sub(1).min(5);
    Duration::from_millis(base + fastrand::u64(0..250))
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let seconds: u64 = response
        .headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_kind_is_detected_and_redacted() {
        let key = Credential::from_secret(" 0123456789abcdef0123456789ABCDEF ").unwrap();
        assert!(matches!(key, Credential::ApiKey(_)));
        let token = Credential::from_secret("eyJhbGciOiJIUzI1NiJ9.e30.sig").unwrap();
        assert!(matches!(token, Credential::Bearer(_)));
        assert!(Credential::from_secret("   ").is_none());
        assert_eq!(format!("{token:?}"), "Credential::Bearer(<redacted>)");
    }

    #[test]
    fn cache_keys_exclude_credentials() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let client =
            TmdbClient::new(Credential::ApiKey("0123456789abcdef0123456789abcdef".into()), store).unwrap();
        let params = [("language", "en-US".to_owned())];
        let url = client.api_url("/genre/movie/list", &params).unwrap();
        assert!(url.as_str().contains("api_key="), "v3 keys travel in the URL");
        let key = cache_key("/genre/movie/list", &params);
        assert_eq!(key, "/genre/movie/list?language=en-US");
        assert!(!key.contains("api_key"));
    }

    #[test]
    fn backoff_grows_exponentially() {
        let ms = |attempt| backoff(attempt).as_millis();
        assert!((500..750).contains(&ms(1)));
        assert!((1000..1250).contains(&ms(2)));
        assert!((2000..2250).contains(&ms(3)));
    }
}

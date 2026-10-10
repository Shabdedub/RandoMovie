//! Error types shared by the TMDB client, the IMDb download and the picker.
//!
//! Messages never include request URLs, so a v3 API key passed as a query parameter
//! can't leak into logs or the UI through an error.

use std::fmt;

/// A failed HTTP call. Cloneable so that de-duplicated requests can share one result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// No connection (DNS, connect or TLS failure) and no usable cached copy.
    Offline,
    /// The request kept timing out after retries.
    Timeout,
    /// TMDB rejected the token (HTTP 401).
    Unauthorized,
    /// The resource doesn't exist (HTTP 404).
    NotFound,
    /// Still rate-limited (HTTP 429) after backing off.
    RateLimited,
    /// Any other unsuccessful HTTP status, after retries where they apply.
    Status(u16),
    /// The response wasn't the JSON we expected.
    Decode(String),
    /// The local cache failed.
    Storage(String),
}

impl ApiError {
    /// True when the failure means "we couldn't reach the server", as opposed to the server
    /// answering with an error. Callers may fall back to stale cached data in that case.
    pub fn is_connectivity(&self) -> bool {
        matches!(
            self,
            ApiError::Offline | ApiError::Timeout | ApiError::RateLimited | ApiError::Status(500..=599)
        )
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Offline => write!(f, "You're offline"),
            ApiError::Timeout => write!(f, "The server took too long to answer"),
            ApiError::Unauthorized => write!(f, "TMDB didn't accept the API token"),
            ApiError::NotFound => write!(f, "Not found"),
            ApiError::RateLimited => write!(f, "TMDB is limiting requests; try again shortly"),
            ApiError::Status(code) => write!(f, "The server answered with HTTP {code}"),
            ApiError::Decode(detail) => write!(f, "Unexpected response: {detail}"),
            ApiError::Storage(detail) => write!(f, "Local cache error: {detail}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::Storage(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::Decode(e.to_string())
    }
}

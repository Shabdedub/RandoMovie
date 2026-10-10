//! Small shared types naming a title and the kind of media.

use serde::{Deserialize, Serialize};

/// TMDB's two media types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaType {
    Movie,
    Tv,
}

impl MediaType {
    /// The path segment TMDB uses: `movie` or `tv`.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::Movie => "movie",
            MediaType::Tv => "tv",
        }
    }

    pub fn parse(s: &str) -> Option<MediaType> {
        match s {
            "movie" => Some(MediaType::Movie),
            "tv" => Some(MediaType::Tv),
            _ => None,
        }
    }

    /// Label for badges: "Movie" or "Series".
    pub fn label(self) -> &'static str {
        match self {
            MediaType::Movie => "Movie",
            MediaType::Tv => "Series",
        }
    }
}

/// A title on TMDB: media type plus id (movie and TV ids overlap, so both are needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TitleRef {
    pub media: MediaType,
    pub id: u32,
}

impl TitleRef {
    pub fn new(media: MediaType, id: u32) -> TitleRef {
        TitleRef { media, id }
    }
}

/// The Movies / Series / Both toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaChoice {
    Movies,
    Series,
    #[default]
    Both,
}

impl MediaChoice {
    /// The media types this choice covers, movies first.
    pub fn types(self) -> &'static [MediaType] {
        match self {
            MediaChoice::Movies => &[MediaType::Movie],
            MediaChoice::Series => &[MediaType::Tv],
            MediaChoice::Both => &[MediaType::Movie, MediaType::Tv],
        }
    }
}

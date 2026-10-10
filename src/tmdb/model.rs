//! TMDB response types. Only the fields the app reads are declared; serde skips the rest.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::error::ApiError;
use crate::media::{MediaType, TitleRef};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Genre {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GenreList {
    pub genres: Vec<Genre>,
}

/// A country from `/watch/providers/regions`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Region {
    #[serde(rename = "iso_3166_1")]
    pub code: String,
    pub english_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegionList {
    pub results: Vec<Region>,
}

/// A provider from `/watch/providers/{movie,tv}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProviderEntry {
    pub provider_id: u32,
    pub provider_name: String,
    pub logo_path: Option<String>,
    /// Display priority per region code; lower is more prominent.
    #[serde(default)]
    pub display_priorities: HashMap<String, i32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProviderList {
    pub results: Vec<ProviderEntry>,
}

/// One way to watch a title, from `watch/providers.results[region]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Offer {
    pub provider_id: u32,
    pub provider_name: String,
    pub logo_path: Option<String>,
    #[serde(default)]
    pub display_priority: i32,
}

/// Where a title can be watched in one region.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RegionAvailability {
    /// TMDB's "where to watch" page for the title in this region.
    pub link: Option<String>,
    #[serde(default)]
    pub flatrate: Vec<Offer>,
    #[serde(default)]
    pub free: Vec<Offer>,
    #[serde(default)]
    pub ads: Vec<Offer>,
    #[serde(default)]
    pub rent: Vec<Offer>,
    #[serde(default)]
    pub buy: Vec<Offer>,
}

impl RegionAvailability {
    pub fn has_offers(&self) -> bool {
        !(self.flatrate.is_empty()
            && self.free.is_empty()
            && self.ads.is_empty()
            && self.rent.is_empty()
            && self.buy.is_empty())
    }
}

/// A page of discover or search results.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Page {
    pub page: u32,
    pub results: Vec<ListItem>,
    pub total_pages: u32,
    pub total_results: u32,
}

/// A movie, series or person in a result list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ListItem {
    pub id: u32,
    /// Present in multi-search results: `movie`, `tv` or `person`.
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub first_air_date: Option<String>,
    #[serde(default)]
    pub poster_path: Option<String>,
}

impl ListItem {
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or("Untitled")
    }

    pub fn year(&self) -> Option<u16> {
        year_of(self.release_date.as_deref().or(self.first_air_date.as_deref()))
    }

    pub fn media(&self) -> Option<MediaType> {
        self.media_type.as_deref().and_then(MediaType::parse)
    }
}

/// Year from a TMDB date such as `1999-03-31`; empty dates give `None`.
pub fn year_of(date: Option<&str>) -> Option<u16> {
    date.and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok())
        .filter(|&y| y > 0)
}

/// Details of one title for one region: metadata, availability and IMDb id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleDetails {
    pub media: MediaType,
    pub id: u32,
    pub title: String,
    pub year: Option<u16>,
    /// Movies only.
    pub runtime_minutes: Option<u32>,
    /// Series only.
    pub seasons: Option<u32>,
    pub genres: Vec<String>,
    pub overview: String,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub imdb_id: Option<String>,
    pub availability: RegionAvailability,
    /// When the availability data was fetched from TMDB (unix seconds).
    pub fetched_at: i64,
}

impl TitleDetails {
    pub fn title_ref(&self) -> TitleRef {
        TitleRef::new(self.media, self.id)
    }
}

#[derive(Deserialize)]
struct RawDetails<'a> {
    id: u32,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    runtime: Option<u32>,
    #[serde(default)]
    number_of_seasons: Option<u32>,
    #[serde(default)]
    genres: Vec<Genre>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    backdrop_path: Option<String>,
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(default)]
    external_ids: Option<ExternalIds>,
    #[serde(rename = "watch/providers", borrow, default)]
    watch_providers: Option<WatchProviders<'a>>,
}

#[derive(Deserialize)]
struct ExternalIds {
    #[serde(default)]
    imdb_id: Option<String>,
}

/// Availability for every region, kept as raw JSON so only the user's region is parsed.
#[derive(Deserialize)]
struct WatchProviders<'a> {
    #[serde(borrow)]
    results: HashMap<&'a str, &'a RawValue>,
}

/// Parses `/movie/{id}` or `/tv/{id}` with `append_to_response=watch/providers,external_ids`,
/// keeping availability for `region` only.
pub fn parse_details(
    media: MediaType,
    body: &[u8],
    region: &str,
    fetched_at: i64,
) -> Result<TitleDetails, ApiError> {
    let raw: RawDetails<'_> = serde_json::from_slice(body)?;
    let availability = match raw
        .watch_providers
        .as_ref()
        .and_then(|w| w.results.get(region))
    {
        Some(region_json) => serde_json::from_str(region_json.get())?,
        None => RegionAvailability::default(),
    };
    let date = match media {
        MediaType::Movie => raw.release_date.as_deref(),
        MediaType::Tv => raw.first_air_date.as_deref(),
    };
    let year = year_of(date);
    let imdb_id = raw
        .imdb_id
        .filter(|id| !id.is_empty())
        .or_else(|| raw.external_ids.and_then(|e| e.imdb_id))
        .filter(|id| !id.is_empty());
    Ok(TitleDetails {
        media,
        id: raw.id,
        title: raw.title.or(raw.name).unwrap_or_else(|| "Untitled".to_owned()),
        year,
        runtime_minutes: raw.runtime.filter(|&m| m > 0 && media == MediaType::Movie),
        seasons: raw.number_of_seasons.filter(|&n| n > 0 && media == MediaType::Tv),
        genres: raw.genres.into_iter().map(|g| g.name).collect(),
        overview: raw.overview.unwrap_or_default(),
        poster_path: raw.poster_path,
        backdrop_path: raw.backdrop_path,
        imdb_id,
        availability,
        fetched_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MATRIX: &str = include_str!("../../tests/fixtures/movie_603_details.json");
    const BLUEY: &str = include_str!("../../tests/fixtures/tv_82728_details.json");

    #[test]
    fn parses_movie_details_for_one_region() {
        let d = parse_details(MediaType::Movie, MATRIX.as_bytes(), "AU", 42).unwrap();
        assert_eq!(d.title, "The Matrix");
        assert_eq!(d.year, Some(1999));
        assert_eq!(d.runtime_minutes, Some(136));
        assert_eq!(d.seasons, None);
        assert_eq!(d.imdb_id.as_deref(), Some("tt0133093"));
        assert_eq!(d.genres, ["Action", "Science Fiction"]);
        assert_eq!(d.fetched_at, 42);
        let av = &d.availability;
        assert!(av.link.as_deref().unwrap().contains("locale=AU"));
        let flatrate: Vec<u32> = av.flatrate.iter().map(|o| o.provider_id).collect();
        assert_eq!(flatrate, [119, 21, 1899, 2100, 1825]);
        assert!(av.free.is_empty() && av.ads.is_empty());
        assert_eq!(av.rent.len(), 5);
        assert_eq!(av.buy.len(), 4);
    }

    #[test]
    fn parses_tv_details_with_external_imdb_id() {
        let d = parse_details(MediaType::Tv, BLUEY.as_bytes(), "AU", 0).unwrap();
        assert_eq!(d.title, "Bluey");
        assert_eq!(d.year, Some(2018));
        assert_eq!(d.seasons, Some(4));
        assert_eq!(d.runtime_minutes, None);
        assert_eq!(d.imdb_id.as_deref(), Some("tt7678620"));
        let free: Vec<&str> = d
            .availability
            .free
            .iter()
            .map(|o| o.provider_name.as_str())
            .collect();
        assert_eq!(free, ["ABC iview"]);
        assert!(d.availability.flatrate.is_empty());
    }

    #[test]
    fn missing_region_means_no_offers() {
        let d = parse_details(MediaType::Movie, MATRIX.as_bytes(), "ZZ", 0).unwrap();
        assert!(!d.availability.has_offers());
        assert_eq!(d.availability.link, None);
    }

    #[test]
    fn parses_search_and_discover_pages() {
        let search: Page =
            serde_json::from_str(include_str!("../../tests/fixtures/search_multi_matrix.json")).unwrap();
        let first = &search.results[0];
        assert_eq!(first.id, 603);
        assert_eq!(first.media(), Some(MediaType::Movie));
        assert_eq!(first.display_title(), "The Matrix");
        assert_eq!(first.year(), Some(1999));

        let discover: Page = serde_json::from_str(include_str!(
            "../../tests/fixtures/discover_movie_free_au_p3.json"
        ))
        .unwrap();
        assert_eq!(discover.page, 3);
        assert_eq!(discover.results.len(), 20);
        assert!(discover.total_results >= 10_000);
    }

    #[test]
    fn year_parsing_handles_empty_dates() {
        assert_eq!(year_of(Some("2018-10-01")), Some(2018));
        assert_eq!(year_of(Some("")), None);
        assert_eq!(year_of(None), None);
    }
}

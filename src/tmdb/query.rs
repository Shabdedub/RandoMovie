//! Discover queries for the two candidate pools.
//!
//! Pool A: titles on the user's services (`with_watch_providers` = S,
//! `with_watch_monetization_types=flatrate|free|ads`). Pool B: anything free or ad-supported
//! in the region (`with_watch_monetization_types=free|ads`, no provider filter).

use super::client::LANGUAGE;
use crate::media::MediaType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pool {
    /// Pool A, with the expanded provider ids.
    Services(Vec<u32>),
    /// Pool B.
    Free,
}

/// User and app filters that shape a discover query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoverFilters {
    pub region: String,
    /// `vote_count.gte`: the obscurity floor.
    pub obscurity_floor: u32,
    /// `vote_average.gte`: set about one point below the minimum IMDb rating.
    pub min_tmdb_average: Option<f32>,
    /// `with_genres`, OR-joined. Empty means any genre.
    pub genre_ids: Vec<u32>,
    pub year_from: Option<u16>,
    pub year_to: Option<u16>,
    /// `with_runtime.lte`, movies only.
    pub max_runtime: Option<u16>,
}

/// A discover query without the page number. Parameters are sorted, so equal queries have
/// equal keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiscoverQuery {
    pub media: MediaType,
    params: Vec<(&'static str, String)>,
}

impl DiscoverQuery {
    pub fn path(&self) -> String {
        format!("/discover/{}", self.media.as_str())
    }

    pub fn params(&self) -> &[(&'static str, String)] {
        &self.params
    }

    /// Identifies the query for the session cache of pool sizes.
    pub fn key(&self) -> String {
        let mut key = self.path();
        for (i, (k, v)) in self.params.iter().enumerate() {
            key.push(if i == 0 { '?' } else { '&' });
            key.push_str(k);
            key.push('=');
            key.push_str(v);
        }
        key
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

pub fn discover_query(media: MediaType, pool: &Pool, f: &DiscoverFilters) -> DiscoverQuery {
    let mut params: Vec<(&'static str, String)> = vec![
        ("include_adult", "false".to_owned()),
        ("language", LANGUAGE.to_owned()),
        ("sort_by", "popularity.desc".to_owned()),
        ("watch_region", f.region.clone()),
    ];
    match pool {
        Pool::Services(ids) => {
            params.push(("with_watch_providers", join_ids(ids)));
            params.push(("with_watch_monetization_types", "flatrate|free|ads".to_owned()));
        }
        Pool::Free => params.push(("with_watch_monetization_types", "free|ads".to_owned())),
    }
    if f.obscurity_floor > 0 {
        params.push(("vote_count.gte", f.obscurity_floor.to_string()));
    }
    if let Some(avg) = f.min_tmdb_average {
        params.push(("vote_average.gte", format!("{avg:.1}")));
    }
    if !f.genre_ids.is_empty() {
        params.push(("with_genres", join_ids(&f.genre_ids)));
    }
    let (from_key, to_key) = match media {
        MediaType::Movie => ("primary_release_date.gte", "primary_release_date.lte"),
        MediaType::Tv => ("first_air_date.gte", "first_air_date.lte"),
    };
    if let Some(year) = f.year_from {
        params.push((from_key, format!("{year:04}-01-01")));
    }
    if let Some(year) = f.year_to {
        params.push((to_key, format!("{year:04}-12-31")));
    }
    if media == MediaType::Movie
        && let Some(minutes) = f.max_runtime
    {
        params.push(("with_runtime.lte", minutes.to_string()));
    }
    params.sort_by(|a, b| a.0.cmp(b.0));
    DiscoverQuery { media, params }
}

fn join_ids(ids: &[u32]) -> String {
    ids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters() -> DiscoverFilters {
        DiscoverFilters {
            region: "AU".into(),
            obscurity_floor: 50,
            min_tmdb_average: Some(5.0),
            genre_ids: vec![35, 18],
            year_from: Some(1990),
            year_to: Some(1999),
            max_runtime: Some(120),
        }
    }

    #[test]
    fn services_pool_for_movies() {
        let q = discover_query(MediaType::Movie, &Pool::Services(vec![8, 1796]), &filters());
        assert_eq!(q.path(), "/discover/movie");
        assert_eq!(q.param("watch_region"), Some("AU"));
        assert_eq!(q.param("with_watch_providers"), Some("8|1796"));
        assert_eq!(q.param("with_watch_monetization_types"), Some("flatrate|free|ads"));
        assert_eq!(q.param("include_adult"), Some("false"));
        assert_eq!(q.param("vote_count.gte"), Some("50"));
        assert_eq!(q.param("vote_average.gte"), Some("5.0"));
        assert_eq!(q.param("with_genres"), Some("35|18"));
        assert_eq!(q.param("primary_release_date.gte"), Some("1990-01-01"));
        assert_eq!(q.param("primary_release_date.lte"), Some("1999-12-31"));
        assert_eq!(q.param("with_runtime.lte"), Some("120"));
    }

    #[test]
    fn free_pool_for_series() {
        let q = discover_query(MediaType::Tv, &Pool::Free, &filters());
        assert_eq!(q.path(), "/discover/tv");
        assert_eq!(q.param("with_watch_providers"), None);
        assert_eq!(q.param("with_watch_monetization_types"), Some("free|ads"));
        assert_eq!(q.param("first_air_date.gte"), Some("1990-01-01"));
        assert_eq!(q.param("with_runtime.lte"), None, "runtime filter is movies only");
    }

    #[test]
    fn keys_are_stable_and_distinct() {
        let a = discover_query(MediaType::Movie, &Pool::Free, &filters());
        let b = discover_query(MediaType::Movie, &Pool::Free, &filters());
        assert_eq!(a.key(), b.key());
        let mut other = filters();
        other.region = "NZ".into();
        let c = discover_query(MediaType::Movie, &Pool::Free, &other);
        assert_ne!(a.key(), c.key());
        assert!(a.key().starts_with("/discover/movie?include_adult=false&"));
    }
}

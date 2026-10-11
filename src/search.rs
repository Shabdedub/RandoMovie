//! Search: the query sent to TMDB, result pages turned into rows, and each row's rating and
//! availability line.

use std::collections::HashSet;

use crate::card::{ImdbBadge, TitleCard};
use crate::media::TitleRef;
use crate::picker::Verdict;
use crate::tmdb::model::Page;

/// Shortest query worth sending: a single letter matches too much to be useful.
pub const MIN_QUERY_CHARS: usize = 2;

/// The query as sent to TMDB: trimmed, with runs of whitespace collapsed. `None` while it's
/// too short to search.
pub fn normalize_query(text: &str) -> Option<String> {
    let query = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (query.chars().count() >= MIN_QUERY_CHARS).then_some(query)
}

/// One movie or series in the results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: TitleRef,
    pub name: String,
    /// "1999", or empty when TMDB has no date.
    pub year: String,
    /// "Movie" or "Series".
    pub kind: &'static str,
    pub poster_path: Option<String>,
}

impl SearchHit {
    /// "Movie · 1999", or just "Movie" without a year.
    pub fn meta(&self) -> String {
        if self.year.is_empty() {
            self.kind.to_owned()
        } else {
            format!("{} · {}", self.kind, self.year)
        }
    }
}

/// The movies and series on a results page, in TMDB's order. People are left out, and so is
/// any title already listed: `seen` covers every page loaded for the query so far.
pub fn hits(page: &Page, seen: &mut HashSet<TitleRef>) -> Vec<SearchHit> {
    page.results
        .iter()
        .filter_map(|item| {
            let media = item.media()?;
            let title = TitleRef::new(media, item.id);
            seen.insert(title).then(|| SearchHit {
                title,
                name: item.display_title().trim().to_owned(),
                year: item.year().map(|y| y.to_string()).unwrap_or_default(),
                kind: media.label(),
                poster_path: item.poster_path.clone(),
            })
        })
        .collect()
}

/// How a row shows where the title streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAvailability {
    /// Included with one of the user's services.
    OnYourServices,
    /// Free or ad-supported somewhere.
    Free,
    /// Streams only on other services, or is rent and buy only.
    NotOnServices,
    /// No offers at all in the region.
    NotStreaming,
}

/// A row's rating and availability, taken from the title's card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitInfo {
    /// "★ 8.7 IMDb · 2.3M votes", "IMDb rating unavailable" or "Loading IMDb ratings…".
    pub imdb: String,
    pub imdb_badge: ImdbBadge,
    /// "Included with Netflix", "Free on SBS On Demand", ...
    pub verdict: String,
    pub availability: RowAvailability,
}

pub fn hit_info(card: &TitleCard) -> HitInfo {
    HitInfo {
        imdb: card.imdb.clone(),
        imdb_badge: card.imdb_badge,
        verdict: card.verdict.clone(),
        availability: match card.watch.verdict {
            Verdict::Included(_) => RowAvailability::OnYourServices,
            Verdict::Free(_) => RowAvailability::Free,
            Verdict::NotOnServices => RowAvailability::NotOnServices,
            Verdict::Unavailable => RowAvailability::NotStreaming,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card;
    use crate::imdb::{ImdbStatus, Rating};
    use crate::media::MediaType;
    use crate::services::ServiceCatalog;
    use crate::tmdb::model::{ProviderList, parse_details};

    fn matrix_page() -> Page {
        serde_json::from_str(include_str!("../tests/fixtures/search_multi_matrix.json")).unwrap()
    }

    #[test]
    fn queries_are_trimmed_and_need_two_characters() {
        assert_eq!(normalize_query("  the   matrix "), Some("the matrix".to_owned()));
        assert_eq!(normalize_query("up"), Some("up".to_owned()));
        assert_eq!(normalize_query(" m "), None);
        assert_eq!(normalize_query("   "), None);
        assert_eq!(normalize_query("é"), None, "characters, not bytes");
        assert_eq!(normalize_query("éé"), Some("éé".to_owned()));
    }

    #[test]
    fn hits_keep_order_and_skip_repeats() {
        let page = matrix_page();
        let mut seen = HashSet::new();
        let hits = hits(&page, &mut seen);
        assert_eq!(hits.len(), 20);
        let first = &hits[0];
        assert_eq!(first.title, TitleRef::new(MediaType::Movie, 603));
        assert_eq!((first.name.as_str(), first.year.as_str(), first.kind), ("The Matrix", "1999", "Movie"));
        assert_eq!(first.meta(), "Movie · 1999");
        assert!(first.poster_path.is_some());
        let series = hits.iter().find(|h| h.title.media == MediaType::Tv).unwrap();
        assert_eq!((series.name.as_str(), series.kind), ("Escape The Matrix", "Series"));
        let undated = hits.iter().find(|h| h.title.id == 1_268_926).unwrap();
        assert_eq!(undated.meta(), "Movie");
        assert!(undated.poster_path.is_none());
        // The same page again (as "Load more" could return) adds nothing.
        assert!(super::hits(&page, &mut seen).is_empty());
    }

    #[test]
    fn info_comes_from_the_card() {
        let movie: ProviderList =
            serde_json::from_str(include_str!("../tests/fixtures/providers_movie_au.json")).unwrap();
        let tv: ProviderList =
            serde_json::from_str(include_str!("../tests/fixtures/providers_tv_au.json")).unwrap();
        let catalog = ServiceCatalog::from_lists("AU", &movie.results, &tv.results);
        let body = include_str!("../tests/fixtures/movie_603_details.json");
        let details = parse_details(MediaType::Movie, body.as_bytes(), "AU", 0).unwrap();
        let rated = ImdbStatus::Rated(Rating {
            rating_x10: 87,
            votes: 2_300_000,
        });
        let stan: HashSet<u32> = [21].into();
        let info = hit_info(&card::build(&details, &rated, &stan, &catalog, "Australia"));
        assert_eq!(info.imdb, "★ 8.7 IMDb · 2.3M votes");
        assert_eq!(info.imdb_badge, ImdbBadge::Rated);
        assert_eq!(info.verdict, "Included with Stan");
        assert_eq!(info.availability, RowAvailability::OnYourServices);
        let netflix: HashSet<u32> = [8].into();
        let info = hit_info(&card::build(&details, &rated, &netflix, &catalog, "Australia"));
        assert_eq!(info.availability, RowAvailability::NotOnServices);
        assert_eq!(info.verdict, "Not on your services");
    }
}

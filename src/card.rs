//! The title card's content, shared by the Pick and Search screens: everything shown about a
//! title, as display-ready text plus the image paths to load.

use std::collections::HashSet;

use crate::format;
use crate::imdb::ImdbStatus;
use crate::media::{MediaType, TitleRef};
use crate::picker::{Verdict, WhereToWatch, where_to_watch};
use crate::services::ServiceCatalog;
use crate::tmdb::model::TitleDetails;

/// How the IMDb badge should look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImdbBadge {
    Rated,
    Unavailable,
    Loading,
}

impl ImdbBadge {
    pub fn of(status: &ImdbStatus) -> ImdbBadge {
        match status {
            ImdbStatus::Rated(_) => ImdbBadge::Rated,
            ImdbStatus::Unavailable => ImdbBadge::Unavailable,
            ImdbStatus::Loading => ImdbBadge::Loading,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleCard {
    pub title: TitleRef,
    pub name: String,
    /// "1999", or empty when TMDB has no date.
    pub year: String,
    /// "Movie" or "Series".
    pub kind: &'static str,
    /// "2h 16m · Action, Science Fiction" or "4 seasons · Animation".
    pub facts: String,
    pub overview: String,
    /// "★ 7.8 IMDb · 1.2M votes", "IMDb rating unavailable" or "Loading IMDb ratings…".
    pub imdb: String,
    pub imdb_badge: ImdbBadge,
    /// "Included with Netflix", "Free on SBS On Demand", ...
    pub verdict: String,
    pub watch: WhereToWatch,
    /// The region the availability is for, as in "Where to watch in Australia".
    pub region_name: String,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    /// When the availability was fetched (unix seconds).
    pub fetched_at: i64,
}

/// Builds the card for one title in one region. `services` is the expanded selection.
pub fn build(
    details: &TitleDetails,
    imdb: &ImdbStatus,
    services: &HashSet<u32>,
    catalog: &ServiceCatalog,
    region_name: &str,
) -> TitleCard {
    let watch = where_to_watch(&details.availability, services, catalog);
    TitleCard {
        title: details.title_ref(),
        name: details.title.clone(),
        year: details.year.map(|y| y.to_string()).unwrap_or_default(),
        kind: details.media.label(),
        facts: facts(details),
        overview: if details.overview.trim().is_empty() {
            "No overview available.".to_owned()
        } else {
            details.overview.trim().to_owned()
        },
        imdb: format::imdb_badge(imdb),
        imdb_badge: ImdbBadge::of(imdb),
        verdict: format::verdict(&watch.verdict, region_name),
        watch,
        region_name: region_name.to_owned(),
        poster_path: details.poster_path.clone(),
        backdrop_path: details.backdrop_path.clone(),
        fetched_at: details.fetched_at,
    }
}

/// Runtime (movies) or seasons (series), then up to three genres.
pub fn facts(details: &TitleDetails) -> String {
    let mut parts = Vec::new();
    match details.media {
        MediaType::Movie => {
            if let Some(minutes) = details.runtime_minutes {
                parts.push(format::runtime(minutes));
            }
        }
        MediaType::Tv => {
            if let Some(seasons) = details.seasons {
                parts.push(format::seasons(seasons));
            }
        }
    }
    let genres: Vec<&str> = details.genres.iter().take(3).map(String::as_str).collect();
    if !genres.is_empty() {
        parts.push(genres.join(", "));
    }
    parts.join(" · ")
}

/// True when the verdict means the user can watch it now (on a service of theirs or free).
pub fn watchable(card: &TitleCard) -> bool {
    matches!(card.watch.verdict, Verdict::Included(_) | Verdict::Free(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imdb::Rating;
    use crate::picker::ChipGroupKind;
    use crate::tmdb::model::{ProviderList, parse_details};

    fn catalog() -> ServiceCatalog {
        let movie: ProviderList =
            serde_json::from_str(include_str!("../tests/fixtures/providers_movie_au.json")).unwrap();
        let tv: ProviderList =
            serde_json::from_str(include_str!("../tests/fixtures/providers_tv_au.json")).unwrap();
        ServiceCatalog::from_lists("AU", &movie.results, &tv.results)
    }

    #[test]
    fn movie_card() {
        let body = include_str!("../tests/fixtures/movie_603_details.json");
        let details = parse_details(MediaType::Movie, body.as_bytes(), "AU", 7).unwrap();
        let rated = ImdbStatus::Rated(Rating {
            rating_x10: 87,
            votes: 2_300_000,
        });
        let services: HashSet<u32> = [21].into();
        let card = build(&details, &rated, &services, &catalog(), "Australia");
        assert_eq!(card.name, "The Matrix");
        assert_eq!(card.year, "1999");
        assert_eq!(card.kind, "Movie");
        assert_eq!(card.facts, "2h 16m · Action, Science Fiction");
        assert_eq!(card.imdb, "★ 8.7 IMDb · 2.3M votes");
        assert_eq!(card.imdb_badge, ImdbBadge::Rated);
        assert_eq!(card.verdict, "Included with Stan");
        assert_eq!(card.region_name, "Australia");
        assert!(watchable(&card));
        let kinds: Vec<ChipGroupKind> = card.watch.groups.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            [
                ChipGroupKind::YourServices,
                ChipGroupKind::OtherSubscriptions,
                ChipGroupKind::RentBuy
            ]
        );
        assert!(card.watch.link.as_deref().unwrap().contains("locale=AU"));
        assert_eq!(card.fetched_at, 7);
        assert!(card.poster_path.is_some() && card.backdrop_path.is_some());
    }

    #[test]
    fn series_card_without_a_rating() {
        let body = include_str!("../tests/fixtures/tv_82728_details.json");
        let details = parse_details(MediaType::Tv, body.as_bytes(), "AU", 0).unwrap();
        let card = build(&details, &ImdbStatus::Loading, &HashSet::new(), &catalog(), "Australia");
        assert_eq!(card.kind, "Series");
        assert!(card.facts.starts_with("4 seasons · "));
        assert_eq!(card.imdb, "Loading IMDb ratings…");
        assert_eq!(card.verdict, "Free on ABC iview");
        assert!(watchable(&card));
    }

    #[test]
    fn missing_overview_and_offers_read_gracefully() {
        let body = include_str!("../tests/fixtures/movie_603_details.json");
        let mut details = parse_details(MediaType::Movie, body.as_bytes(), "ZZ", 0).unwrap();
        details.overview = "  ".into();
        details.year = None;
        let card = build(&details, &ImdbStatus::Unavailable, &HashSet::new(), &catalog(), "Nowhere");
        assert_eq!(card.overview, "No overview available.");
        assert_eq!(card.year, "");
        assert_eq!(card.imdb, "IMDb rating unavailable");
        assert_eq!(card.verdict, "Not available to stream in Nowhere");
        assert!(!watchable(&card));
    }
}

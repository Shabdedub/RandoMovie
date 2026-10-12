//! Where a service button under "Where to watch" leads. TMDB's availability names services
//! but has no links into them, so a button opens, in order of preference:
//!
//! 1. the title's own page on the service, when Wikidata records the service's id for the
//!    title (Netflix, Disney+, Apple TV);
//! 2. the service's search for the title, for services whose search links were checked;
//! 3. TMDB's watch page for the title, which links to every service that has it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::media::MediaType;

/// Wikidata properties: Netflix ID, Disney+ movie and series IDs, Apple TV movie and show IDs.
const NETFLIX: &str = "P1874";
const DISNEY_MOVIE: &str = "P7595";
const DISNEY_SERIES: &str = "P7596";
const APPLE_MOVIE: &str = "P9586";
const APPLE_SHOW: &str = "P9751";

/// A title's ids on streaming services, as Wikidata records them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceIds {
    #[serde(default)]
    pub netflix: Option<String>,
    #[serde(default)]
    pub disney_movie: Option<String>,
    #[serde(default)]
    pub disney_series: Option<String>,
    #[serde(default)]
    pub apple_movie: Option<String>,
    #[serde(default)]
    pub apple_show: Option<String>,
}

impl ServiceIds {
    /// The ids from a `wbgetentities` answer for one item. Ids go into links, so any that
    /// aren't plain letters, digits, dots, dashes or underscores are dropped.
    pub fn from_entity(body: &[u8]) -> Result<ServiceIds, serde_json::Error> {
        let value: Value = serde_json::from_slice(body)?;
        let claims = value["entities"]
            .as_object()
            .and_then(|entities| entities.values().next())
            .map(|entity| &entity["claims"]);
        let first = |property: &str| {
            claims
                .and_then(|claims| claims[property].as_array())
                .and_then(|values| {
                    values
                        .iter()
                        .find_map(|claim| claim["mainsnak"]["datavalue"]["value"].as_str())
                })
                .filter(|id| plain_id(id))
                .map(str::to_owned)
        };
        Ok(ServiceIds {
            netflix: first(NETFLIX),
            disney_movie: first(DISNEY_MOVIE),
            disney_series: first(DISNEY_SERIES),
            apple_movie: first(APPLE_MOVIE),
            apple_show: first(APPLE_SHOW),
        })
    }
}

fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// An IMDb id such as `tt0133093`, safe to put in a query.
pub fn is_imdb_id(id: &str) -> bool {
    id.len() > 2 && id.len() <= 16 && id.starts_with("tt") && id[2..].bytes().all(|b| b.is_ascii_digit())
}

/// Finds the Wikidata item with this IMDb id.
pub fn item_search_url(imdb_id: &str) -> String {
    format!(
        "https://www.wikidata.org/w/api.php?action=query&list=search&srsearch=haswbstatement:P345={imdb_id}&srlimit=1&srprop=&format=json"
    )
}

/// The item's statements, which hold the service ids.
pub fn item_claims_url(item: &str) -> String {
    format!("https://www.wikidata.org/w/api.php?action=wbgetentities&ids={item}&props=claims&format=json")
}

/// Looks up a title's service ids on Wikidata: its item by IMDb id, then the item's
/// statements. A title Wikidata doesn't have gives no ids; an unreachable Wikidata gives an
/// error.
pub async fn fetch_service_ids(http: &reqwest::Client, imdb_id: &str) -> Result<ServiceIds, String> {
    if !is_imdb_id(imdb_id) {
        return Err(format!("not an IMDb id: {imdb_id}"));
    }
    let get = async |url: String| -> Result<Vec<u8>, String> {
        let response = http.get(url).send().await.map_err(|e| e.without_url().to_string())?;
        let response = response.error_for_status().map_err(|e| e.without_url().to_string())?;
        Ok(response.bytes().await.map_err(|e| e.without_url().to_string())?.to_vec())
    };
    let search = get(item_search_url(imdb_id)).await?;
    match item_from_search(&search).map_err(|e| e.to_string())? {
        Some(item) => {
            let claims = get(item_claims_url(&item)).await?;
            ServiceIds::from_entity(&claims).map_err(|e| e.to_string())
        }
        None => Ok(ServiceIds::default()),
    }
}

/// The item (such as `Q83495`) from an item search answer, if one was found.
pub fn item_from_search(body: &[u8]) -> Result<Option<String>, serde_json::Error> {
    let value: Value = serde_json::from_slice(body)?;
    Ok(value["query"]["search"]
        .as_array()
        .and_then(|results| results.first())
        .and_then(|result| result["title"].as_str())
        .filter(|item| item.len() > 1 && item.starts_with('Q') && item[1..].bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    Netflix,
    DisneyPlus,
    AppleTv,
    PrimeVideo,
    Binge,
    SbsOnDemand,
    AbcIview,
    Tubi,
    YouTube,
    GooglePlay,
}

/// The service behind a TMDB provider id; plans with ads, kids' plans, stores and channels of
/// a service open the same site.
fn service(provider_id: u32) -> Option<Service> {
    Some(match provider_id {
        8 | 175 | 1796 => Service::Netflix,
        337 => Service::DisneyPlus,
        2 | 350 | 2243 => Service::AppleTv,
        9 | 10 | 119 | 2100 => Service::PrimeVideo,
        385 => Service::Binge,
        132 => Service::SbsOnDemand,
        135 => Service::AbcIview,
        73 => Service::Tubi,
        188 | 192 => Service::YouTube,
        3 => Service::GooglePlay,
        _ => return None,
    })
}

/// The button opens the service itself (its page for the title, or its search), rather than
/// TMDB's watch page.
pub fn opens_service(provider_id: u32) -> bool {
    service(provider_id).is_some()
}

/// Opening this service can use ids from Wikidata, so they're worth looking up.
pub fn uses_service_ids(provider_id: u32) -> bool {
    matches!(
        service(provider_id),
        Some(Service::Netflix | Service::DisneyPlus | Service::AppleTv)
    )
}

/// The link a service button opens. `name` is the title searched for, `region` the two-letter
/// country, and `watch_page` TMDB's watch page for the title in the region.
pub fn button_url(
    provider_id: u32,
    media: MediaType,
    ids: Option<&ServiceIds>,
    name: &str,
    region: &str,
    watch_page: Option<&str>,
) -> Option<String> {
    let none = ServiceIds::default();
    let ids = ids.unwrap_or(&none);
    let q = encode(name);
    let url = match service(provider_id) {
        Some(Service::Netflix) => Some(match &ids.netflix {
            Some(id) => format!("https://www.netflix.com/title/{id}"),
            None => format!("https://www.netflix.com/search?q={q}"),
        }),
        // Disney+ search needs a sign-in to take a query, so without an id the watch page is
        // the better way in.
        Some(Service::DisneyPlus) => match media {
            MediaType::Movie => ids.disney_movie.as_ref().map(|id| format!("https://www.disneyplus.com/movies/wd/{id}")),
            MediaType::Tv => ids.disney_series.as_ref().map(|id| format!("https://www.disneyplus.com/series/wp/{id}")),
        },
        Some(Service::AppleTv) => Some(match (media, &ids.apple_movie, &ids.apple_show) {
            (MediaType::Movie, Some(id), _) => format!("https://tv.apple.com/movie/{id}"),
            (MediaType::Tv, _, Some(id)) => format!("https://tv.apple.com/show/{id}"),
            _ => {
                let country = if region.len() == 2 && region.bytes().all(|b| b.is_ascii_alphabetic()) {
                    region.to_ascii_lowercase()
                } else {
                    "us".to_owned()
                };
                format!("https://tv.apple.com/{country}/search?term={q}")
            }
        }),
        Some(Service::PrimeVideo) => Some(format!("https://www.primevideo.com/search/ref=atv_nb_sug?ie=UTF8&phrase={q}")),
        Some(Service::Binge) => Some(format!("https://binge.com.au/search?q={q}")),
        Some(Service::SbsOnDemand) => Some(format!("https://www.sbs.com.au/ondemand/search?query={q}")),
        Some(Service::AbcIview) => Some(format!("https://iview.abc.net.au/search?keyword={q}")),
        Some(Service::Tubi) => Some(format!("https://tubitv.com/search/{q}")),
        Some(Service::YouTube) => Some(format!("https://www.youtube.com/results?search_query={q}")),
        Some(Service::GooglePlay) => Some(format!("https://play.google.com/store/search?q={q}&c=movies")),
        None => None,
    };
    url.or_else(|| watch_page.filter(|page| page.starts_with("https://")).map(str::to_owned))
}

/// Percent-encodes text for a query or path: everything except unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.trim().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WATCH: Option<&str> = Some("https://www.themoviedb.org/movie/603-the-matrix/watch?locale=AU");

    fn ids() -> ServiceIds {
        ServiceIds {
            netflix: Some("20557937".into()),
            disney_movie: Some("4uKGzAJi3ROz".into()),
            disney_series: Some("3jLIGMDYINqD".into()),
            apple_movie: Some("umc.cmc.af8k9kcq9r1s1qmmdxpq4itn".into()),
            apple_show: Some("umc.cmc.vtoh0mn0xn7t3c643xqonfzy".into()),
        }
    }

    #[test]
    fn exact_pages_when_ids_are_known() {
        let ids = ids();
        let url = |provider, media| button_url(provider, media, Some(&ids), "The Matrix", "AU", WATCH).unwrap();
        assert_eq!(url(8, MediaType::Movie), "https://www.netflix.com/title/20557937");
        assert_eq!(url(1796, MediaType::Movie), "https://www.netflix.com/title/20557937", "plan with ads");
        assert_eq!(url(337, MediaType::Movie), "https://www.disneyplus.com/movies/wd/4uKGzAJi3ROz");
        assert_eq!(url(337, MediaType::Tv), "https://www.disneyplus.com/series/wp/3jLIGMDYINqD");
        assert_eq!(url(350, MediaType::Movie), "https://tv.apple.com/movie/umc.cmc.af8k9kcq9r1s1qmmdxpq4itn");
        assert_eq!(url(2, MediaType::Tv), "https://tv.apple.com/show/umc.cmc.vtoh0mn0xn7t3c643xqonfzy", "store");
    }

    #[test]
    fn search_then_watch_page_without_ids() {
        let url = |provider, name| button_url(provider, MediaType::Movie, None, name, "AU", WATCH);
        assert_eq!(url(8, "The Matrix").unwrap(), "https://www.netflix.com/search?q=The%20Matrix");
        assert_eq!(url(350, "Amélie & Co").unwrap(), "https://tv.apple.com/au/search?term=Am%C3%A9lie%20%26%20Co");
        assert_eq!(url(119, "The Boys").unwrap(), "https://www.primevideo.com/search/ref=atv_nb_sug?ie=UTF8&phrase=The%20Boys");
        assert_eq!(url(73, "Mad Max: Fury Road").unwrap(), "https://tubitv.com/search/Mad%20Max%3A%20Fury%20Road");
        assert_eq!(url(132, "Vikings").unwrap(), "https://www.sbs.com.au/ondemand/search?query=Vikings");
        // Disney+ without an id, and services with no checked search, open the watch page.
        assert_eq!(url(337, "Frozen").as_deref(), WATCH);
        assert_eq!(url(21, "Bluey").as_deref(), WATCH, "Stan");
        assert_eq!(button_url(21, MediaType::Tv, None, "Bluey", "AU", None), None);
        assert_eq!(button_url(21, MediaType::Tv, None, "Bluey", "AU", Some("javascript:alert(1)")), None);
    }

    #[test]
    fn buttons_know_where_they_lead() {
        assert!(opens_service(8) && opens_service(385) && !opens_service(21));
        assert!(uses_service_ids(8) && uses_service_ids(337) && uses_service_ids(2));
        assert!(!uses_service_ids(119), "Prime Video ids on Wikidata are US store ids");
    }

    #[test]
    fn reads_wikidata_answers() {
        let search = br#"{"query":{"search":[{"ns":0,"title":"Q83495"}]}}"#;
        assert_eq!(item_from_search(search).unwrap().as_deref(), Some("Q83495"));
        assert_eq!(item_from_search(br#"{"query":{"search":[]}}"#).unwrap(), None);
        assert_eq!(item_from_search(br#"{"query":{"search":[{"title":"Q1&x=y"}]}}"#).unwrap(), None);

        let entity = br#"{"entities":{"Q83495":{"claims":{
            "P1874":[{"mainsnak":{"datavalue":{"value":"20557937"}}}],
            "P9586":[{"mainsnak":{"datavalue":{"value":"umc.cmc.af8k9kcq9r1s1qmmdxpq4itn"}}}],
            "P7595":[{"mainsnak":{"datavalue":{"value":"bad id/../x"}}}]
        }}}}"#;
        let ids = ServiceIds::from_entity(entity).unwrap();
        assert_eq!(ids.netflix.as_deref(), Some("20557937"));
        assert_eq!(ids.apple_movie.as_deref(), Some("umc.cmc.af8k9kcq9r1s1qmmdxpq4itn"));
        assert_eq!(ids.disney_movie, None, "unsafe ids are dropped");
        assert_eq!(ids.disney_series, None);

        assert!(is_imdb_id("tt0133093") && !is_imdb_id("tt") && !is_imdb_id("tt01&x") && !is_imdb_id("nm0000206"));
    }
}

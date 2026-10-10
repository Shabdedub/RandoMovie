//! The pick loop.
//!
//! 1. Build the pools for the request (pool A per media type when services are selected,
//!    pool B per media type when free titles are on).
//! 2. Learn each pool's size from page 1, cached for the session by the full query.
//! 3. Draw a pool and index uniformly, fetch that page, and try the drawn item first.
//! 4. Verify availability with details no older than 24 h, then apply the IMDb minimums,
//!    the runtime limit, the exclusions and the recent picks.
//! 5. On a miss, try the rest of the page in random order, then draw a new page. After
//!    10 misses (or 10 pages) give up with "Nothing matches".

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::{Mutex, MutexGuard};

use super::eligibility::is_eligible;
use super::genres::{UnifiedGenre, ids_for};
use super::sampling::{PAGE_SIZE, draw, page_and_offset, reachable, try_order};
use crate::error::ApiError;
use crate::imdb::{ImdbRatings, ImdbStatus};
use crate::media::{MediaChoice, MediaType, TitleRef};
use crate::store::Settings;
use crate::tmdb::model::{Page, TitleDetails};
use crate::tmdb::query::{DiscoverFilters, DiscoverQuery, Pool, discover_query};
use crate::tmdb::{Freshness, TmdbClient};

pub const MAX_MISSES: u32 = 10;
pub const MAX_PAGE_FETCHES: u32 = 10;
/// Picks this recent are never repeated.
pub const RECENT_PICKS_EXCLUDED: usize = 50;

/// Where candidates and details come from. `TmdbClient` in the app; a fake in tests.
pub trait Catalog {
    fn discover(&self, query: &DiscoverQuery, page: u32) -> impl Future<Output = Result<Page, ApiError>>;
    /// Must return availability data no older than 24 hours.
    fn details(&self, title: TitleRef, region: &str) -> impl Future<Output = Result<TitleDetails, ApiError>>;
}

impl Catalog for TmdbClient {
    fn discover(&self, query: &DiscoverQuery, page: u32) -> impl Future<Output = Result<Page, ApiError>> {
        TmdbClient::discover(self, query, page)
    }

    fn details(&self, title: TitleRef, region: &str) -> impl Future<Output = Result<TitleDetails, ApiError>> {
        TmdbClient::details(self, title, region, Freshness::Fresh)
    }
}

pub trait RatingSource {
    fn rating(&self, imdb_id: Option<&str>) -> ImdbStatus;
}

impl RatingSource for ImdbRatings {
    fn rating(&self, imdb_id: Option<&str>) -> ImdbStatus {
        self.lookup(imdb_id)
    }
}

/// Everything that decides which titles may be picked.
#[derive(Debug, Clone, PartialEq)]
pub struct PickRequest {
    pub region: String,
    /// S: the selected services expanded to variant ids.
    pub services: Vec<u32>,
    pub include_free: bool,
    pub media: MediaChoice,
    /// `None`: any genre. `Some(ids)`: any of these; empty means movies can't match.
    pub movie_genres: Option<Vec<u32>>,
    pub tv_genres: Option<Vec<u32>>,
    /// Minimum IMDb rating × 10; `None` is "Any".
    pub min_imdb_x10: Option<u8>,
    pub min_imdb_votes: Option<u32>,
    pub year_from: Option<u16>,
    pub year_to: Option<u16>,
    /// Movies only, in minutes.
    pub max_runtime: Option<u16>,
    /// Minimum TMDB vote count.
    pub obscurity_floor: u32,
}

impl PickRequest {
    pub fn from_settings(
        settings: &Settings,
        region: &str,
        media: MediaChoice,
        services: Vec<u32>,
        genres: &[UnifiedGenre],
    ) -> PickRequest {
        let f = &settings.filters;
        PickRequest {
            region: region.to_owned(),
            services,
            include_free: settings.include_free,
            media,
            movie_genres: ids_for(genres, &f.genres, MediaType::Movie),
            tv_genres: ids_for(genres, &f.genres, MediaType::Tv),
            min_imdb_x10: f
                .min_imdb_rating
                .map(|r| (r * 10.0).round().clamp(0.0, 100.0) as u8),
            min_imdb_votes: f.min_imdb_votes,
            year_from: f.year_from,
            year_to: f.year_to,
            max_runtime: f.max_runtime,
            obscurity_floor: settings.obscurity_floor,
        }
    }

    fn discover_filters(&self, genre_ids: Vec<u32>) -> DiscoverFilters {
        DiscoverFilters {
            region: self.region.clone(),
            obscurity_floor: self.obscurity_floor,
            // About one point below the IMDb minimum, to skip hopeless candidates early.
            min_tmdb_average: self
                .min_imdb_x10
                .filter(|&m| m > 10)
                .map(|m| f32::from(m - 10) / 10.0),
            genre_ids,
            year_from: self.year_from,
            year_to: self.year_to,
            max_runtime: self.max_runtime,
        }
    }

    /// The discover queries to sample from.
    pub fn pools(&self) -> Result<Vec<DiscoverQuery>, PickError> {
        if self.services.is_empty() && !self.include_free {
            return Err(PickError::NeedServices);
        }
        let mut pools = Vec::new();
        for &media in self.media.types() {
            let genres = match media {
                MediaType::Movie => &self.movie_genres,
                MediaType::Tv => &self.tv_genres,
            };
            let genre_ids = match genres {
                None => Vec::new(),
                Some(ids) if ids.is_empty() => continue,
                Some(ids) => ids.clone(),
            };
            let filters = self.discover_filters(genre_ids);
            if !self.services.is_empty() {
                pools.push(discover_query(media, &Pool::Services(self.services.clone()), &filters));
            }
            if self.include_free {
                pools.push(discover_query(media, &Pool::Free, &filters));
            }
        }
        if pools.is_empty() {
            return Err(PickError::NothingMatches);
        }
        Ok(pools)
    }
}

/// A verified pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub details: TitleDetails,
    pub imdb: ImdbStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickError {
    /// No services selected and free titles are off.
    NeedServices,
    NothingMatches,
    Api(ApiError),
}

impl fmt::Display for PickError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PickError::NeedServices => {
                write!(f, "Choose your streaming services, or turn on free titles")
            }
            PickError::NothingMatches => write!(f, "Nothing matches, try looser filters"),
            PickError::Api(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PickError {}

/// Titles that must not be picked: "Seen it", "Not interested" and recent picks.
#[derive(Debug, Clone, Default)]
pub struct SkipList {
    pub excluded: HashSet<TitleRef>,
    pub recent: HashSet<TitleRef>,
}

impl SkipList {
    pub fn contains(&self, title: &TitleRef) -> bool {
        self.excluded.contains(title) || self.recent.contains(title)
    }
}

/// Holds the session cache of pool sizes and the random generator.
pub struct Picker {
    sizes: Mutex<HashMap<String, u32>>,
    rng: Mutex<fastrand::Rng>,
}

impl Default for Picker {
    fn default() -> Self {
        Picker::new()
    }
}

impl Picker {
    pub fn new() -> Picker {
        Picker::with_rng(fastrand::Rng::new())
    }

    pub fn with_seed(seed: u64) -> Picker {
        Picker::with_rng(fastrand::Rng::with_seed(seed))
    }

    fn with_rng(rng: fastrand::Rng) -> Picker {
        Picker {
            sizes: Mutex::new(HashMap::new()),
            rng: Mutex::new(rng),
        }
    }

    /// Forgets cached pool sizes (any change of region, services or filters makes new
    /// query keys anyway; this also frees the memory).
    pub fn reset(&self) {
        self.sizes().clear();
    }

    fn sizes(&self) -> MutexGuard<'_, HashMap<String, u32>> {
        self.sizes.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn rng(&self) -> MutexGuard<'_, fastrand::Rng> {
        self.rng.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub async fn pick<C: Catalog, R: RatingSource>(
        &self,
        catalog: &C,
        ratings: &R,
        request: &PickRequest,
        skip: &SkipList,
    ) -> Result<Pick, PickError> {
        let pools = request.pools()?;
        let mut sizes = self.pool_sizes(catalog, &pools).await?;
        let services: HashSet<u32> = request.services.iter().copied().collect();
        let mut misses = 0;
        let mut pages = 0;
        while misses < MAX_MISSES && pages < MAX_PAGE_FETCHES {
            let drawn = draw(&sizes, &mut self.rng());
            let Some((pool, index)) = drawn else {
                return Err(PickError::NothingMatches);
            };
            let (page_number, offset) = page_and_offset(index);
            let query = &pools[pool];
            let page = catalog
                .discover(query, page_number)
                .await
                .map_err(PickError::Api)?;
            pages += 1;

            // Keep the size estimate in step with what TMDB reports now.
            let mut size = reachable(page.total_results);
            if page.results.is_empty() {
                size = size.min((page_number - 1) * PAGE_SIZE);
                misses += 1;
            }
            if size != sizes[pool] {
                sizes[pool] = size;
                self.sizes().insert(query.key(), size);
            }

            let order = try_order(offset, page.results.len(), &mut self.rng());
            for i in order {
                let title = TitleRef::new(query.media, page.results[i].id);
                if skip.contains(&title) {
                    continue;
                }
                match verify(catalog, ratings, request, &services, title).await? {
                    Some(pick) => return Ok(pick),
                    None => {
                        misses += 1;
                        if misses >= MAX_MISSES {
                            break;
                        }
                    }
                }
            }
        }
        Err(PickError::NothingMatches)
    }

    /// Page-1 sizes for up to four pools, fetched concurrently on first use.
    async fn pool_sizes<C: Catalog>(
        &self,
        catalog: &C,
        pools: &[DiscoverQuery],
    ) -> Result<Vec<u32>, PickError> {
        debug_assert!(pools.len() <= 4, "two media types times two pools");
        let size_of = |i: usize| async move {
            match pools.get(i) {
                Some(query) => self.pool_size(catalog, query).await.map(Some),
                None => Ok(None),
            }
        };
        let (a, b, c, d) = tokio::join!(size_of(0), size_of(1), size_of(2), size_of(3));
        let mut sizes = Vec::with_capacity(pools.len());
        for size in [a, b, c, d] {
            if let Some(size) = size.map_err(PickError::Api)? {
                sizes.push(size);
            }
        }
        Ok(sizes)
    }

    async fn pool_size<C: Catalog>(&self, catalog: &C, query: &DiscoverQuery) -> Result<u32, ApiError> {
        let key = query.key();
        if let Some(&size) = self.sizes().get(&key) {
            return Ok(size);
        }
        let first_page = catalog.discover(query, 1).await?;
        let size = reachable(first_page.total_results);
        self.sizes().insert(key, size);
        Ok(size)
    }
}

/// Fetches details and applies the guarantee and the filters. `Ok(None)` is a miss.
async fn verify<C: Catalog, R: RatingSource>(
    catalog: &C,
    ratings: &R,
    request: &PickRequest,
    services: &HashSet<u32>,
    title: TitleRef,
) -> Result<Option<Pick>, PickError> {
    let details = match catalog.details(title, &request.region).await {
        Ok(details) => details,
        Err(ApiError::NotFound) => return Ok(None),
        Err(e) => return Err(PickError::Api(e)),
    };
    if !is_eligible(&details.availability, services, request.include_free) {
        return Ok(None);
    }
    if let (Some(max), Some(runtime)) = (request.max_runtime, details.runtime_minutes)
        && runtime > u32::from(max)
    {
        return Ok(None);
    }
    let imdb = ratings.rating(details.imdb_id.as_deref());
    if !passes_imdb_minimums(&imdb, request) {
        return Ok(None);
    }
    Ok(Some(Pick { details, imdb }))
}

/// With a minimum set, a title needs an IMDb rating that meets it. While the ratings are
/// still downloading the check is skipped; discover's `vote_average.gte` still applies.
fn passes_imdb_minimums(status: &ImdbStatus, request: &PickRequest) -> bool {
    if request.min_imdb_x10.is_none() && request.min_imdb_votes.is_none() {
        return true;
    }
    match status {
        ImdbStatus::Loading => true,
        ImdbStatus::Unavailable => false,
        ImdbStatus::Rated(r) => {
            request.min_imdb_x10.is_none_or(|min| r.rating_x10 >= min)
                && request.min_imdb_votes.is_none_or(|min| r.votes >= min)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imdb::Rating;
    use crate::tmdb::model::{ListItem, Offer, RegionAvailability};
    use std::cell::Cell;

    const NETFLIX: u32 = 8;
    const SBS: u32 = 132;
    const APPLE_STORE: u32 = 2;

    fn offer(id: u32) -> Offer {
        Offer {
            provider_id: id,
            provider_name: format!("P{id}"),
            logo_path: None,
            display_priority: 1,
        }
    }

    /// Movie `id` streams on Netflix when divisible by 3 and is free on SBS when divisible
    /// by 5; every movie can be rented. Its runtime is 80 + id minutes.
    ///
    /// Like TMDB, discover is mostly right but slightly stale: pool A also lists movies
    /// divisible by 7 that have left Netflix, so verification has misses to catch. Discover
    /// applies `with_runtime.lte` itself. Series 1001..=1010 are in pool B and free.
    struct FakeCatalog {
        detail_calls: Cell<u32>,
    }

    impl FakeCatalog {
        fn new() -> FakeCatalog {
            FakeCatalog { detail_calls: Cell::new(0) }
        }

        fn ids(query: &DiscoverQuery) -> Vec<u32> {
            let max_runtime: u32 = query
                .param("with_runtime.lte")
                .map_or(u32::MAX, |m| m.parse().unwrap());
            let movies = |keep: fn(u32) -> bool| -> Vec<u32> {
                (1..=45).filter(|&id| keep(id) && 80 + id <= max_runtime).collect()
            };
            match (query.media, query.param("with_watch_providers")) {
                (MediaType::Movie, Some(_)) => {
                    movies(|id| id.is_multiple_of(3) || id.is_multiple_of(7))
                }
                (MediaType::Movie, None) => movies(|id| id.is_multiple_of(5)),
                (MediaType::Tv, Some(_)) => Vec::new(),
                (MediaType::Tv, None) => (1001..=1010).collect(),
            }
        }
    }

    impl Catalog for FakeCatalog {
        async fn discover(&self, query: &DiscoverQuery, page: u32) -> Result<Page, ApiError> {
            let ids = FakeCatalog::ids(query);
            let start = ((page - 1) * PAGE_SIZE) as usize;
            let results = ids
                .iter()
                .skip(start)
                .take(PAGE_SIZE as usize)
                .map(|&id| ListItem {
                    id,
                    media_type: None,
                    title: Some(format!("Title {id}")),
                    name: None,
                    release_date: None,
                    first_air_date: None,
                    poster_path: None,
                })
                .collect();
            Ok(Page {
                page,
                results,
                total_pages: (ids.len() as u32).div_ceil(PAGE_SIZE),
                total_results: ids.len() as u32,
            })
        }

        async fn details(&self, title: TitleRef, _region: &str) -> Result<TitleDetails, ApiError> {
            self.detail_calls.set(self.detail_calls.get() + 1);
            let id = title.id;
            let mut availability = RegionAvailability {
                rent: vec![offer(APPLE_STORE)],
                ..Default::default()
            };
            if title.media == MediaType::Movie && id.is_multiple_of(3) {
                availability.flatrate.push(offer(NETFLIX));
            }
            if title.media == MediaType::Tv || id.is_multiple_of(5) {
                availability.free.push(offer(SBS));
            }
            Ok(TitleDetails {
                media: title.media,
                id,
                title: format!("Title {id}"),
                year: Some(2000),
                runtime_minutes: (title.media == MediaType::Movie).then_some(80 + id),
                seasons: None,
                genres: Vec::new(),
                overview: String::new(),
                poster_path: None,
                backdrop_path: None,
                imdb_id: Some(format!("tt{id:07}")),
                availability,
                fetched_at: 0,
            })
        }
    }

    /// Every title is rated 7.0 with 5000 votes, except id 9 at 4.0.
    struct FakeRatings;

    impl RatingSource for FakeRatings {
        fn rating(&self, imdb_id: Option<&str>) -> ImdbStatus {
            match imdb_id {
                Some("tt0000009") => ImdbStatus::Rated(Rating { rating_x10: 40, votes: 5000 }),
                Some(_) => ImdbStatus::Rated(Rating { rating_x10: 70, votes: 5000 }),
                None => ImdbStatus::Unavailable,
            }
        }
    }

    fn request(services: &[u32], include_free: bool, media: MediaChoice) -> PickRequest {
        PickRequest {
            region: "AU".into(),
            services: services.to_vec(),
            include_free,
            media,
            movie_genres: None,
            tv_genres: None,
            min_imdb_x10: Some(60),
            min_imdb_votes: Some(1000),
            year_from: None,
            year_to: None,
            max_runtime: None,
            obscurity_floor: 50,
        }
    }

    fn run<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn picks_only_titles_on_the_selected_service() {
        let catalog = FakeCatalog::new();
        let picker = Picker::with_seed(11);
        let req = request(&[NETFLIX], false, MediaChoice::Movies);
        for _ in 0..200 {
            let pick = run(picker.pick(&catalog, &FakeRatings, &req, &SkipList::default())).unwrap();
            let id = pick.details.id;
            assert_eq!(id % 3, 0, "title {id} isn't on Netflix");
            assert_ne!(id, 9, "below the IMDb minimum");
            assert!(pick.details.availability.flatrate.iter().any(|o| o.provider_id == NETFLIX));
        }
    }

    #[test]
    fn free_only_picks_are_free() {
        let catalog = FakeCatalog::new();
        let picker = Picker::with_seed(12);
        let req = request(&[], true, MediaChoice::Both);
        let mut saw_series = false;
        for _ in 0..200 {
            let pick = run(picker.pick(&catalog, &FakeRatings, &req, &SkipList::default())).unwrap();
            let av = &pick.details.availability;
            assert!(!av.free.is_empty() || !av.ads.is_empty());
            saw_series |= pick.details.media == MediaType::Tv;
        }
        assert!(saw_series, "both media types are sampled");
    }

    #[test]
    fn skip_list_and_filters_are_respected() {
        let catalog = FakeCatalog::new();
        let picker = Picker::with_seed(13);
        let mut req = request(&[NETFLIX], false, MediaChoice::Movies);
        req.max_runtime = Some(100); // runtime is 80 + id, so only ids up to 20
        let mut skip = SkipList::default();
        skip.excluded.insert(TitleRef::new(MediaType::Movie, 3));
        skip.recent.insert(TitleRef::new(MediaType::Movie, 6));
        for _ in 0..100 {
            let pick = run(picker.pick(&catalog, &FakeRatings, &req, &skip)).unwrap();
            let id = pick.details.id;
            assert!([12, 15, 18].contains(&id), "unexpected pick {id}");
        }
    }

    #[test]
    fn runtime_limit_is_checked_after_discover() {
        let catalog = FakeCatalog::new();
        let services: HashSet<u32> = [NETFLIX].into();
        let mut req = request(&[NETFLIX], false, MediaChoice::Movies);
        req.max_runtime = Some(100);
        let long = TitleRef::new(MediaType::Movie, 30); // 110 minutes, on Netflix
        let short = TitleRef::new(MediaType::Movie, 18); // 98 minutes, on Netflix
        assert_eq!(run(verify(&catalog, &FakeRatings, &req, &services, long)), Ok(None));
        assert!(run(verify(&catalog, &FakeRatings, &req, &services, short)).unwrap().is_some());
    }

    #[test]
    fn gives_up_after_ten_misses() {
        let catalog = FakeCatalog::new();
        let picker = Picker::with_seed(14);
        // Provider 999 matches the fake discover pool but no title really streams there.
        let req = request(&[999], false, MediaChoice::Movies);
        let result = run(picker.pick(&catalog, &FakeRatings, &req, &SkipList::default()));
        assert_eq!(result, Err(PickError::NothingMatches));
        assert_eq!(catalog.detail_calls.get(), MAX_MISSES);
    }

    #[test]
    fn needs_services_or_free_titles() {
        let req = request(&[], false, MediaChoice::Both);
        assert_eq!(req.pools(), Err(PickError::NeedServices));
    }

    #[test]
    fn genre_filters_drop_unmatched_media_types() {
        let mut req = request(&[NETFLIX], true, MediaChoice::Both);
        assert_eq!(req.pools().unwrap().len(), 4);
        req.tv_genres = Some(Vec::new()); // e.g. Horror: no TV equivalent
        req.movie_genres = Some(vec![27]);
        let pools = req.pools().unwrap();
        assert_eq!(pools.len(), 2);
        assert!(pools.iter().all(|q| q.media == MediaType::Movie));
        assert!(pools.iter().all(|q| q.param("with_genres") == Some("27")));
        assert_eq!(pools[0].param("vote_average.gte"), Some("5.0"));
        req.movie_genres = Some(Vec::new());
        assert_eq!(req.pools(), Err(PickError::NothingMatches));
    }

    #[test]
    fn imdb_minimums() {
        let req = request(&[NETFLIX], false, MediaChoice::Movies);
        let rated = |r, v| ImdbStatus::Rated(Rating { rating_x10: r, votes: v });
        assert!(passes_imdb_minimums(&rated(60, 1000), &req));
        assert!(!passes_imdb_minimums(&rated(59, 1000), &req));
        assert!(!passes_imdb_minimums(&rated(80, 999), &req));
        assert!(!passes_imdb_minimums(&ImdbStatus::Unavailable, &req));
        assert!(passes_imdb_minimums(&ImdbStatus::Loading, &req));
        let mut any = req.clone();
        any.min_imdb_x10 = None;
        any.min_imdb_votes = None;
        assert!(passes_imdb_minimums(&ImdbStatus::Unavailable, &any));
    }

    #[test]
    fn request_from_settings() {
        let mut settings = Settings::default();
        settings.filters.genres = vec!["Horror".into()];
        let genres = vec![UnifiedGenre {
            name: "Horror".into(),
            movie_ids: vec![27],
            tv_ids: vec![],
        }];
        let req = PickRequest::from_settings(&settings, "AU", MediaChoice::Both, vec![8], &genres);
        assert_eq!(req.min_imdb_x10, Some(60));
        assert_eq!(req.min_imdb_votes, Some(1000));
        assert_eq!(req.movie_genres, Some(vec![27]));
        assert_eq!(req.tv_genres, Some(vec![]));
        assert_eq!(req.obscurity_floor, 50);
        assert!(req.include_free);
    }
}

//! Live checks against TMDB and IMDb, including the spec's acceptance checks.
//! They need the network and `TMDB_TOKEN` in `.env`, so they are ignored by default:
//!
//! ```text
//! cargo test --release --test live -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Data goes to `target/tmp/live`, never to the app's real folders. Each test starts with an
//! empty cache and history; the IMDb index is downloaded once and reused.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use randomovie::error::ApiError;
use randomovie::format;
use randomovie::imdb::{ImdbRatings, ImdbStatus, RefreshOutcome};
use randomovie::media::{MediaChoice, MediaType, TitleRef};
use randomovie::paths::AppPaths;
use randomovie::picker::genres::unify;
use randomovie::picker::{
    ChipGroupKind, PickRequest, Picker, SkipList, UnifiedGenre, Verdict, where_to_watch,
};
use randomovie::services::{ServiceCatalog, ServiceSelection};
use randomovie::store::{Settings, Store};
use randomovie::tmdb::{Credential, Freshness, TmdbClient};
use randomovie::util::time::{DAY, now_unix};

const REGION: &str = "AU";

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn paths() -> AppPaths {
    let paths = AppPaths::under(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("live"));
    paths.ensure_dirs().unwrap();
    paths
}

fn credential() -> Credential {
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    let token = std::env::var("TMDB_TOKEN").expect("TMDB_TOKEN in .env");
    Credential::from_secret(&token).expect("non-empty TMDB_TOKEN")
}

/// Downloads the IMDb index once per test process.
fn ensure_imdb() {
    static READY: Mutex<bool> = Mutex::new(false);
    let mut ready = READY.lock().unwrap_or_else(|e| e.into_inner());
    if *ready {
        return;
    }
    let imdb = ImdbRatings::open(&paths().imdb_dir());
    if imdb.needs_refresh(now_unix()) {
        let http = reqwest::Client::new();
        runtime()
            .block_on(imdb.refresh(&http, false))
            .expect("IMDb ratings download");
    }
    *ready = true;
}

struct Live {
    store: Arc<Store>,
    client: TmdbClient,
    imdb: ImdbRatings,
}

/// A client with a fresh, empty database named `db_name`.
fn live(db_name: &str) -> Live {
    ensure_imdb();
    let paths = paths();
    let db = paths.data_dir.join(db_name);
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let store = Arc::new(Store::open(&db).unwrap());
    let client = TmdbClient::new(credential(), store.clone()).unwrap();
    let imdb = ImdbRatings::open(&paths.imdb_dir());
    assert!(imdb.is_ready());
    Live { store, client, imdb }
}

impl Live {
    async fn catalog(&self) -> ServiceCatalog {
        let movie = self.client.providers(MediaType::Movie, REGION).await.unwrap();
        let tv = self.client.providers(MediaType::Tv, REGION).await.unwrap();
        ServiceCatalog::from_lists(REGION, &movie, &tv)
    }

    async fn genres(&self) -> Vec<UnifiedGenre> {
        let movie = self.client.genres(MediaType::Movie).await.unwrap();
        let tv = self.client.genres(MediaType::Tv).await.unwrap();
        unify(&movie, &tv)
    }

    fn skip_list(&self) -> SkipList {
        SkipList {
            excluded: self.store.excluded_titles().unwrap(),
            recent: self.store.recent_picks(50).unwrap().into_iter().collect(),
        }
    }
}

fn describe(pick: &randomovie::picker::Pick) -> String {
    let d = &pick.details;
    format!(
        "{} ({}) [{}] {}",
        d.title,
        d.year.map_or("?".to_owned(), |y| y.to_string()),
        d.media.label(),
        format::imdb_badge(&pick.imdb)
    )
}

#[test]
#[ignore = "live: needs the network and TMDB_TOKEN"]
fn acceptance_one_service_free_off_30_picks() {
    let live = live("acceptance-one-service.sqlite3");
    runtime().block_on(async {
        let catalog = live.catalog().await;
        let netflix = catalog.find("Netflix").expect("Netflix in AU");
        let mut selection = ServiceSelection::default();
        selection.set_group(netflix, true);
        let services = selection.expand(&catalog);
        let service_set: HashSet<u32> = services.iter().copied().collect();
        let settings = Settings {
            include_free: false,
            ..Settings::default()
        };
        let request = PickRequest::from_settings(
            &settings,
            REGION,
            MediaChoice::Both,
            services,
            &live.genres().await,
        );
        let picker = Picker::new();
        let started = Instant::now();
        for n in 1..=30 {
            let pick = picker
                .pick(&live.client, &live.imdb, &request, &live.skip_list())
                .await
                .unwrap_or_else(|e| panic!("pick {n} failed: {e}"));
            let watch = where_to_watch(&pick.details.availability, &service_set, &catalog);
            let yours = watch
                .group(ChipGroupKind::YourServices)
                .unwrap_or_else(|| panic!("pick {n} has nothing on your services: {}", describe(&pick)));
            assert!(
                yours.chips.iter().any(|c| c.name == "Netflix"),
                "pick {n} isn't on Netflix: {}",
                describe(&pick)
            );
            assert!(matches!(watch.verdict, Verdict::Included(_)));
            assert!(now_unix() - pick.details.fetched_at < DAY, "availability older than 24 h");
            live.store
                .record_pick(pick.details.title_ref(), now_unix())
                .unwrap();
            println!("{n:2}. {}  ->  {}", describe(&pick), format::verdict(&watch.verdict, "Australia"));
        }
        println!("30 picks in {:.1} s", started.elapsed().as_secs_f32());
        let unique: HashSet<TitleRef> = live.store.recent_picks(30).unwrap().into_iter().collect();
        assert_eq!(unique.len(), 30, "no repeats within the last 50 picks");
    });
}

#[test]
#[ignore = "live: needs the network and TMDB_TOKEN"]
fn acceptance_no_services_free_on_picks_are_free() {
    let live = live("acceptance-free-only.sqlite3");
    runtime().block_on(async {
        let catalog = live.catalog().await;
        let request = PickRequest::from_settings(
            &Settings::default(),
            REGION,
            MediaChoice::Both,
            Vec::new(),
            &live.genres().await,
        );
        assert!(request.include_free);
        let picker = Picker::new();
        let mut series = 0;
        for n in 1..=20 {
            let pick = picker
                .pick(&live.client, &live.imdb, &request, &live.skip_list())
                .await
                .unwrap_or_else(|e| panic!("pick {n} failed: {e}"));
            let watch = where_to_watch(&pick.details.availability, &HashSet::new(), &catalog);
            let free = watch
                .group(ChipGroupKind::Free)
                .unwrap_or_else(|| panic!("pick {n} has no free provider: {}", describe(&pick)));
            assert!(!free.chips.is_empty());
            assert!(matches!(watch.verdict, Verdict::Free(_)));
            if pick.details.media == MediaType::Tv {
                series += 1;
            }
            live.store
                .record_pick(pick.details.title_ref(), now_unix())
                .unwrap();
            println!("{n:2}. {}  ->  {}", describe(&pick), format::verdict(&watch.verdict, "Australia"));
        }
        println!("{series} of 20 picks were series");
    });
}

#[test]
#[ignore = "live: needs the network and TMDB_TOKEN"]
fn acceptance_search_shows_rating_and_availability_fast() {
    let live = live("acceptance-search.sqlite3");
    runtime().block_on(async {
        // Warm up DNS and TLS the way the running app would have, then time a cold lookup.
        live.client.validate().await.unwrap();
        let started = Instant::now();
        let page = live.client.search("The Matrix", 1).await.unwrap();
        let first = &page.results[0];
        assert_eq!((first.id, first.media()), (603, Some(MediaType::Movie)));
        let details = live
            .client
            .details(TitleRef::new(MediaType::Movie, 603), REGION, Freshness::AllowStale)
            .await
            .unwrap();
        let rating = live.imdb.lookup(details.imdb_id.as_deref());
        let elapsed = started.elapsed();
        println!(
            "search + details + rating in {} ms: {} · {}",
            elapsed.as_millis(),
            details.title,
            format::imdb_badge(&rating)
        );
        assert!(matches!(rating, ImdbStatus::Rated(r) if r.votes > 1_000_000));
        assert!(details.availability.has_offers(), "The Matrix has AU offers");
        assert!(details.availability.link.as_deref().unwrap().contains("locale=AU"));
        assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");

        // A repeat is served from the cache.
        let started = Instant::now();
        live.client.search("The Matrix", 1).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
    });
}

#[test]
#[ignore = "live: needs the network and TMDB_TOKEN"]
fn token_is_validated() {
    let live = live("token.sqlite3");
    runtime().block_on(async {
        live.client.validate().await.expect("the .env token is valid");
        let bad = TmdbClient::new(
            Credential::Bearer("not-a-real-token".to_owned()),
            live.store.clone(),
        )
        .unwrap();
        assert_eq!(bad.validate().await, Err(ApiError::Unauthorized));
    });
}

#[test]
#[ignore = "live: needs the network and TMDB_TOKEN"]
fn imdb_refresh_is_conditional() {
    ensure_imdb();
    let imdb = ImdbRatings::open(&paths().imdb_dir());
    let meta = imdb.meta();
    assert!(meta.records > 1_000_000, "index has {} records", meta.records);
    assert!(meta.etag.is_some() || meta.last_modified.is_some());
    let outcome = runtime()
        .block_on(imdb.refresh(&reqwest::Client::new(), false))
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::NotModified);
    assert!(matches!(imdb.lookup(Some("tt0133093")), ImdbStatus::Rated(_)));

    // Lookup speed with random ids, so the small LRU rarely helps.
    let mut rng = fastrand::Rng::with_seed(5);
    let ids: Vec<String> = (0..10_000)
        .map(|_| format!("tt{:07}", rng.u32(1..15_000_000)))
        .collect();
    let started = Instant::now();
    let rated = ids
        .iter()
        .filter(|id| matches!(imdb.lookup(Some(id)), ImdbStatus::Rated(_)))
        .count();
    let per_lookup = started.elapsed() / 10_000;
    println!("10 000 random lookups: {per_lookup:?} each, {rated} rated");
    assert!(per_lookup < Duration::from_millis(1));
}

#[test]
#[ignore = "live: needs the network and TMDB_TOKEN"]
fn regions_providers_and_genres_parse() {
    let live = live("lists.sqlite3");
    runtime().block_on(async {
        let regions = live.client.regions().await.unwrap();
        assert!(regions.iter().any(|r| r.code == "AU" && r.english_name == "Australia"));
        let catalog = live.catalog().await;
        assert!(catalog.groups.len() > 40);
        for name in ["Netflix", "Stan", "SBS On Demand", "ABC iview", "Paramount Plus"] {
            assert!(catalog.find(name).is_some(), "{name} missing");
        }
        let genres = live.genres().await;
        assert!(genres.iter().any(|g| g.name == "Science Fiction" && !g.tv_ids.is_empty()));
    });
}

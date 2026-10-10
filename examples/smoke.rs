//! Prints one random pick and one search result for Australia from the live APIs.
//!
//! ```text
//! cargo run --release --example smoke
//! ```
//!
//! Reads `TMDB_TOKEN` from `.env`. Uses the app's data folders (or `RANDOMOVIE_HOME`), so the
//! IMDb ratings it downloads on first run are reused by the app.

use std::collections::HashSet;
use std::error::Error;
use std::sync::Arc;
use std::time::Instant;

use randomovie::format;
use randomovie::imdb::{ImdbRatings, ImdbStatus};
use randomovie::media::{MediaChoice, MediaType, TitleRef};
use randomovie::paths::AppPaths;
use randomovie::picker::genres::unify;
use randomovie::picker::{PickRequest, Picker, SkipList, where_to_watch};
use randomovie::services::{ServiceCatalog, ServiceSelection};
use randomovie::store::{Settings, Store};
use randomovie::tmdb::model::TitleDetails;
use randomovie::tmdb::{Credential, Freshness, TmdbClient};
use randomovie::util::time::{format_date, now_unix};

const REGION: &str = "AU";
const SEARCH: &str = "The Matrix";

fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    let token = std::env::var("TMDB_TOKEN")
        .map_err(|_| "TMDB_TOKEN is missing: copy .env.example to .env and add your token")?;
    let credential = Credential::from_secret(&token).ok_or("TMDB_TOKEN is empty")?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(credential))
}

async fn run(credential: Credential) -> Result<(), Box<dyn Error>> {
    let paths = AppPaths::from_env();
    paths.ensure_dirs()?;
    let store = Arc::new(Store::open(&paths.database_file())?);
    let client = TmdbClient::new(credential, store.clone())?;
    let imdb = ImdbRatings::open(&paths.imdb_dir());

    println!("RandoMovie smoke test, region {REGION}");
    client.validate().await?;
    println!("TMDB token: valid");

    if imdb.needs_refresh(now_unix()) {
        println!("Updating IMDb ratings (the first run downloads about 9 MB)...");
        let started = Instant::now();
        match imdb.refresh(client.http(), false).await {
            Ok(outcome) => println!("  {outcome:?} in {:.1} s", started.elapsed().as_secs_f32()),
            Err(e) => println!("  {e}; ratings will show as loading"),
        }
    }
    let meta = imdb.meta();
    if imdb.is_ready() {
        let date = meta.data_date().map(format_date).unwrap_or_default();
        println!("IMDb ratings: {} titles, data from {date}", meta.records);
    }

    let (movie_providers, tv_providers) = tokio::join!(
        client.providers(MediaType::Movie, REGION),
        client.providers(MediaType::Tv, REGION)
    );
    let catalog = ServiceCatalog::from_lists(REGION, &movie_providers?, &tv_providers?);
    let region_name = client
        .regions()
        .await?
        .into_iter()
        .find(|r| r.code == REGION)
        .map_or_else(|| REGION.to_owned(), |r| r.english_name);

    let netflix = catalog.find("Netflix").ok_or("Netflix isn't listed in this region")?;
    let mut selection = ServiceSelection::default();
    selection.set_group(netflix, true);
    let services = selection.expand(&catalog);
    let service_set: HashSet<u32> = services.iter().copied().collect();
    println!(
        "Services: {} ({} variants), plus free titles; {} services listed in {region_name}",
        netflix.name,
        services.len(),
        catalog.groups.len()
    );

    let (movie_genres, tv_genres) = tokio::join!(
        client.genres(MediaType::Movie),
        client.genres(MediaType::Tv)
    );
    let genres = unify(&movie_genres?, &tv_genres?);
    let request = PickRequest::from_settings(
        &Settings::default(),
        REGION,
        MediaChoice::Both,
        services,
        &genres,
    );
    let skip = SkipList {
        excluded: store.excluded_titles()?,
        recent: store.recent_picks(50)?.into_iter().collect(),
    };

    let started = Instant::now();
    let pick = Picker::new()
        .pick(&client, &imdb, &request, &skip)
        .await?;
    store.record_pick(pick.details.title_ref(), now_unix())?;
    println!("\nRandom pick ({:.2} s):", started.elapsed().as_secs_f32());
    print_title(&pick.details, &pick.imdb, &service_set, &catalog, &region_name);

    let started = Instant::now();
    let page = client.search(SEARCH, 1).await?;
    let first = page.results.first().ok_or("no search results")?;
    let media = first.media().ok_or("unexpected media type")?;
    let details = client
        .details(TitleRef::new(media, first.id), REGION, Freshness::AllowStale)
        .await?;
    let rating = imdb.lookup(details.imdb_id.as_deref());
    println!(
        "\nSearch \"{SEARCH}\" ({:.2} s, {} results), first result:",
        started.elapsed().as_secs_f32(),
        page.total_results
    );
    print_title(&details, &rating, &service_set, &catalog, &region_name);
    Ok(())
}

fn print_title(
    details: &TitleDetails,
    imdb: &ImdbStatus,
    services: &HashSet<u32>,
    catalog: &ServiceCatalog,
    region_name: &str,
) {
    let mut facts = vec![details.media.label().to_owned()];
    if let Some(minutes) = details.runtime_minutes {
        facts.push(format::runtime(minutes));
    }
    if let Some(seasons) = details.seasons {
        facts.push(format::seasons(seasons));
    }
    if !details.genres.is_empty() {
        facts.push(details.genres.join(", "));
    }
    let year = details.year.map(|y| format!(" ({y})")).unwrap_or_default();
    println!("  {}{year} · {}", details.title, facts.join(" · "));
    println!("  {}", format::imdb_badge(imdb));
    let watch = where_to_watch(&details.availability, services, catalog);
    println!(
        "  Where to watch in {region_name}: {}",
        format::verdict(&watch.verdict, region_name)
    );
    for group in &watch.groups {
        let names: Vec<&str> = group.chips.iter().map(|c| c.name.as_str()).collect();
        println!("    {}: {}", group.kind.label(), names.join(", "));
    }
    if let Some(link) = &watch.link {
        println!("  {link}");
    }
    println!("  Streaming data: JustWatch");
}

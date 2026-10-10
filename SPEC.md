# RandoMovie: a lightweight random movie/series picker for Windows, in Rust

## Project location and credentials (do this first)
- **Project root:** `C:\Users\shabe\Desktop\folderss\my codes\RandoMovie`. Build everything here, with `Cargo.toml` directly in this folder (no nested project folder). Create the folder if it doesn't exist; if it already contains files, don't delete or overwrite anything unrelated. The path contains a space, so quote it in every command and script. If a build step still fails because of the path, set `CARGO_TARGET_DIR` to a short path (e.g. `C:\cargo-target\randomovie`) and note it in the README.
- **TMDB credentials** for development and testing on this machine:
  - API Read Access Token (use this for every request, as `Authorization: Bearer …`): see .env (`TMDB_TOKEN`)
  - API key (v3, fallback only): see .env (`TMDB_API_KEY`)
- **Keep the credentials out of git.** Before `git init` or any commit: create `.gitignore` (at least `/target` and `.env`), save the credentials in `.env` as `TMDB_TOKEN=…` and `TMDB_API_KEY=…`, add a tracked `.env.example` with placeholders, and make sure no tracked file contains them, including `SPEC.md` (write "see .env" there instead). Never print them in logs, test output or commit messages.
- Tests and `examples/smoke.rs` load `.env` (a dev-dependency such as `dotenvy` is fine). The app looks for its token in this order: Windows Credential Manager, then the `TMDB_TOKEN` environment variable, then the first-run prompt. Never compile the token into the binary.

## How to work
You are a senior Rust engineer who ships fast, polished native Windows apps. Build the complete application below.
- Save this spec as `SPEC.md` in the project root, with the credentials removed as above. Before coding, write `PLAN.md`: module layout, data flow, and each dependency with a one-line justification.
- Build in the phases at the end. After each phase: release build succeeds, `cargo clippy -- -D warnings` and `cargo test` pass, you commit, and you stop and tell me how to check the phase before starting the next one.
- APIs and crates change: check the current official docs (Slint, TMDB, IMDb datasets, crates.io) before relying on any endpoint, field, feature flag or function, and use the latest stable versions. Never invent API fields.
- If something is ambiguous, pick what best serves "fast, light, polished", note it in `README.md`, and continue. Ask only if truly blocked.
- Simple, readable code in small modules. No stubs or TODOs in required features.

## The product
One click answers "what should I watch tonight?": the app picks a random movie or series the user can watch right now in their country, using only the streaming services they have plus anything free to stream, and shows its IMDb rating and where to watch it. A search mode answers "where can I watch X, and is it on my services?".

## Hard requirements
1. Every random pick is watchable in the user's region either (a) included with at least one of the user's selected services, or (b) free to stream (free or ad-supported) on any service. Rent/buy-only never counts. Verify each pick against availability data no older than 24 h before showing it.
2. Every title shown displays its IMDb rating and vote count, or "IMDb rating unavailable". Never present another site's score as the IMDb rating.
3. Availability is for the user's region, never global.
4. Services can be viewed and changed within one or two clicks of the main screen, and the next pick respects the change.
5. Search shows any movie or series with its IMDb rating and regional availability, clearly marking the user's services.
6. Native UI only: no Electron, Tauri, WebView2 or embedded browser.
7. Personal, non-commercial use. API credentials never appear in source code, tracked files or the binary.

## Tech stack (deviate only if something is genuinely broken, and record why)
- Rust stable, edition 2024, `x86_64-pc-windows-msvc`, Windows 10 22H2 and 11. README lists prerequisites (rustup, Visual Studio Build Tools with the C++ workload).
- UI: latest stable Slint, `.slint` files compiled by `slint-build`, Fluent style. Prefer the software renderer (least memory, no GPU context, repaints only dirty regions); use FemtoVG only if a needed visual is unsupported or scrolling stutters, and say why in `PERFORMANCE.md`. Use Slint's royalty-free licence: show the `AboutSlint` widget on an About screen reachable from the main navigation.
- Threads: UI on the main thread; one worker thread running a Tokio current-thread runtime for all network, disk, parsing and image decoding. Commands go over a channel; results come back via `slint::invoke_from_event_loop` / `Weak::upgrade_in_event_loop`. The UI thread never waits on I/O.
- HTTP: one shared `reqwest` client, default features off, only JSON and gzip, Windows-native TLS (SChannel) instead of a bundled crypto library. 10 s timeouts, max ~6 concurrent requests, de-duplicated in-flight requests, exponential backoff on 429/5xx.
- Storage: SQLite via `rusqlite` (bundled, small page cache) for cached responses, pick history and exclusions. Settings in `%APPDATA%\RandoMovie\settings.json`; cache, images, IMDb data and logs in `%LOCALAPPDATA%\RandoMovie\`.
- Images: `image` crate with only JPEG and PNG. Decode and downscale on the worker to the exact physical pixels displayed; send a `SharedPixelBuffer` to the UI thread and create the `slint::Image` there.
- `windows` crate (minimal features) for region detection, title-bar theming and Credential Manager. Also `serde`/`serde_json` (deserialize only fields you use), `flate2`, `fastrand`. No crate for anything std or an existing dependency already does.

## Data sources
**TMDB API** (Read Access Token from the credentials section, sent as `Authorization: Bearer …`): metadata, posters, search, and availability (sourced from JustWatch).
- Regions: `/3/watch/providers/regions`. Services per region: `/3/watch/providers/movie` and `/3/watch/providers/tv` with `watch_region`; merge by `provider_id`, order by the region's display priority.
- Candidates: `/3/discover/movie` and `/3/discover/tv` with `watch_region`, `with_watch_providers` (pipe-separated = OR), `with_watch_monetization_types`, `include_adult=false`, `vote_count.gte` and user filters. Movie and TV genre IDs differ (`/3/genre/movie/list`, `/3/genre/tv/list`); map each chosen genre per type.
- Details + availability + IMDb ID in one call: `/3/movie/{id}?append_to_response=watch/providers,external_ids` (same for `/3/tv/{id}`). `watch/providers.results[XX]` holds `flatrate`, `free`, `ads`, `rent`, `buy`, `link`.
- Search: `/3/search/multi`, keeping `movie` and `tv` results. Metadata in `language=en-US`.
- Images: `https://image.tmdb.org/t/p/{size}{path}`, always the smallest size covering display size × DPI scale (e.g. `w92` thumbnails/logos, `w342`/`w500` hero poster, `w300` backdrop).

**IMDb ratings**: IMDb's official non-commercial dataset `https://datasets.imdbws.com/title.ratings.tsv.gz` (`tconst`, `averageRating`, `numVotes`), no API key.
- Download on first run, then in the background when the local copy is over 7 days old, with conditional requests (ETag / If-Modified-Since). Until it's ready, show "Loading IMDb ratings…" and fill in later.
- Stream-decompress into a compact binary index of fixed-size records (numeric `tconst` as u32, rating × 10 as u8, votes as u32), sorted by numeric ID. Text order isn't numeric order (IDs have 7 or 8 digits), so sort after parsing. Keep the rebuild's peak memory modest, free it afterwards, and swap in the new index atomically.
- Look up by binary search with positional file reads (never load the table into RAM), plus a small LRU of recent lookups.
- IMDb ID comes from TMDB (`imdb_id` for movies, `external_ids.imdb_id` for TV); no ID or no row → "IMDb rating unavailable".

**Attribution** (About screen, plus a small line under availability): TMDB logo and "This product uses the TMDB API but is not endorsed or certified by TMDB."; "Streaming data: JustWatch" beside availability (required by TMDB); IMDb credited as the ratings source per its non-commercial terms; Slint's `AboutSlint` widget.

## Random pick algorithm
R = user's region. S = user's selected services, expanded to include their variants (see My Services). "Free" = monetization `free` or `ads`.
1. Pools. A: discover with `with_watch_providers` = S and `with_watch_monetization_types=flatrate|free|ads`. B (only if "Include free titles" is on, the default): discover with `with_watch_monetization_types=free|ads` and no provider filter; confirm via the docs and a live call that this works with `watch_region` alone, and if not, flag it in `PLAN.md` with your best alternative. Respect the Movies / Series / Both toggle.
2. Uniform randomness. Fetch page 1 once per (region, services, filters, type) to learn `total_results`; cache it for the session and invalidate on any change. Choose a pool weighted by size, draw a random index below min(`total_results`, 10,000) (TMDB stops at page 500), fetch that page and take that item (handle a short last page).
3. Verify (this is the guarantee). Fetch details and providers. Accept only if, in R, `flatrate ∪ free ∪ ads` contains a provider in S, or free titles are on and `free ∪ ads` is non-empty. Then post-filter: minimum IMDb rating and votes, not marked "Seen it"/"Not interested", not in the last 50 picks. On a miss, try the rest of that page (shuffled), then a new random page; after ~10 misses show "Nothing matches, try looser filters".
4. With a minimum IMDb rating set, add `vote_average.gte` about one point lower to discover, to cut wasted lookups.
5. After showing a pick, prefetch and verify the next one in the background so "Pick again" is instant. Steady state ≈ 3 requests per pick (page, details, poster), fewer when cached.

## Screens
Slim left navigation rail: Pick, Search, My Services; Settings (with About) at the bottom.

**Pick (home)**
- Large title card (below) with a prominent "Pick for me" / "Pick again" button.
- Filter bar: Movies / Series / Both; genres; minimum IMDb rating (default 6.0) and votes (default 1,000), each allowing "Any"; year range; maximum runtime (movies).
- A compact row of the active services' logos with an Edit button that opens the services panel as a side sheet, without leaving the screen.
- "← Previous" steps back through this session's picks; "Seen it" and "Not interested" exclude a title from future picks.

**Title card (shared by Pick and Search)**
- Poster, title, year, Movie/Series badge, runtime or seasons, genres, 3-line overview with "More".
- IMDb badge, e.g. "★ 7.8 IMDb · 1.2M votes".
- "Where to watch in {region name}": a verdict line first ("Included with Netflix", "Free on SBS On Demand", "Not on your services"), then logo chips grouped as On your services (highlighted), Free, Other subscriptions, Rent/Buy (collapsed).
- "Open where to watch" opens the TMDB watch page (`link`) in the default browser.
- A soft, darkened, blurred backdrop behind the card, generated once on the worker from the `w300` backdrop.

**Search**: 300 ms debounce, stale responses ignored; results with thumbnail, title, year and type badge; keyboard navigable; "Load more". Selecting a result opens the title card.

**My Services**
- Grid of logo tiles with names and on/off state, a filter box, and "Show all" for the long tail; most popular in the region first. Changes save instantly.
- One tile per service, grouping TMDB's variants (e.g. "Netflix" also covers "Netflix basic with Ads"; "Paramount Plus" covers its Apple TV Channel and Amazon Channel versions) by normalised name, with suffixes such as "with Ads", "Amazon Channel", "Apple TV Channel", "Basic" and "Standard" stripped. Users can expand a tile to toggle variants individually.
- "Include free titles" switch, explained in one line (free/ad-supported services such as SBS On Demand, ABC iview, 7plus and 9Now in Australia count even if not selected).
- With nothing selected, picks come from free titles only (or, if free titles are off, Pick asks the user to choose services).

**Settings and first run**
- First run: if no token is found (Credential Manager, then `TMDB_TOKEN`), ask for it (link to TMDB's API page), validate it with a test call and store it in Credential Manager. Then confirm region and choose services.
- Region auto-detected from Windows (`GetUserDefaultGeoName`), overridable from TMDB's region list; fall back to AU.
- Theme (System / Dark / Light), default media type, include free titles, obscurity floor (minimum TMDB votes, default 50), cache size with "Clear cache", IMDb data date with "Refresh now", About.
- The token lives in Windows Credential Manager, never in the app's settings file, tracked files or logs.

## Look and feel
- Modern Windows 11 look: Fluent style, dark by default, title bar themed to match (`DwmSetWindowAttribute` with `DWMWA_USE_IMMERSIVE_DARK_MODE` if Slint doesn't already).
- Tokens: dark surfaces around #0F1115 / #171A21 / #1F2430, text #F2F4F8 / #A3AAB8, one accent colour (the Windows accent if contrast allows); 8 px grid; 12 px radius on cards, 8 px on controls; subtle elevation.
- System fonts (Segoe UI Variable, falling back to Segoe UI) and the system icon font (Segoe Fluent Icons, falling back to Segoe MDL2 Assets); don't bundle fonts or icon packs.
- Skeleton placeholders while loading; inline, non-blocking errors with Retry; friendly empty states.
- Transitions ≤ 200 ms and only on user action; nothing animates while idle.
- Usable down to 800 × 560, remembers size and position, crisp at 100–200 % scaling.
- Keyboard: Space/Enter pick again, Ctrl+F search, Esc back, arrows in lists. Visible focus, accessible names, WCAG AA contrast.

## Performance budgets (measure on a release build; report numbers and method in `PERFORMANCE.md`)
- 0 % CPU when idle: no polling, timers or animations.
- Private working set under 50 MB after viewing ~20 titles: LRU-bounded decoded images, released for screens not shown.
- Window interactive within 0.5 s of launch (render from cached settings; network afterwards).
- Single portable `RandoMovie.exe` under 15 MB; no console window (`#![windows_subsystem = "windows"]`); icon and version info embedded.
- `[profile.release]`: `lto = "fat"`, `codegen-units = 1`, `strip = true`, `opt-level = 3`.
- Cache lifetimes: provider lists 7 days; details/availability 24 h; images 30 days, capped at 150 MB with LRU eviction.
- Closing the window exits the process: no tray icon, background service, autostart or telemetry.

## Reliability
Offline → cached data plus an offline banner, resuming automatically. TMDB 401 → open token settings; 429 → back off; timeouts/5xx → retry, then an inline error. Missing poster, IMDb ID or availability renders gracefully. Errors and panics go to a small size-capped log file; nothing fails silently.

## Tests and acceptance
- Unit tests on recorded JSON fixtures (no network): eligibility rule, variant grouping, random index → page/offset maths (500-page cap, short last page), IMDb index build and lookup, cache expiry, settings migration.
- An `#[ignore]`d integration test against the real APIs using `TMDB_TOKEN` from `.env`. You have a working token, so run the live tests and acceptance checks yourself at the end of every phase that touches the APIs.
- Acceptance checks, results in the README:
  - Region AU, one service selected, free titles off: 30 consecutive picks all show that service under "On your services".
  - No services selected, free titles on: every pick shows a Free provider.
  - Searching a well-known film shows its IMDb rating and AU availability within about a second on a normal connection.
  - Idle 60 s with a pick on screen: 0 % CPU, memory within budget.

## Deliverables and phases
Cargo project with a clear module split (e.g. `ui/`, `tmdb/`, `imdb/`, `picker/`, `store/`, `platform/`), plus `README.md` (setup including copying `.env.example` to `.env` with a TMDB token, build, run, architecture, assumptions), `PERFORMANCE.md`, `SPEC.md` and `PLAN.md`.
1. Data layer: TMDB client, IMDb pipeline, store, eligibility and picker logic, with tests and `examples/smoke.rs` printing one pick and one search result for AU.
2. UI shell: navigation, theming, settings, first-run flow, My Services.
3. Pick screen: verification, filters, history, exclusions, prefetch.
4. Search and the shared title card.
5. Polish: accessibility, empty/error states, performance measurement and tuning, final README.

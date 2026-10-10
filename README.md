# RandoMovie

RandoMovie answers "what should I watch tonight?" with one click. It picks a random movie or
series that you can stream right now in your country, on the services you have or for free,
and shows its IMDb rating and where to watch it. A search mode answers "where can I watch X,
and is it on my services?".

It is a native Windows app written in Rust with a Slint UI. It is for personal,
non-commercial use.

## Status

| Phase | Scope | State |
|---|---|---|
| 1 | Data layer: TMDB client, IMDb pipeline, storage, eligibility and picker | Done |
| 2 | UI shell: navigation, theming, settings, first-run flow, My Services | Done |
| 3 | Pick screen: verification, filters, history, exclusions, prefetch | Next |
| 4 | Search and the shared title card | Planned |
| 5 | Polish, accessibility, performance measurement, final README | Planned |

The app opens, runs the first-run flow, and has the navigation rail, My Services, Settings and
About. The Pick and Search pages say which phase brings them.

## Prerequisites

- Windows 10 22H2 or Windows 11, x64.
- [rustup](https://rustup.rs) with the stable `x86_64-pc-windows-msvc` toolchain.
  Rust 1.92 or newer is required; development used 1.99.0.
- Visual Studio Build Tools 2022 with the **C++ build tools** workload, which provides the MSVC
  linker, the resource compiler and the Windows SDK. Install it from the Visual Studio website
  or with:

  ```powershell
  winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```

- A TMDB account and its API Read Access Token, from <https://www.themoviedb.org/settings/api>.

## Setup

1. Copy `.env.example` to `.env` in the project root.
2. Put your API Read Access Token after `TMDB_TOKEN=`. The v3 key line is optional.

`.env` is git-ignored and is read only by the tests and the smoke example. The app itself looks
for its token in Windows Credential Manager first, then the `TMDB_TOKEN` environment variable,
then asks on first run. The token is never compiled into the binary.

The project path contains a space. The build handles it without help, so `CARGO_TARGET_DIR`
isn't needed.

## Build, run and test

```powershell
cargo run --release                          # build and start the app
cargo run --release --example smoke          # one random AU pick and one AU search result
cargo test                                   # unit tests, no network
cargo clippy --all-targets -- -D warnings    # lints
cargo test --release --test live -- --ignored --test-threads=1 --nocapture   # live checks
```

The portable app is `target\release\RandoMovie.exe`. It needs nothing else installed.

The live tests call TMDB and IMDb with the token from `.env`. They keep their data in
`target\tmp\live`, so they never touch the app's real folders.

## First run

1. **Token.** If no token is saved, the app asks for the TMDB API Read Access Token, checks it
   with TMDB, and keeps it in Windows Credential Manager under `RandoMovie/TMDB`.
2. **Region.** The country comes from Windows' region settings and can be changed from TMDB's list.
3. **Services.** Turn on the services you pay for. Free titles are included unless you switch
   them off.

Afterwards, Settings can change the region, theme, default type, free titles and obscurity
floor, clear the cache, refresh the IMDb ratings, and change or remove the token.

## Where data lives

| What | Where |
|---|---|
| Settings | `%APPDATA%\RandoMovie\settings.json` |
| TMDB token | Windows Credential Manager, generic credential `RandoMovie/TMDB` |
| Cache database: API responses, images, pick history, exclusions | `%LOCALAPPDATA%\RandoMovie\cache.sqlite3` |
| IMDb ratings index and its metadata | `%LOCALAPPDATA%\RandoMovie\imdb\` |
| Log, capped at about 1 MB | `%LOCALAPPDATA%\RandoMovie\logs\` |

Setting `RANDOMOVIE_HOME` moves the settings and data folders under one directory, which is
useful for testing. The smoke example writes to the real folders, so the IMDb ratings it
downloads are reused.

## Architecture

`PLAN.md` has the module layout, the data flow and every dependency with its reason. In short:

- **Threads.** The Slint UI runs on the main thread. One worker thread runs a current-thread
  Tokio runtime for all network, disk, parsing and image decoding. The UI sends commands over a
  channel and never waits; results come back through the Slint event loop.
- **TMDB client.** One shared reqwest client using SChannel TLS. It caches every response in
  SQLite with a lifetime per endpoint, allows six requests at a time, merges identical
  in-flight requests, and retries 429, 5xx and timeouts with exponential backoff.
- **IMDb ratings.** The official dataset streams through gzip into a 9-byte-per-title index,
  sorted with an external merge sort. Lookups binary-search the file with positional reads.
- **Picker.** It samples uniformly from TMDB discover results for your services and for free
  titles. It then verifies each candidate against availability data under 24 hours old before
  returning it.
- **Storage.** SQLite holds the caches, history and exclusions. Settings are versioned JSON,
  saved atomically.

## Assumptions and decisions

The spec left these points open. Each choice favours fast, light and predictable behaviour.

- **Service grouping.** Besides the spec's suffixes ("with Ads", "Amazon Channel",
  "Apple TV Channel", "Basic", "Standard"), the tier words "Premium" and "Kids" are stripped,
  so "Paramount Plus Premium" and "Netflix Kids" join their services. "YouTube Premium" stays
  separate from "YouTube". A service with a single variant keeps its full name, such as
  "MGM Amazon Channel".
- **My Services.** The 24 most popular services show first, plus any selected service further
  down; "Show all" reveals the rest. The filter matches service and plan names.
- **Selections survive region changes.** Services are stored by normalised name, plus any
  variants switched off individually.
- **Region detection.** Windows' "Country or region" comes first. If TMDB has no streaming data
  for it, the regional format's country is used (en-AU gives Australia), then Australia. The
  development machine needed this: its "Country or region" is Sint Maarten.
- **Free titles.** "Free" means JustWatch's `free` and `ads` types. That includes library
  services such as Beamafilm and Hoopla, which JustWatch lists as free. The one-line hint names
  SBS On Demand, ABC iview, 7plus and 9Now in Australia and stays generic elsewhere.
- **Theme and accent.** Dark is the default. The Windows accent colour is used when a shade of it
  passes WCAG AA contrast in the current theme; otherwise a built-in blue is used.
- **Default type.** The setting decides where the Movies / Series / Both filter starts each time
  the app opens.
- **Obscurity floor.** Settings offers Any, 25, 50 (default), 100, 250, 500 and 1,000 TMDB votes.
  A different value typed into `settings.json` is kept and shown as an extra choice.
- **Pool sizes.** TMDB now reports at most 20,001 results and serves at most 500 pages, so a
  pool's reachable size is capped at 10,000 titles, the most popular first.
- **Misses.** A miss is a candidate that fails verification or a filter. Excluded and recently
  picked titles are skipped before any request and don't count. A pick also stops after 10
  page fetches, so it always ends quickly.
- **IMDb minimums.** With a minimum set, a title needs an IMDb rating that meets it, so titles
  without one are skipped. While the ratings are still downloading, this check is skipped and
  TMDB's `vote_average.gte` pre-filter still applies.
- **Genres.** One list covers movies and series. TV genres such as "Action & Adventure" count
  for both parts, and "Sci-Fi" equals "Science Fiction". Several genres mean any of them. If a
  media type has no matching genre, such as Horror for series, only the other type is picked.
- **Filters.** The year range uses the release date for movies and the first air date for
  series. The runtime limit applies to movies only, and an unknown runtime passes.
- **Cache lifetimes the spec didn't set.** Discover pages and searches last 24 hours. Genre and
  region lists last 7 days. Cached responses older than 30 days are deleted at startup.
- **Offline.** Browsing falls back to expired cached data. Picks never do, because each one
  must be verified with availability data under 24 hours old.
- **Verdict line.** A title with no offers at all in the region reads "Not available to stream
  in {region}". Each service appears once in the chips, in the first group that applies.
- **Credentials.** The token check accepts either a v4 read access token or a 32-character v3
  key. The app, tests and examples use the token.
- **Hand-written settings.** `settings.json` without a `version` field may list services by
  name, such as `"services": ["Netflix", "Stan"]`, and may put `min_imdb_rating` at the top
  level. It is migrated to the current layout on load.
- **Release profile.** `panic = "abort"` is added to the required release settings. It removes
  about 4.4 MB of unwinding tables; panics are still written to the log.
- **Repaint workaround.** Slint 1.18.1's software renderer can leave areas unpainted after Windows
  discards window content, for example after a snap or a restore. The app forces a full repaint
  when Windows reports such an area, mirroring Slint's unreleased fix (PLAN.md has details).

## Acceptance checks

Phase 1 checks ran at the data-layer level on 2026-10-10, region AU. They will be repeated
through the UI in phase 3.

| Check | Result |
|---|---|
| One service (Netflix), free titles off: 30 consecutive picks show it under "On your services" | Pass: 30 of 30 "Included with Netflix", no repeats |
| No services, free titles on: every pick shows a Free provider | Pass: 20 of 20, 5 of them series |
| Search a well-known film for its IMDb rating and AU availability in about a second | Pass: "The Matrix", ★ 8.7 IMDb · 2.3M votes, AU offers, 511 ms cold |
| Idle 60 s with a pick on screen: 0 % CPU, memory within budget | Needs the Pick screen (phases 3 and 5) |

The live tests above reproduce the first three checks.

## Attribution

- This product uses the TMDB API but is not endorsed or certified by TMDB.
- Streaming availability data is provided by JustWatch through TMDB.
- Information courtesy of IMDb (<https://www.imdb.com>). Used with permission. IMDb ratings come
  from IMDb's non-commercial datasets, for personal and non-commercial use.
- The UI is built with Slint under its royalty-free licence; the About screen shows Slint's
  `AboutSlint` credit.

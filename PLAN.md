# RandoMovie implementation plan

This plan covers the module layout, the data flow, and every dependency with a one-line reason.
It was written before coding and is updated when a decision changes.

## Toolchain

- Rust stable 1.99, edition 2024, target `x86_64-pc-windows-msvc`.
- Visual Studio Build Tools 2022 with the "C++ build tools" workload (MSVC linker, Windows SDK).
  `rusqlite` compiles SQLite from source, so the C compiler is needed too.

## Crate layout

The crate is a library plus one binary.
The library holds everything that can be tested without a window; the binary holds the Slint UI.

```
Cargo.toml, build.rs            build.rs compiles ui/*.slint (slint-build) and embeds icon + version info
src/
  lib.rs                        public modules of the data layer
  main.rs                       binary entry: logging, settings, window, worker, event loop   (phase 2)
  error.rs                      Error / ApiError enums, all Clone + Display, no secrets inside
  paths.rs                      %APPDATA%\RandoMovie and %LOCALAPPDATA%\RandoMovie locations
  format.rs                     "★ 7.8 IMDb · 1.2M votes", runtimes, verdict text
  services.rs                   provider merge, variant grouping by normalised name, selection expansion
  util/
    lru.rs                      tiny bounded LRU map (IMDb lookups, decoded images)
    time.rs                     unix time and RFC 3339 / HTTP-date formatting without a date crate
    log.rs                      size-capped log file, panic hook, never logs credentials
  tmdb/
    client.rs                   shared reqwest client: auth, cache, 6-request limiter, in-flight
                                de-duplication, retry with exponential backoff, offline state
    model.rs                    serde types with only the fields we read
    query.rs                    discover query builder (pools A and B, filters, genres per type)
    images.rs                   smallest TMDB size covering the displayed pixels
  imdb/
    index.rs                    fixed 9-byte records, external-sort builder, binary-search reader, LRU
    dataset.rs                  conditional download, stream decompression, atomic swap, metadata
  picker/
    eligibility.rs              the availability guarantee and the chip grouping for the title card
    sampling.rs                 weighted pool choice, random index to (page, offset)
    genres.rs                   unified genre list mapped to movie and TV genre ids
    engine.rs                   pick loop: pools, draw, verify, post-filter, retry budget
  store/
    db.rs                       SQLite connection (WAL, 1 MB page cache) and schema
    cache.rs                    HTTP response cache with TTLs, image cache with 30-day TTL and 150 MB LRU cap
    history.rs                  pick history and "Seen it" / "Not interested" exclusions
    settings.rs                 settings.json load/save, versioned migration, sanitising
  platform/                     (phase 2)
    region.rs                   GetUserDefaultGeoName with AU fallback
    credentials.rs              Windows Credential Manager read/write/delete
    theme.rs                    dark title bar via DwmSetWindowAttribute, Windows accent colour
    shell.rs                    open the TMDB watch page in the default browser (ShellExecuteW)
    network.rs                  connectivity-change notification to leave offline mode without polling
  worker/                       (phase 2)
    mod.rs                      worker thread, current-thread Tokio runtime, command loop
    commands.rs                 Command (UI to worker) and Update (worker to UI) enums
    images.rs                   decode + downscale to exact pixels, blurred backdrop, SharedPixelBuffer
  ui/                           (phase 2+, binary only)
    *.slint                     app, theme tokens, nav rail, pick, title card, search, services, settings, about
    bridge.rs                   applies Updates to Slint properties and models on the UI thread
examples/smoke.rs               prints one AU pick and one AU search result from the live APIs
tests/fixtures/*.json           recorded TMDB responses (no credentials inside)
tests/live.rs                   #[ignore]d live checks against TMDB and IMDb
```

## Threads and data flow

```
 UI thread (Slint event loop)                       Worker thread (Tokio current-thread runtime)
 ───────────────────────────                        ─────────────────────────────────────────────
 callback ── Command ──► mpsc::UnboundedSender ───► recv loop ─► spawn_local(task)
                                                     task: TmdbClient / ImdbRatings / Store / Picker
 apply(Update) ◄── Weak::upgrade_in_event_loop ◄──── Update { plain data, SharedPixelBuffer }
```

- The UI thread never touches the network, disk or SQLite. Sending a command never blocks.
- Every result carries the request generation it answers, so stale results (search typing, a pick
  superseded by a filter change) are dropped on arrival.
- `slint::Image` values are created on the UI thread from `SharedPixelBuffer`s decoded on the worker.
- Reqwest's resolver uses short-lived blocking threads for DNS. That is internal to reqwest; all of
  our own work stays on the one worker thread.

## Pick flow

1. Build the pools for region R and the expanded service set S:
   pool A per media type when S is not empty, pool B per media type when free titles are on.
   Genres are mapped per type; a type with no matching genre is skipped.
2. Pool sizes come from page 1 of each discover query, cached for the session by the full query
   string, so any change to region, services, filters or type yields a new key.
3. Draw a global index below the sum of `min(total_results, 10 000)`, map it to a pool and to
   `(page, offset)` with 20 results per page, and fetch that page (cached 24 h).
4. Verify: fetch details with `append_to_response=watch/providers,external_ids`, using cached data
   only if it is under 24 h old. Accept when `flatrate ∪ free ∪ ads` in R meets S, or free titles
   are on and `free ∪ ads` in R is non-empty. Then apply the IMDb minimums, the exclusions and the
   last 50 picks. Excluded and recent titles are skipped before any request.
5. On a miss, walk the rest of the page in shuffled order, then draw a new page. After 10 misses the
   picker returns "Nothing matches, try looser filters".
6. The worker prefetches and verifies the next pick after each shown pick, and re-verifies a
   prefetched pick whose data has passed 24 h before showing it.

### Pool B check (spec step 1)

Confirmed with live calls on 2026-10-10 that `with_watch_monetization_types=free|ads` works with
`watch_region` alone. AU movie page 3 shared no titles with the unfiltered list, and every sampled
title had a `free` or `ads` offer in AU. TMDB caps the reported `total_results` at 20 001 and
rejects `page > 500`, so the 10 000-item cap in step 2 is what bounds the draw. No alternative is
needed.

## Caching

| Data | Where | Lifetime |
|---|---|---|
| Regions, provider lists, genre lists | SQLite `http_cache` | 7 days |
| Details + availability, discover pages, search | SQLite `http_cache` | 24 hours |
| Images (posters, logos, backdrops) | SQLite `image_cache` | 30 days, 150 MB LRU cap |
| Pool sizes | worker memory | session, keyed by the full query |
| IMDb lookups | worker memory, LRU of 512 | session; cleared when the index is swapped |
| Decoded images | UI thread LRU | bounded; released when a screen is hidden |

When offline, expired cache entries are served for browsing with an offline banner.
Picks are never verified with stale availability.

## IMDb pipeline

1. On first run, or when the last check is over 7 days old, GET the dataset with `If-None-Match`
   and `If-Modified-Since`. A 304 only updates the check time.
2. A 200 streams through `flate2::write::GzDecoder` into a line parser while chunks arrive.
   Records of 9 bytes (`u32` id, `u8` rating × 10, `u32` votes) collect in 256 K-record chunks.
   Each chunk is sorted and spilled to a temporary run file, then the runs are k-way merged into
   the final file. Peak memory stays near 3 MB.
3. The new index is written beside the old one, flushed, and swapped under the index lock by
   closing the old handle and renaming. Lookups see either the old or the new file, never a mix.
4. Lookups binary-search with positional reads (`FileExt::seek_read`) and read only the final
   small window, so the table is never loaded into RAM.

## Settings and secrets

- `%APPDATA%\RandoMovie\settings.json` has a `version` field. Unversioned files migrate to the
  current version, missing fields take defaults, out-of-range values are clamped, and an unreadable
  file is kept as `settings.json.bak` before defaults are used.
- The TMDB token is read from Windows Credential Manager, then `TMDB_TOKEN`, then the first-run
  prompt. It is never written to settings, logs, cache keys or the binary.
- The token is only sent to `api.themoviedb.org`. Image and IMDb requests carry no credentials.

## Dependencies

| Crate | Version | Features | Why |
|---|---|---|---|
| `slint` | 1.18.1 | `std`, `compat-1-2`, `backend-winit`, `renderer-software`, `accessibility`, `raw-window-handle-06` | Native UI with the software renderer, accessibility, and the window handle for title-bar theming |
| `slint-build` | 1.18.1 | (build) | Compiles `.slint` files at build time with the Fluent style |
| `reqwest` | 0.13.5 | no defaults; `json`, `gzip`, `native-tls` | One async HTTP client using SChannel through native-tls, so no bundled crypto |
| `tokio` | 1.53.2 | `rt`, `sync`, `time`, `macros` | Current-thread runtime, semaphore, OnceCell, channels and timers for the worker |
| `rusqlite` | 0.40.2 | `bundled` | SQLite for caches, history and exclusions with no system dependency |
| `serde` | 1.0.229 | `derive` | Typed deserialization of only the fields we read |
| `serde_json` | 1.0.151 | `raw_value` | JSON parsing; `raw_value` skips the other 95 regions in watch-provider results |
| `flate2` | 1.1.10 | default (miniz_oxide) | Streaming gzip decompression of the IMDb dataset |
| `fastrand` | 2.5.0 | default | Small seeded RNG for uniform picks and shuffles |
| `image` | 0.25.10 | no defaults; `jpeg`, `png` | Decode and resize posters and logos; no rayon threads, no unused codecs |
| `windows` | 0.62.2 | `Win32_Foundation`, `Win32_Globalization`, `Win32_Security_Credentials`, `Win32_Graphics_Dwm`, `Win32_UI_Shell`, `Win32_UI_WindowsAndMessaging`, `Win32_System_Registry`, `Win32_NetworkManagement_IpHelper`, `Win32_Networking_WinSock` | Region, Credential Manager, dark title bar, accent colour, opening links, connectivity events |
| `raw-window-handle` | 0.6.2 | default | Reads the HWND from Slint's window handle for `DwmSetWindowAttribute` |
| `winresource` | 0.1.31 | (build) | Embeds the icon and version info from Cargo metadata into the exe |
| `dotenvy` | 0.15.7 | (dev) | Loads `.env` for tests and the smoke example only |

Phase 1 uses only reqwest, tokio, rusqlite, serde, serde_json, flate2 and fastrand.
The UI and platform crates arrive in phase 2, and `image` arrives in phase 3.

Not used on purpose: no `chrono` (std time plus a small formatter), no `thiserror` or `anyhow`
(small manual error enums), no `log` facade (one file logger), no `futures` (tokio covers it),
no `tempfile` (tests use unique folders under the system temp dir).

## Testing

- Unit tests on the recorded fixtures cover: the eligibility rule and chip grouping, variant
  grouping on the real AU provider list, random index to page/offset maths (500-page cap, short
  last page), IMDb index build and lookup (7- and 8-digit ids, multi-run merge, gzip input),
  cache expiry and image LRU eviction, settings migration, genre mapping, and the pick loop
  against a fake catalog with a seeded RNG.
- `tests/live.rs` holds `#[ignore]`d live tests using `TMDB_TOKEN` from `.env`, including the
  acceptance checks for single-service picks, free-only picks and search speed.
- Every phase ends with a release build, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
  the live tests, and a commit.

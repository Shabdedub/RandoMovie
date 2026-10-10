# RandoMovie implementation plan

This plan covers the module layout, the data flow, and every dependency with a one-line reason.
It was written before coding and is updated when a decision changes.

## Toolchain

- Rust stable 1.99, edition 2024, target `x86_64-pc-windows-msvc`.
- Visual Studio Build Tools 2022 with the "C++ build tools" workload (MSVC linker, Windows SDK).
  `rusqlite` compiles SQLite from source, so the C compiler is needed too.

## Crate layout

The crate is a library plus one binary.
The library holds everything that can be tested without a window, including the Windows API
wrappers. The binary holds the worker thread and the Slint UI.

```
Cargo.toml, build.rs            build.rs compiles ui/*.slint (slint-build, Fluent) and embeds icon + version info
assets/                         app icon (.ico and 64 px PNG), TMDB logo (SVG, from TMDB's logo page)
ui/                             Slint UI, compiled into the binary
  app.slint                     window: onboarding or rail + page, side sheet, token dialog, repaint layer
  theme.slint                   design tokens (surfaces, text, accent, radii, type ramp) and icon glyphs
  state.slint                   enums, model structs and the globals Rust reads and writes
  widgets.slint                 buttons, links, cards, segmented control, chips, filter buttons, skeletons
  card.slint                    the title card shared by Pick and Search, and its placeholder
  nav.slint, services.slint, token.slint, pages/*.slint
src/
  lib.rs                        public modules of the data layer
  main.rs                       binary entry: DPI awareness, logging, then ui::run
  ui/mod.rs                     backend selection, theming, window events, callback wiring  (binary)
  ui/controller.rs              UI-thread state: settings, models, logos, commands to the worker (binary)
  ui/controller/pick.rs         Pick screen: requests, the card on screen, its images, filter bar (binary)
  worker/mod.rs                 worker thread, current-thread Tokio runtime, command handlers (binary)
  worker/pick.rs                pick requests, the prepared next pick, session history, exclusions (binary)
  worker/messages.rs            Command (UI to worker) and Update (worker to UI)            (binary)
  worker/images.rs              decode, scale to exact pixels, round corners, blur backdrops (binary)
  card.rs                       the title card's display text and where-to-watch groups
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
    filters.rs                  filter bar steps, summaries and year-box validation
  store/
    db.rs                       SQLite connection (WAL, 1 MB page cache) and schema
    cache.rs                    HTTP response cache with TTLs, image cache with 30-day TTL and 150 MB LRU cap
    history.rs                  pick history and "Seen it" / "Not interested" exclusions
    settings.rs                 settings.json load/save, versioned migration, sanitising
  platform/                     Windows APIs (windows crate)
    region.rs                   country: GetUserDefaultGeoName, then the regional format, then AU
    credentials.rs              Windows Credential Manager read/write/delete
    theme.rs                    light/dark preference, accent palette with WCAG checks, title bar (DWM)
    monitor.rs                  per-monitor DPI, primary work area, is a saved position on screen
    repaint.rs                  GetUpdateRect, for the software-renderer repaint workaround
    shell.rs                    open web links in the default browser (ShellExecuteW)
    network.rs                  connectivity-change notification to leave offline mode without polling
    fonts.rs                    Segoe UI Variable or Segoe UI, Segoe Fluent Icons or MDL2 Assets
    dialog.rs                   error box for failures before the window exists
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
   picker returns "Nothing matches, try looser filters". Candidates are verified three at a time
   (phase 3); the pick is still the first one in this order that passes, so the odds don't change.
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
| `slint` | 1.18.1 | `std`, `compat-1-2`, `backend-winit`, `renderer-software`, `accessibility`, `unstable-winit-030` | Native UI with the software renderer and accessibility; the winit hook themes the window before it exists and exposes the HWND and window events |
| `slint-build` | 1.18.1 | (build) | Compiles `.slint` files at build time with the Fluent style |
| `reqwest` | 0.13.5 | no defaults; `json`, `gzip`, `native-tls` | One async HTTP client using SChannel through native-tls, so no bundled crypto |
| `tokio` | 1.53.2 | `rt`, `sync`, `time`, `macros` | Current-thread runtime, semaphore, OnceCell, channels and timers for the worker |
| `rusqlite` | 0.40.2 | `bundled` | SQLite for caches, history and exclusions with no system dependency |
| `serde` | 1.0.229 | `derive` | Typed deserialization of only the fields we read |
| `serde_json` | 1.0.151 | `raw_value` | JSON parsing; `raw_value` skips the other 95 regions in watch-provider results |
| `flate2` | 1.1.10 | default (miniz_oxide) | Streaming gzip decompression of the IMDb dataset |
| `fastrand` | 2.5.0 | default | Small seeded RNG for uniform picks and shuffles |
| `image` | 0.25.10 | no defaults; `jpeg`, `png` | Decode and resize posters and logos; no rayon threads, no unused codecs |
| `windows` | 0.62.2 | `Win32_Foundation`, `Win32_Globalization`, `Win32_Graphics_Dwm`, `Win32_Graphics_Gdi`, `Win32_NetworkManagement_IpHelper`, `Win32_Networking_WinSock`, `Win32_Security_Credentials`, `Win32_System_Com`, `Win32_System_Registry`, `Win32_UI_HiDpi`, `Win32_UI_Shell`, `Win32_UI_WindowsAndMessaging` | Region, Credential Manager, title bar, accent colour, monitors and DPI, repaint detection, opening links, connectivity events, error box |
| `winresource` | 0.1.31 | (build) | Embeds the icon and version info from Cargo metadata into the exe |
| `dotenvy` | 0.15.7 | (dev) | Loads `.env` for tests and the smoke example only |

Phase 1 uses only reqwest, tokio, rusqlite, serde, serde_json, flate2 and fastrand.
Slint, `windows` and `image` arrive in phase 2 (`image` decodes the service logos).
The HWND comes from winit's own `raw_window_handle` re-export, so no separate crate is needed.

The release profile adds `panic = "abort"` to the four required settings. Unwinding tables made
up about 4.4 MB of the exe; the panic hook still writes the panic to the log before exiting.

Not used on purpose: no `chrono` (std time plus a small formatter), no `thiserror` or `anyhow`
(small manual error enums), no `log` facade (one file logger), no `futures` (tokio covers it),
no `tempfile` (tests use unique folders under the system temp dir).

## UI shell decisions (phase 2)

- **State ownership.** The UI thread owns the settings and the view state and updates the screen at
  once. Every change is sent to the worker, which writes `settings.json` atomically. The UI thread
  reads `settings.json` once at launch, before the window exists, to render from cached settings.
- **Theme.** Rust resolves System / Dark / Light and sets `Palette.color-scheme` and the design
  tokens. The winit attributes hook sets the window theme before creation, so the title bar is
  never the wrong colour, and `DwmSetWindowAttribute` matches the Windows 11 caption to the window.
  In System mode, winit's theme events switch the app live.
- **Accent.** The Windows accent palette from the registry is used when the shade WinUI would use
  passes 4.5:1 contrast both against the surfaces and for text drawn on it; otherwise a built-in
  blue is used.
- **Images.** The worker decodes each logo, scales it to the exact physical pixels shown, rounds
  its corners (the software renderer can't clip to rounded rectangles) and sends a
  `SharedPixelBuffer`. Tile logos are dropped from memory whenever no screen shows the tiles.
- **Window.** The first launch fits the default 1120 × 760 window to the primary monitor's work
  area. Size, position and maximized state are restored only if the title bar would be on a
  connected monitor.
- **Repaint workaround.** Slint 1.18.1's software renderer presents only the regions it changed, so
  areas Windows discards can stay unpainted (slint-ui/slint#13652, #13537). When a redraw arrives
  with a system update region (`GetUpdateRect`), or the window is uncovered, the app flips an
  invisible full-window layer to force a full repaint. This mirrors the upstream fix,
  slint-ui/slint#13568, which is merged but not yet released; remove it once a release has it.
- **Region.** Windows' "Country or region" can name a place TMDB has no data for, as on the
  development machine (Sint Maarten). Detection therefore tries it, then the regional format's
  country (en-AU gives AU), then AU, and keeps the first that TMDB lists.

## Pick screen decisions (phase 3)

- **One request at a time, newest wins.** Every Pick or Previous carries an id. The worker
  remembers the newest id and drops the answer to an older one, so a pick that was replaced
  (for example by a filter change while it ran) is never shown or recorded. The UI drops
  answers to ids it no longer waits for.
- **The prepared next pick.** The worker keeps one background pick, made for an exact
  `PickRequest` (region, expanded services, free titles, type, genres, IMDb minimums, years,
  runtime, obscurity floor). It starts once setup is done and the catalog and genres have
  loaded, after every shown pick, and 0.6 s after the last change to filters or services. It
  also downloads that title's poster and backdrop into the image cache, at the poster width the
  UI reports as soon as the Pick page is laid out. "Pick again" uses it
  only for an identical request. Before it is shown, a title excluded in the meantime is
  dropped, IMDb ratings that finished downloading are applied, and availability older than
  24 hours is verified again with `reverify`, the same check the pick loop uses.
- **History.** The worker keeps this session's picks (up to 100) and a cursor. "Previous" moves
  the cursor back and reloads the title's details for the current region through the cache,
  so a region or services change is reflected. A new pick is appended after all others.
  Exclusions are written to SQLite at once; the last 50 picks come from the database.
- **Images at exact size.** The card reports its size and its poster's size through Slint
  `changed` callbacks. The UI converts them to physical pixels and asks for both images; the
  worker decodes the poster with rounded corners and builds the backdrop: TMDB's `w300`
  image, cropped to the card's shape at 160 px wide, blurred, scaled to the card's exact size,
  corners rounded. During a window resize the old images stretch, and exact ones follow 150 ms
  after resizing stops. Leaving the Pick page releases both.
- **No flash between cards.** A new card waits up to 250 ms for its poster, so text and poster
  appear together. An answer that takes longer than 150 ms shows placeholders first.
- **Layout.** The filter bar and the provider chips use `FlexboxLayout`, stable in Slint 1.18 and
  backed by taffy, which Slint already links. At the minimum window size the filter bar wraps
  to two rows and the details scroll beside the poster.
- **Overview clamp.** A hidden copy of the overview measures its wrapped height, and a hidden
  three-line string measures three lines. "More" appears only when the first is taller. The
  button is positioned inside the overview's box: as a layout item, its presence would feed
  back into the width the text wraps at, which Slint reports as a binding loop.
- **Keyboard.** Space or Enter picks when no control uses the key, Ctrl+F opens Search, and Esc
  closes the open panel, then the side sheet, then leaves a text field.

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

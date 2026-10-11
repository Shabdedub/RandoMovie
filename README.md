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
| 3 | Pick screen: verification, filters, history, exclusions, prefetch | Done |
| 4 | Search and the shared title card | Done |
| 5 | Polish, accessibility, performance measurement, final README | Done |

Every screen works: first run, Pick, Search, My Services, Settings and About. The final build
(0.5.0) meets every budget in the spec: 0 % CPU at idle, about 22 MB of private memory after 30
titles, a window on screen about 50 ms after launch, and a 13.17 MB exe. PERFORMANCE.md has the
measurements and how they were taken.

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

## The Pick screen

- **Pick for me** (later **Pick again**) shows a random title you can stream now. Every pick is
  checked against availability data under 24 hours old first. The next pick is prepared in the
  background, so the button usually answers at once.
- **The title card** shows the poster, title, year, Movie or Series badge, runtime or seasons,
  genres, the IMDb rating and votes, and a three-line overview with **More**. "Where to watch
  in {region}" gives the verdict, then the services grouped as On your services, Free, Other
  subscriptions and Rent or buy (collapsed). **Open where to watch** opens TMDB's watch page.
- **The filter bar** has Movies / Series / Both, genres, the minimum IMDb rating and votes,
  the year range, and the maximum length for movies. **Reset filters** appears once anything
  differs from the defaults.
- **Your services** shows the services picks come from. **Edit** opens them in a side sheet.
- **Previous** steps back through this session's picks. **Seen it** and **Not interested**
  keep a title out of future picks, and **Undo** takes that back.

## Search

- **Type a title.** Results appear 300 ms after you stop typing, movies and series only.
- **Each result** shows its poster, year and type, its IMDb rating, and where it streams in your
  region: "Included with Stan", "Free on SBS On Demand", "Not on your services" or "Not
  available to stream". These load in the background, top rows first.
- **Open a result** for the same title card as on Pick. **Back to results** or Esc returns to
  the list where you left it.
- **Load more results** fetches the next page. With the keyboard, Down on the last result does
  the same.

## Keyboard and accessibility

Everything works from the keyboard. Tab and Shift+Tab move between controls, and the focused
one gets an accent ring. Clicking with the mouse never shows the ring.

| Key | Action |
|---|---|
| Space or Enter | Use the focused control. On the Pick screen with nothing focused: pick again |
| Ctrl+F | Go to Search, cursor in the box |
| Up / Down | Navigation rail: move between pages; Enter or Space opens one |
| Left / Right | Movies / Series / Both and the other choice rows: change the choice |
| Arrow keys | My Services: move between service tiles |
| Enter, Space or Down on a filter | Open its panel, with focus on the panel's first control |
| Down / Up | In Search: from the box into the results, then from result to result |
| Home, End, Page Up, Page Down | In Search: jump through the results |
| Enter | In Search: open the selected result |
| Esc | Close the open panel (focus returns to its button), side sheet or result; otherwise leave a text field |

For screen readers such as Narrator and NVDA:

- Every control has a name and a role. Buttons say what they do ("Seen it: never pick this
  title again"), filter buttons read their current value ("IMDb minimum: IMDb 6+ · 1K+
  votes"), service tiles are checkboxes, and each search result reads its title, year, type,
  rating and verdict. Decorative icons are hidden.
- A new pick, a search's results, an opened title and any error are announced.
- All text meets WCAG AA contrast (4.5:1), and icons and focus rings meet 3:1, in both themes
  and over the title card's backdrop. A unit test reads `ui/theme.slint` and checks every
  colour against every surface it's drawn on.
- The app is crisp from 100 % to 200 % scaling, because every image is decoded at the exact
  pixel size shown. It stays usable down to the minimum window of 800 × 560.

Two limits come from Slint 1.18 and can't be changed from the app: a filter panel appears to
screen readers as an unnamed window around its named controls, and the clear button (×) in
text fields is an unnamed image that works only with the mouse.

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
- **Search.** TMDB's multi-search, movies and series only. Each row then gets the same details
  the title card uses, cached for 24 hours, for its rating and where it streams.
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
  passes WCAG AA contrast in the current theme, as text on the title card over any backdrop
  and with text drawn on it; otherwise a built-in blue is used.
- **Default type.** The setting decides where the Movies / Series / Both filter starts each time
  the app opens. The filter bar's choice isn't saved; the other filters are. Changing the
  default in Settings also changes the current choice.
- **Prepared picks.** The next pick is prepared in the background after startup, after every
  pick, and 0.6 s after filters or services change. It is used only if it was made for exactly
  the current settings. A title excluded since is dropped, and availability older than 24
  hours is verified again before it is shown.
- **History.** "Previous" walks back through this session's picks in the order they were
  shown, up to 100. Going back removes nothing: the next pick is added after all of them. A
  card shown again uses availability for the current region and services.
- **Seen it and Not interested.** Both keep the title out of future picks until undone, also
  in later sessions. The card stays on screen. Separately, the last 50 picks are never
  repeated.
- **Year filter.** The panel has decade presets and From / To boxes. A box applies once it
  holds a full year from 1870 to 2100; until then the previous range stays.
- **No page title on Pick.** The title card gets the room instead. At the minimum window size
  the filter bar wraps to two rows and the details scroll beside the poster.
- **Card timing.** A new card waits up to 250 ms for its poster, so text and poster appear
  together. If a pick takes longer than 150 ms, placeholders show in the meantime.
- **Backdrop.** It is made from TMDB's `w300` backdrop: blurred at 160 px wide, scaled to the
  card's exact size, and covered by a veil of the card colour, 86 % in the dark theme and 90 %
  in the light one. The veil keeps muted text and accent links above 4.5:1 over any image. A
  title without a backdrop shows the plain card.
- **Attribution on the card.** "Streaming data: JustWatch via TMDB" sits under the "Where to
  watch" heading, with "Open where to watch" beside it, so both stay in view.
- **Failed picks.** The card area explains what happened and offers one way out: Reset
  filters, Choose services, Add token or Try again. "Previous" then returns to the last card.
- **Ratings and availability in the results.** The spec lists thumbnail, title, year and type
  for each result. Each row also shows the IMDb rating and the verdict line, so the list
  itself answers "is it on my services?" and every title shown carries its rating. Rows load
  their details four at a time; a new query stops the old one's requests.
- **Search queries** need two characters. A single letter matches too much to be useful.
- **Search and the card.** Typing a new query closes an open result. Leaving Search keeps the
  query and results but frees their images, which reload when you come back. A change of
  services or region updates the rows and the open card the next time Search shows.
- **Offline search.** Earlier searches and titles come from the cache, under the offline
  banner. A search that failed offline runs again by itself when the connection returns. A
  result that didn't open offers Try again.
- **Title card on Search.** It's the Pick screen's card without "Seen it" and "Not
  interested", which belong to picking.
- **Obscurity floor.** Settings offers Any, 25, 50 (default), 100, 250, 500 and 1,000 TMDB votes.
  A different value typed into `settings.json` is kept and shown as an extra choice.
- **Pool sizes.** TMDB now reports at most 20,001 results and serves at most 500 pages, so a
  pool's reachable size is capped at 10,000 titles, the most popular first.
- **Misses.** A miss is a candidate that fails verification or a filter. Excluded and recently
  picked titles are skipped before any request and don't count. A pick also stops after 10
  page fetches, so it always ends quickly. Candidates are verified three at a time; the pick
  is still the first one in order that passes, so every title keeps the same chance.
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
  level. It is migrated to the current layout on load. A UTF-8 byte-order mark, which some
  editors add, is accepted. An IMDb minimum or maximum length between the filter bar's steps
  moves to the step the bar shows, so the filter shown is the filter applied.
- **Release profile.** `panic = "abort"` is added to the required release settings. It removes
  about 4.4 MB of unwinding tables; panics are still written to the log.
- **Repaint workaround.** Slint 1.18.1's software renderer can leave areas unpainted after Windows
  discards window content, for example after a snap or a restore. The app forces a full repaint
  when Windows reports such an area, mirroring Slint's unreleased fix (PLAN.md has details).
- **Patched text layout, for the size budget.** Slint 1.18.1 always links ICU4X's line-break
  dictionaries for Thai, Lao, Khmer, Burmese, Chinese and Japanese through parley, its text
  layout crate: 3.6 MB that alone broke the 15 MB budget, with no Slint setting to drop them.
  The build uses a copy of parley 0.11.1 in `vendor/parley` that uses ICU4X's rule-based line
  breaking instead (`vendor/parley/RANDOMOVIE-PATCH.md`). Chinese and Japanese text still wraps
  between characters; Thai, Lao, Khmer and Burmese wrap only at spaces. TMDB text is requested
  in English, so this rarely shows.
- **WebP images.** The spec limits decoding to JPEG and PNG. A few TMDB images are WebP files
  despite their `.jpg` names, whatever the request asks for; one turned up in testing, the
  1966 "Batman" poster. Those show the placeholder, like a missing poster.
- **Stores in My Services.** TMDB's service list includes stores that only rent or sell, such
  as Apple TV Store, Amazon Video and Google Play Movies, and doesn't say which ones they are.
  They stay in the list. Picks never come from them, and when the selected services yield
  nothing, Pick's message says that stores which only rent or sell don't count.
- **Genre panel.** "Any genre" is the first chip in the panel, checked when no genre is chosen
  and clearing the choice when pressed, so the panel opens with the keyboard on it.
- **Arrow keys in the navigation rail** move the focus only; Enter or Space opens the page, as
  in Windows' own navigation panes, so a screen reader user hears each page before switching.
- **First launch.** Windows checks a newly built or downloaded exe once before starting it.
  That took about 1 s in testing (2.4 s right after a build) and happens only on the first
  launch of each new exe; the app itself shows its window about 50 ms after it starts.

## Acceptance checks

The final run of all four checks was on 2026-10-11, region AU, with the final release build
(0.5.0), through the app itself. UI Automation scripts typed into the search box or pressed
"Pick again", and read each row or card from the accessibility tree, as a screen reader would.
Each profile started with an empty cache.

| Check | Result |
|---|---|
| One service (Netflix), free titles off: 30 consecutive picks show it under "On your services" | Pass: 30 of 30 read "Included with Netflix" with Netflix under "On your services", no repeats |
| No services, free titles on: every pick shows a Free provider | Pass: 20 of 20 read "Free on …" with a Free group, no repeats, 2 of them series |
| Search a well-known film for its IMDb rating and AU availability in about a second | Pass: "The Matrix" row with ★ 8.7 IMDb · 2.3M votes and its AU verdict 732 ms after typing on a cold cache, including the 300 ms pause. Another cold run, slowed by one slow TMDB response, took 1,188 ms; Inception, Paddington 2 and Dune: Part Two took 819 to 934 ms |
| Idle 60 s with a pick on screen: 0 % CPU, memory within budget | Pass: 0 ms of CPU across all threads in 60 s, 22.3 MB private bytes after 30 picks. Also 0 ms with search results on screen |

Every one of those 50 picks came from the pick prepared in the background, and each card
appeared 7 to 14 ms after the click, poster included. PERFORMANCE.md has the details.

The live tests repeat the first three checks at the data level; all 7 pass on the final build:

```powershell
cargo test --release --test live -- --ignored --test-threads=1 --nocapture
```

## Attribution

- This product uses the TMDB API but is not endorsed or certified by TMDB.
- Streaming availability data is provided by JustWatch through TMDB.
- Information courtesy of IMDb (<https://www.imdb.com>). Used with permission. IMDb ratings come
  from IMDb's non-commercial datasets, for personal and non-commercial use.
- The UI is built with Slint under its royalty-free licence; the About screen shows Slint's
  `AboutSlint` credit.

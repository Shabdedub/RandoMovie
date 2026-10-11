# Performance

Measurements are taken on release builds. This file grows with each phase, newest first: the
UI shell is measured from phase 2, and phase 5 repeats every budget on the final build. Sizes
use binary units, as Windows Explorer shows them: 1 MB is 1,048,576 bytes.

## Budgets from the spec

All budgets were met by phase 5's final build (0.5.0) and still are with the Library (0.6.0).

| Budget | Target | Result |
|---|---|---|
| Idle CPU | 0 % | 0 ms of CPU in 60 s, across all threads, with a pick on screen and with search results on screen; 0 ms in 30 s with the Library on screen (0.6.0) |
| Private working set after about 20 titles | under 50 MB | 22.3 MB private bytes after 30 picks; 22.7 MB after a series of searches and opened titles more |
| Window interactive after launch | within 0.5 s | Visible 43 to 51 ms after launch; first frame 59 to 63 ms after `main` starts |
| `RandoMovie.exe` size | under 15 MB | 13.63 MB (14,295,040 bytes) with the Library; 13.17 MB in 0.5.0 |
| IMDb rebuild peak memory | "modest" | About 2 MB extra (phase 1) |

## After phase 5: the Library (0.6.0)

Measured on 2026-10-11 with the 0.6.0 release build, on a test profile with titles in both
lists.

| Measurement | Result |
|---|---|
| `RandoMovie.exe` | 13.63 MB (14,295,040 bytes), 472 KB more than 0.5.0 |
| Idle 30 s with the Library on screen, all threads | 0 ms of CPU |
| Private bytes with the Library on screen | 14.9 MB at the test window's size, 13.3 MB at the minimum size |

The Library adds no network work: the list is one SQLite query, ratings come from the local
IMDb index, and marking a title saves its details from the response already cached for the
card. Most of the 472 KB is the code Slint generates for the page and its row, much as
Search's page added 0.47 MB in phase 4; the exe stays 1.37 MB under budget.

## Phase 5: final measurements

Measured on 2026-10-11 with the final release build (0.5.0) on the development machine
described under phase 2, region AU. Each acceptance profile started with the IMDb index in
place and an empty cache. UI Automation drove the app and read each card and row from the
accessibility tree; the app's log timed each step.

| Measurement | Result |
|---|---|
| Launch to visible window, measured from outside (`EnumWindows` every 2 ms) | 43 and 44 ms; 51 ms for a second copy of the exe |
| Window created / first frame, from the app's log (from `main`) | 55 to 58 ms / 59 to 63 ms (three launches) |
| Private bytes 3 s after launch, Pick page | 13.8 to 14.0 MB |
| 30 Netflix-only picks, 2.5 s between clicks | 30 of 30 from the prepared pick; click to card on screen 7 to 14 ms, median 8, poster included |
| 20 free-only picks, 2.5 s between clicks | 20 of 20 from the prepared pick; 7 to 14 ms |
| Idle 60 s with a pick on screen, all 9 threads | 0 ms of CPU; private bytes 22.3 MB after the 30 picks, working set 48 MB |
| Search, typing to the first row's IMDb rating and AU verdict, cold cache | 732 to 1,188 ms over five queries, including the 300 ms pause (table below) |
| Idle 60 s with search results on screen | 0 ms of CPU; private bytes 22.7 MB after the 30 picks, the searches below and others, and several opened titles |
| `RandoMovie.exe` | 13.17 MB (13,811,712 bytes), version info 0.5.0 |

| Query (cold cache) | Search sent to results | To the first rating and verdict | Typing to rating and verdict |
|---|---|---|---|
| The Matrix (Netflix profile) | 649 ms | 857 ms | 1,188 ms |
| The Matrix (free-titles profile) | 149 ms | 299 ms | 732 ms |
| Inception | 302 ms | 546 ms | 934 ms |
| Paddington 2 | 243 ms | 497 ms | 819 ms |
| Dune: Part Two, after 70 s idle | 245 ms | 559 ms | 885 ms |

The slow Matrix search was a slow TMDB response, not a cold connection: the query after 70
seconds of idle, when the keep-alive connection could have closed, took the usual 245 ms.

### First launch of a new exe

The first launch right after building took 2.4 s to show the window, while the app's log put
its first frame 63 ms after `main`. A fresh copy of the same exe showed where the time goes:
`Process.Start` returned after 1,048 ms because Windows checks a new, unsigned executable once
before creating its process. The window was visible 64 ms after the process existed, and the
copy's second launch took 51 ms in all. Signing the exe is the usual way to shorten that check;
nothing inside the app can.

### Exe size

The final exe is 13.17 MB, down 3.68 MB from phase 4's 16.85 MB and 1.83 MB under budget.

Nearly all of the saving came from one source. Phase 4's exe held 5.06 MB of read-only data,
and 3.6 MB of it were ICU4X's line-break dictionaries (1.9 MB for Thai, Lao, Khmer and
Burmese, 1.7 MB for Chinese and Japanese). They come from parley, Slint's text layout crate:
Slint 1.18.1 always turns on parley's `complex-scripts` feature, which switches parley to
ICU4X's dictionary-based segmenters, and no Slint feature turns it off. The project now builds
parley 0.11.1 from `vendor/parley`, an unmodified copy except that it always uses ICU4X's
rule-based segmenters (`vendor/parley/RANDOMOVIE-PATCH.md`). Text in those scripts still
renders and wraps by the Unicode rules: Chinese and Japanese break between characters as
before, while Thai, Lao, Khmer and Burmese wrap only at spaces. RandoMovie asks TMDB for
English text, so this rarely shows.

| Section | Phase 4 | Phase 5 |
|---|---|---|
| Code (`.text`) | 11.51 MB | 11.44 MB |
| Read-only data (`.rdata`) | 5.06 MB | 1.45 MB |
| Everything else (unwind info, data, resources, relocations) | 0.28 MB | 0.28 MB |

Code shrank by 73 KB, 69 KB of it when the icon widget stopped inheriting from `Text`: icon
instances no longer carry the accessibility properties a `Text` gets. The per-package `opt-level = "s"`
idea from phase 3 wasn't needed and wasn't adopted, so every crate keeps the required
`opt-level = 3`.

### Method

- **Startup.** As in phase 2: a script starts the exe and polls `EnumWindows` every 2 ms for a
  visible window from the process, three launches in a row, then reads the app's log lines.
- **Picks, idle and memory.** As in phase 3: UI Automation presses "Pick again", waits for the
  new card, checks its groups, and pauses 2.5 s. After the run, the idle script records every
  thread's `TotalProcessorTime` and the process's private bytes, waits 60 s, and reads them
  again.
- **Search.** As in phase 4: UI Automation sets the search box's text and polls until the
  expected row shows its rating and verdict. Each query was new to the profile's cache.

## Phase 4: Search

Measured on 2026-10-11 with the release build on the same machine and connection, region AU.
UI Automation typed into the search box and read the rows and cards from the accessibility
tree; the app's log timed each step.

| Measurement | Result |
|---|---|
| Typing "The Matrix" to its row on screen, cold cache, measured from outside | 541 ms |
| Typing to that row's IMDb rating and AU verdict, cold cache, measured from outside | 710 ms |
| Search sent to results on screen / to the first row's rating and verdict, from the app's log | 177 ms / 385 ms |
| The same query again (cache) | 0 ms to results, 4 ms to the first rating and verdict |
| "Load more": next page on screen | 222 to 262 ms |
| Idle for 60 s with 59 results on screen, all threads | 0 ms of CPU |
| Private bytes after 30 picks, 59 search results and 6 opened titles | 23.0 MB |
| Click to new card on screen on Pick, poster included (50 picks, two profiles) | 7 to 11 ms |

The 300 ms between the last keystroke and the request is the spec's debounce; everything
after it is network time on a cold cache. Each row's details are the same call the title card
uses, so opening a result is a cache hit and only the poster has to download.

### Exe size

Search added 0.47 MB, to 16.85 MB (17,668,096 bytes): a page, a worker module and its
messages, and the card code now shared by both pages. Phase 5 has to win back about 1.9 MB.

## Phase 3: Pick screen

Measured on 2026-10-11 with the release build on the same machine and connection, region AU,
with UI Automation pressing "Pick again" and reading each card from the accessibility tree.
Each profile started with the IMDb index in place and an empty cache.

| Measurement | Result |
|---|---|
| Idle for 60 s with a pick on screen, all threads (two runs) | 0 ms of CPU |
| Private bytes after 30 titles (two runs) | 23.0 and 24.7 MB; working set 49 to 60 MB, including shared system DLL pages |
| Picks answered from the prepared pick, 2.5 s between clicks | Netflix only: 30 of 30. Free titles only: 20 of 20 |
| Click to new card on screen, poster included, prepared pick | 3 to 6 ms (48 cards after the first of each run, from the app's log) |
| First pick after a cold start, clicked 8 s after launch | 8 ms, poster included |
| Startup: window created / first frame (three launches) | 54 to 99 ms / 61 to 103 ms |
| 30 Netflix picks back to back at the data level (live test, cold cache) | 13.5 s, 0.45 s each |

### What makes "Pick again" instant

- The worker prepares the next pick in the background and downloads its poster and backdrop
  into the image cache, so showing it needs no network. The UI reports the poster's size as
  soon as the page is laid out, so even the pick prepared during startup has its poster
  ready. Before that hint existed, the first card of a run took 250 to 393 ms.
- The UI holds the new card for up to 250 ms until its poster is decoded at the exact size, so
  text and poster replace the old card in one frame.
- Verification checks three candidates at a time (`VERIFY_BATCH`). The chosen title is still
  the first one in try order that passes, so the odds don't change, but a run of misses is
  three times shorter. This matters most for free titles: many are obscure and fail the IMDb
  minimums. Before batching, with 2.5 s between clicks, only 11 of 20 free-only picks were
  ready in time and the slowest waited 4.4 s; after it, all 20 were ready.

### Exe size

Phase 3 added 1.34 MB: the exe went from 15.03 MB to 16.38 MB (17,175,040 bytes). `cargo bloat`
on an unstripped build puts the code at 11.1 MB, up from 9.8 MB, and spreads the growth over
the new screen rather than one dependency:

| Code | Phase 2 | Phase 3 |
|---|---|---|
| Rust standard library, including generic code instantiated for the app's types | 1.8 MB | 2.3 MB |
| The app's UI, mostly code generated from the `.slint` files | 1.2 MB | 1.6 MB |
| The data layer (`randomovie` library) | in "the rest" | 0.15 MB |
| `taffy`, the flexbox solver behind `FlexboxLayout` (already linked, now used) | none | 0.06 MB |

Phase 5 has to win back about 1.4 MB, plus whatever Search adds in phase 4. The candidates,
in order: optimizing cold code for size (`opt-level = "s"` per package for the SVG stack,
networking, SQLite and accessibility), trimming the generated UI code, and looking again at
decoders the app never uses, such as WebP, which Slint's image loading pulls in.

### Memory

Only the card on screen keeps decoded images: the poster at its exact size and the backdrop at
the card's exact size. In the test window (1120 × 639 logical at 150 %) the poster is 408 × 612
pixels (1.0 MB) and the backdrop 1476 × 672 (3.8 MB). Both are dropped when another page shows
and decoded again from the image cache when Pick returns. Chip logos are 24 px images in the
same logo map as the service tiles, bounded by the number of services in the region.

### Method

- **Picks.** A UI Automation script invokes "Pick again", polls the accessibility tree until a
  new card appears, and checks its verdict and its "On your services" or "Free" group. It waits
  2.5 s after each card, like someone reading it. The app's log records whether each pick came
  from the prepared pick, and the time from the click to the card on screen.
- **Idle and memory.** After the 30 picks, the script waits 5 s, then records every thread's
  `TotalProcessorTime` and the process's private bytes, waits 60 s, and reads them again.

## Phase 2: UI shell

Measured on 2026-10-10 with the release build, on the development machine: Windows 11 build
26200, 16 logical cores, a 1920 × 1200 primary display at 150 % scaling. The profile had
finished setup with Netflix and Stan selected, so the Pick page showed two logos.

| Measurement | Result |
|---|---|
| Launch to visible window, measured from outside | 120 to 140 ms |
| Window created / first frame, from the app's log (from `main`) | 98 ms / 107 ms |
| CPU used while idle on the Pick page for 60 s, all threads | 0 ms |
| Private bytes after startup on the Pick page | 11.3 MB |
| Private bytes on My Services, 24 logos decoded at 150 % | 13.0 MB |
| Private bytes back on Pick, tile logos released | 12.9 MB |
| Working set, including shared system DLL pages | 43 to 45 MB |

### Method

- **Startup.** A PowerShell script starts `RandoMovie.exe` and polls `EnumWindows` every 2 ms
  for a visible window from the process. .NET's `Process.MainWindowHandle` is not used: it
  reported this window seconds late, and polling it cost the app CPU time.
- **Idle CPU.** After 5 s to settle, the script records every thread's `TotalProcessorTime`,
  waits 60 s without touching the app, and reads them again.
- **Memory.** `Process.PrivateMemorySize64` (private commit) and `WorkingSet64`, read at each
  point while UI Automation moves between pages.

### Renderer

The app uses Slint's software renderer, as the spec prefers: no GPU context, and only changed
regions are repainted. Every visual in the shell works with it. Two of its limits shaped the
design rather than forcing FemtoVG:

- It can't clip images to rounded rectangles, so the worker rounds logo corners while decoding.
- It has no drop shadows, so elevation uses surface tones and 1 px borders, as Fluent does in
  dark mode.

Slint 1.18.1's software renderer has a known Windows bug: it presents only the regions it
changed, so pixels Windows discards (after a snap or a restore) can stay unpainted
(slint-ui/slint#13652). It was seen once during testing. The app now forces a full repaint
whenever Windows reports an update region, which mirrors the upstream fix (#13568, merged, not
yet released). This costs one full-window repaint per system invalidation and nothing at idle.

### Exe size

| Build | `RandoMovie.exe` |
|---|---|
| Required profile (`lto = "fat"`, `codegen-units = 1`, `strip = true`, `opt-level = 3`) | 19.44 MB |
| The same plus `panic = "abort"` | 15.03 MB (15,762,944 bytes) |

Of the 15.03 MB, code is 9.8 MB and read-only data 5 MB. `cargo bloat --crates` on an
unstripped copy attributes the code like this:

| Code | Size |
|---|---|
| Rust standard library | 1.8 MB |
| The app's UI, mostly code generated from the `.slint` files | 1.2 MB |
| Text shaping and fonts: skrifa, harfrust, read-fonts, zeno, parley, fontique | 1.5 MB |
| Slint core and software renderer | 1.0 MB |
| SVG: usvg, resvg, tiny-skia, roxmltree, svgtypes | 0.8 MB |
| JPEG and WebP decoders | 0.4 MB |
| AccessKit, winit, tokio, hyper, reqwest, SQLite and the rest | about 3 MB |

The SVG and image decoders are switched on by Slint's winit backend itself, and the text stack
is required by the software renderer, so none of them can be turned off with a feature. Phase 5
will try size-optimizing cold dependencies and trimming the generated UI code, and report the
final size. Phases 3 and 4 add code, so the trimming has to win back more than 34 KB.

## Phase 1: data layer

Measured on 2026-10-10 on the development machine (Windows 11 build 26200, x64) over a home
connection in Australia.

| Measurement | Result |
|---|---|
| IMDb dataset download plus index build, 1,718,968 titles | 0.7 to 1.9 s |
| Peak private bytes of the smoke run with a full IMDb rebuild | 4.9 MB |
| Peak private bytes of the same run with the index already built | 2.9 MB |
| Peak working set, with and without the rebuild | 14.8 MB and 12.8 MB |
| IMDb index on disk | 14.8 MB (9 bytes per title plus a 16-byte header) |
| IMDb lookup, random ids, LRU cold | 36 µs each |
| Random pick, cold cache (Netflix plus free titles) | 0.55 to 0.83 s |
| 30 picks in a row with one service, cold cache, debug build | 18.7 s, about 0.6 s each |
| Search, details and rating for "The Matrix", cold cache | 0.30 to 0.51 s |
| The same search repeated (cache hit) | under 0.01 s |

### Method

- **Peak memory.** `target\release\examples\smoke.exe` ran with `RANDOMOVIE_HOME` set to an
  empty folder, which forces the IMDb download and rebuild, then again with the index in
  place. A PowerShell loop polled `Process.PeakPagedMemorySize64` (peak commit, which is
  private bytes) and `PeakWorkingSet64` every 20 ms until exit. The difference between the
  two runs is the cost of the rebuild.
- **Lookup speed.** `cargo test --release --test live imdb_refresh_is_conditional -- --ignored --nocapture`
  times 10,000 lookups of random ids.
- **Pick and search times.** These come from the smoke example and the live acceptance tests,
  which print their own timings.

### Why the rebuild stays small

The dataset arrives sorted as text, which isn't numeric order once 8-digit ids appear. Instead
of loading all 1.7 million rows to sort them, the builder keeps one chunk of 256 K records
(2.3 MB), sorts it, and spills it to a run file. The 7 runs are then merged through 32 KB
buffers into the final index. Everything is freed when the build returns, and the old index
keeps serving lookups until the new one is renamed into place.

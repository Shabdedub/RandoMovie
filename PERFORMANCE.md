# Performance

Measurements are taken on release builds. This file grows with each phase: the UI shell is
measured from phase 2, and phase 5 repeats every budget with picks on screen. Sizes use binary
units, as Windows Explorer shows them: 1 MB is 1,048,576 bytes.

## Budgets from the spec

| Budget | Target | Status |
|---|---|---|
| Idle CPU | 0 % | Shell: 0 ms of CPU in 60 s idle. Recheck with a pick on screen in phase 5 |
| Private working set after about 20 titles | under 50 MB | Shell: 13 MB private bytes. Recheck with titles in phase 5 |
| Window interactive after launch | within 0.5 s | Shell: visible after 120 to 140 ms |
| `RandoMovie.exe` size | under 15 MB | 15.03 MB, over by 34 KB; phase 5 trims it |
| IMDb rebuild peak memory | "modest" | About 2 MB extra, see below |

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

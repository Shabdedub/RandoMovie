# Performance

Measurements are taken on release builds. This file grows with each phase. The UI budgets
(idle CPU, working set after 20 titles, launch time, exe size) are measured in phase 5, once
there is a window to measure.

## Budgets from the spec

| Budget | Target | Status |
|---|---|---|
| Idle CPU | 0 % | Phase 5 |
| Private working set after about 20 titles | under 50 MB | Phase 5 |
| Window interactive after launch | within 0.5 s | Phase 5 |
| `RandoMovie.exe` size | under 15 MB | Phase 5 |
| IMDb rebuild peak memory | "modest" | About 2 MB extra, see below |

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

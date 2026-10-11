# parley 0.11.1, patched for RandoMovie

This is parley 0.11.1 exactly as published on crates.io (Apache-2.0 OR MIT, see the licence
files), with one change in `src/analysis/mod.rs`: the word and line segmenters are always
ICU4X's rule-based ones (`new_for_non_complex_scripts`), even when the `complex-scripts`
feature is on. `Cargo.toml` also allows the `deprecated` lint: Cargo hides a crates.io
dependency's warnings but shows a path copy's, and parley calls one deprecated ICU4X method.

## Why

Slint 1.18.1 always enables parley's `complex-scripts` feature on desktop. That makes parley use
ICU4X's dictionary-based segmenters, which link the line-break dictionaries for Thai, Lao,
Khmer and Burmese (1.9 MB) and for Chinese and Japanese (1.7 MB). Those 3.6 MB alone put
`RandoMovie.exe` over the spec's 15 MB budget, and no Slint feature turns them off. With this
copy the exe went from 16.85 MB to 13.17 MB (sizes in MiB, as Windows Explorer shows them).

## What it changes for the app

RandoMovie asks TMDB for English (`en-US`) text, so these scripts rarely appear. If they do,
text still renders and wraps by the Unicode line-breaking rules: Chinese and Japanese can
break between characters as usual, while Thai, Lao, Khmer and Burmese wrap only at spaces.

## Updating

When Slint moves to a newer parley, copy that version here, re-apply the change (search for
"RandoMovie patch"), and update the `[patch.crates-io]` entry in the root `Cargo.toml`. Remove
this copy entirely once Slint lets the feature be turned off.

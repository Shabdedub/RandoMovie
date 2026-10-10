//! RandoMovie: one click answers "what should I watch tonight?".
//!
//! The main thread runs the Slint UI; one worker thread does all network, disk, parsing
//! and image decoding. See PLAN.md for the architecture.

#![windows_subsystem = "windows"]
// The binary is named after the product, RandoMovie.exe, which isn't snake case.
#![allow(non_snake_case)]

slint::include_modules!();

mod ui;
mod worker;

use std::time::Instant;

use randomovie::paths::AppPaths;
use randomovie::{log_error, log_info};

fn main() {
    let launched = Instant::now();
    randomovie::platform::monitor::enable_per_monitor_dpi();
    let paths = AppPaths::from_env();
    let _ = paths.ensure_dirs();
    let _ = randomovie::util::log::init(&paths.log_dir());
    log_info!("RandoMovie {} starting", env!("CARGO_PKG_VERSION"));

    if let Err(e) = ui::run(paths, launched) {
        log_error!("RandoMovie stopped with an error: {e}");
        randomovie::platform::dialog::show_error(
            "RandoMovie",
            &format!("RandoMovie couldn't start or stopped unexpectedly.\n\n{e}"),
        );
    }
}

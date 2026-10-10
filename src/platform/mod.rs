//! Thin wrappers over the Windows APIs the app needs. Each function is small, never panics,
//! and reports failure by returning `None`, `false` or an error string.

pub mod credentials;
pub mod dialog;
pub mod fonts;
pub mod monitor;
pub mod network;
pub mod region;
pub mod repaint;
pub mod shell;
pub mod theme;

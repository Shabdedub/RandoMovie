//! System fonts. The app bundles none: it uses Segoe UI Variable (Windows 11) or Segoe UI,
//! and Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets (Windows 10).

use std::path::PathBuf;

fn fonts_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts")
}

pub fn installed(file_name: &str) -> bool {
    fonts_dir().join(file_name).exists()
}

/// The text font family.
pub fn ui_font() -> &'static str {
    if installed("SegUIVar.ttf") {
        "Segoe UI Variable Text"
    } else {
        "Segoe UI"
    }
}

/// The icon font family. Both fonts share the codepoints the app uses.
pub fn icon_font() -> &'static str {
    if installed("SegoeIcons.ttf") {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

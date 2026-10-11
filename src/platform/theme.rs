//! Windows appearance: the light/dark preference, the accent colour, and the title bar.

use std::ffi::c_void;

use windows::Win32::Foundation::{ERROR_SUCCESS, HWND};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmSetWindowAttribute,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::core::w;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f64 {
        fn linear(c: u8) -> f64 {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// `0x00BBGGRR`, the Win32 COLORREF layout.
    fn colorref(self) -> u32 {
        u32::from(self.r) | (u32::from(self.g) << 8) | (u32::from(self.b) << 16)
    }
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The app's window backgrounds, used for the title bar.
pub const DARK_BACKGROUND: Rgb = Rgb::new(0x0f, 0x11, 0x15);
pub const LIGHT_BACKGROUND: Rgb = Rgb::new(0xf3, 0xf4, 0xf7);
pub const DARK_TEXT: Rgb = Rgb::new(0xf2, 0xf4, 0xf8);
pub const LIGHT_TEXT: Rgb = Rgb::new(0x14, 0x17, 0x1f);
/// The surface the accent must stand out from as link text: the lightest dark surface and the
/// darkest light one. Both are the title card's veil (`backdrop-veil` in ui/theme.slint) over
/// the worst backdrop pixel, white under the dark veil and black under the light one. The tests
/// check these against the theme file.
const DARK_WORST_SURFACE: Rgb = Rgb::new(0x38, 0x3a, 0x40);
const LIGHT_WORST_SURFACE: Rgb = Rgb::new(0xe6, 0xe6, 0xe6);
const ON_DARK_ACCENT: Rgb = Rgb::new(0x0b, 0x0d, 0x12);
const ON_LIGHT_ACCENT: Rgb = Rgb::new(0xff, 0xff, 0xff);
const DARK_FALLBACK_ACCENT: Rgb = Rgb::new(0x6e, 0xa8, 0xfe);
const LIGHT_FALLBACK_ACCENT: Rgb = Rgb::new(0x1f, 0x5f, 0xd1);
/// WCAG AA for normal text: the accent colours links and labels as well as fills.
const MIN_CONTRAST: f64 = 4.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accent {
    pub fill: Rgb,
    /// Text and icons drawn on the accent fill.
    pub on_fill: Rgb,
    /// False when the built-in blue replaced the Windows accent.
    pub from_system: bool,
}

/// The Windows accent palette, lightest to darkest: Light 3, Light 2, Light 1, Accent,
/// Dark 1, Dark 2, Dark 3, then one unused entry.
pub fn accent_palette() -> Option<[Rgb; 8]> {
    let mut data = [0u8; 32];
    let mut size = data.len() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast::<c_void>()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS || size < 32 {
        return None;
    }
    Some(std::array::from_fn(|i| {
        Rgb::new(data[i * 4], data[i * 4 + 1], data[i * 4 + 2])
    }))
}

/// Picks the shade WinUI uses for the theme (Light 2 on dark, Dark 1 on light), then the
/// neighbouring shades, keeping the first with AA contrast both ways. Falls back to blue.
pub fn pick_accent(palette: Option<[Rgb; 8]>, dark: bool) -> Accent {
    let (surface, on_fill, fallback, order) = if dark {
        (DARK_WORST_SURFACE, ON_DARK_ACCENT, DARK_FALLBACK_ACCENT, [1, 2, 0])
    } else {
        (LIGHT_WORST_SURFACE, ON_LIGHT_ACCENT, LIGHT_FALLBACK_ACCENT, [4, 5, 3])
    };
    if let Some(palette) = palette {
        for index in order {
            let fill = palette[index];
            if contrast(fill, surface) >= MIN_CONTRAST && contrast(on_fill, fill) >= MIN_CONTRAST {
                return Accent {
                    fill,
                    on_fill,
                    from_system: true,
                };
            }
        }
    }
    Accent {
        fill: fallback,
        on_fill,
        from_system: false,
    }
}

/// Windows' "Choose your app mode" setting.
pub fn system_prefers_dark() -> bool {
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast::<c_void>()),
            Some(&mut size),
        )
    };
    status == ERROR_SUCCESS && value == 0
}

/// Dark or light title bar, and on Windows 11 a caption colour matching the window so the
/// title bar blends in. Older Windows ignores the colour attributes.
pub fn style_title_bar(hwnd: isize, dark: bool) {
    let hwnd = HWND(hwnd as *mut c_void);
    let (caption, text) = if dark {
        (DARK_BACKGROUND, DARK_TEXT)
    } else {
        (LIGHT_BACKGROUND, LIGHT_TEXT)
    };
    let dark_mode: i32 = dark.into();
    let set = |attribute, value: u32| unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            attribute,
            (&value as *const u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        );
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark_mode as u32);
    set(DWMWA_CAPTION_COLOR, caption.colorref());
    set(DWMWA_BORDER_COLOR, caption.colorref());
    set(DWMWA_TEXT_COLOR, text.colorref());
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn contrast_matches_wcag_examples() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!((contrast(black, white) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.01);
        // #767676 on white is the classic 4.54:1 boundary grey.
        assert!((contrast(Rgb::new(0x76, 0x76, 0x76), white) - 4.54).abs() < 0.02);
    }

    /// The colour tokens in ui/theme.slint as (dark, light) RGBA pairs. Tokens computed in
    /// Slint (`accent-subtle`, `accent-hover`) aren't listed; the test rebuilds them.
    fn theme_tokens() -> HashMap<String, ([u8; 4], [u8; 4])> {
        fn hex(value: &str) -> Option<[u8; 4]> {
            let digits = value.trim().strip_prefix('#')?;
            let byte = |i: usize| -> Option<u8> { u8::from_str_radix(digits.get(i..i + 2)?, 16).ok() };
            match digits.len() {
                6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
                8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
                _ => None,
            }
        }
        let mut tokens = HashMap::new();
        for line in include_str!("../../ui/theme.slint").lines() {
            let Some((_, declaration)) = line.split_once("property <color> ") else {
                continue;
            };
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_end_matches(';');
            let (dark, light) = value
                .strip_prefix("dark ? ")
                .and_then(|pair| pair.split_once(" : "))
                .unwrap_or((value, value));
            if let (Some(dark), Some(light)) = (hex(dark), hex(light)) {
                tokens.insert(name.trim().to_string(), (dark, light));
            }
        }
        tokens
    }

    /// `top` drawn over `bottom` at `top`'s alpha.
    fn over(top: [u8; 4], bottom: Rgb) -> Rgb {
        let alpha = f64::from(top[3]) / 255.0;
        let mix = |t: u8, b: u8| (f64::from(t) * alpha + f64::from(b) * (1.0 - alpha)).round() as u8;
        Rgb::new(mix(top[0], bottom.r), mix(top[1], bottom.g), mix(top[2], bottom.b))
    }

    fn opaque(color: [u8; 4]) -> Rgb {
        assert_eq!(color[3], 255, "expected an opaque colour");
        Rgb::new(color[0], color[1], color[2])
    }

    /// WCAG AA in both themes: 4.5:1 for text, 3:1 for icons and focus rings (WCAG 1.4.11),
    /// on every surface each colour is drawn on. Disabled controls are exempt.
    #[test]
    fn theme_meets_wcag_aa() {
        let tokens = theme_tokens();
        let mut failures = Vec::new();
        for dark in [true, false] {
            let theme = if dark { "dark" } else { "light" };
            let token = |name: &str| {
                let (dark_value, light_value) = tokens[name];
                if dark { dark_value } else { light_value }
            };
            let solid = |name: &str| (name.to_string(), opaque(token(name)));
            let accent = pick_accent(None, dark);
            let mut accent_rgba = [accent.fill.r, accent.fill.g, accent.fill.b, 255];

            // The title card's veil over its worst backdrop pixel, as pick_accent assumes.
            let veil = over(
                token("backdrop-veil"),
                if dark { Rgb::new(255, 255, 255) } else { Rgb::new(0, 0, 0) },
            );
            let worst = if dark { DARK_WORST_SURFACE } else { LIGHT_WORST_SURFACE };
            assert_eq!(veil, worst, "{theme}: the worst-case surface constant is out of date");

            let mut surfaces: Vec<(String, Rgb)> =
                ["background", "surface", "raised", "hover", "pressed"].map(solid).to_vec();
            surfaces.push(("veil".into(), veil));
            // `accent-subtle` marks pills and chips on the card, active filters on the page,
            // checked chips in popups and selected service tiles. Hover and pressed replace it
            // rather than layer on it.
            accent_rgba[3] = if dark { 36 } else { 31 };
            let subtle: Vec<(String, Rgb)> = [solid("background"), solid("surface"), ("veil".into(), veil)]
                .iter()
                .map(|(name, color)| (format!("accent-subtle on {name}"), over(accent_rgba, *color)))
                .collect();
            let mut check = |name: &str, color: Rgb, backgrounds: &[(String, Rgb)], minimum: f64| {
                for (background, under) in backgrounds {
                    let ratio = contrast(color, *under);
                    if ratio < minimum {
                        failures.push(format!("{theme}: {name} on {background} is {ratio:.2}:1, below {minimum}:1"));
                    }
                }
            };

            let messages = ["danger-subtle", "warning-subtle"].map(solid);
            let text = opaque(token("text"));
            let muted = opaque(token("text-muted"));
            check("text", text, &surfaces, 4.5);
            check("text", text, &subtle, 4.5);
            check("text", text, &messages, 4.5);
            check("text-muted", muted, &surfaces, 4.5);
            check("text-muted", muted, &messages, 4.5);
            // Muted chevrons on active filter buttons.
            check("text-muted icon", muted, &subtle, 3.0);
            // Danger buttons label in red on ordinary surfaces.
            check("danger", opaque(token("danger")), &surfaces, 4.5);
            // Links, "More" and "Undo" are accent-coloured text.
            check("accent", accent.fill, &surfaces, 4.5);
            check("on-accent", accent.on_fill, &[("accent".into(), accent.fill)], 4.5);
            // Icons and focus rings.
            check("accent icon", accent.fill, &surfaces, 3.0);
            check("accent icon", accent.fill, &subtle, 3.0);
            for name in ["star", "success"] {
                check(name, opaque(token(name)), &surfaces, 3.0);
            }
            check("danger icon", opaque(token("danger")), &[solid("danger-subtle")], 3.0);
            check("warning icon", opaque(token("warning")), &[solid("warning-subtle")], 3.0);
        }
        assert!(failures.is_empty(), "contrast below WCAG AA:\n{}", failures.join("\n"));
    }

    #[test]
    fn rust_colours_match_the_theme_file() {
        let tokens = theme_tokens();
        let pair = |name: &str| (opaque(tokens[name].0), opaque(tokens[name].1));
        assert_eq!(pair("background"), (DARK_BACKGROUND, LIGHT_BACKGROUND));
        assert_eq!(pair("text"), (DARK_TEXT, LIGHT_TEXT));
        // The theme file's defaults are what Slint shows before Rust sets the accent.
        assert_eq!(pair("accent").0, DARK_FALLBACK_ACCENT);
        assert_eq!(pair("on-accent").0, ON_DARK_ACCENT);
    }

    #[test]
    fn picks_a_readable_windows_accent() {
        // A green accent palette, as reported by Windows (lightest to darkest).
        let green = [
            Rgb::new(0x5f, 0xff, 0xa5),
            Rgb::new(0x26, 0xff, 0x8e),
            Rgb::new(0x00, 0xe7, 0x75),
            Rgb::new(0x00, 0xcc, 0x6a),
            Rgb::new(0x00, 0xb2, 0x5a),
            Rgb::new(0x00, 0x76, 0x35),
            Rgb::new(0x00, 0x3f, 0x13),
            Rgb::new(0xe3, 0x00, 0x8c),
        ];
        let dark = pick_accent(Some(green), true);
        assert_eq!((dark.fill, dark.from_system), (green[1], true));
        // Dark 1 is too light on a light background, so Dark 2 is used.
        let light = pick_accent(Some(green), false);
        assert_eq!((light.fill, light.from_system), (green[5], true));

        // A palette with no readable shade falls back to blue.
        let grey = [Rgb::new(0x80, 0x80, 0x80); 8];
        assert!(!pick_accent(Some(grey), true).from_system);
    }
}

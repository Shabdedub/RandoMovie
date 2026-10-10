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
/// The surface the accent must stand out from: the lightest dark surface and the darkest
/// light one (tiles and inputs).
const DARK_RAISED: Rgb = Rgb::new(0x1f, 0x24, 0x30);
const LIGHT_RAISED: Rgb = Rgb::new(0xf3, 0xf4, 0xf7);
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
        (DARK_RAISED, ON_DARK_ACCENT, DARK_FALLBACK_ACCENT, [1, 2, 0])
    } else {
        (LIGHT_RAISED, ON_LIGHT_ACCENT, LIGHT_FALLBACK_ACCENT, [4, 5, 3])
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

    #[test]
    fn design_tokens_meet_aa() {
        let dark_muted = Rgb::new(0xa3, 0xaa, 0xb8);
        assert!(contrast(DARK_TEXT, DARK_BACKGROUND) >= 7.0);
        assert!(contrast(dark_muted, DARK_RAISED) >= 4.5);
        let light_muted = Rgb::new(0x4d, 0x55, 0x66);
        assert!(contrast(LIGHT_TEXT, LIGHT_BACKGROUND) >= 7.0);
        assert!(contrast(light_muted, LIGHT_BACKGROUND) >= 4.5);
        for dark in [true, false] {
            let accent = pick_accent(None, dark);
            assert!(!accent.from_system);
            assert!(contrast(accent.on_fill, accent.fill) >= 4.5);
        }
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

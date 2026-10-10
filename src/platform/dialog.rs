//! A plain error box for failures before the window exists (there is no console).

use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::HSTRING;

pub fn show_error(title: &str, message: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            &HSTRING::from(title),
            MB_OK | MB_ICONERROR,
        );
    }
}

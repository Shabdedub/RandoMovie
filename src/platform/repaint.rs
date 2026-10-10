//! Detects when Windows asked the window to repaint part of itself.
//!
//! Slint 1.18.1's software renderer presents only the regions it changed, so pixels that
//! Windows discards (after a snap, a restore from minimized, a grow) can stay unpainted
//! (slint-ui/slint#13652, #13537). Upstream fixed this on master with `GetUpdateRect`
//! (slint-ui/slint#13568); until a release has it, the app checks the same thing and forces
//! a full repaint.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::GetUpdateRect;

/// True while the window has an update region from the system. Only meaningful during
/// `RedrawRequested`, before the frame is presented.
pub fn system_invalidated(hwnd: isize) -> bool {
    unsafe { GetUpdateRect(HWND(hwnd as *mut c_void), None, false).as_bool() }
}

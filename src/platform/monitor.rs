//! Monitors: DPI awareness, the primary work area, and whether a saved position is still
//! on a connected monitor.

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromRect,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};

/// Makes Win32 report physical pixels. Call before any window exists; winit asks for the
/// same awareness when it starts.
pub fn enable_per_monitor_dpi() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// A monitor's work area (excluding the taskbar) in physical pixels, and its scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

pub fn primary_work_area() -> Option<WorkArea> {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let work = info.rcWork;
        Some(WorkArea {
            x: work.left,
            y: work.top,
            width: (work.right - work.left).max(0) as u32,
            height: (work.bottom - work.top).max(0) as u32,
            scale: dpi_x.max(96) as f32 / 96.0,
        })
    }
}

/// True when the top strip of a window at (x, y) of this width, where the title bar is,
/// overlaps some monitor. Coordinates are physical pixels.
pub fn title_bar_visible(x: i32, y: i32, width: u32) -> bool {
    let width = width.min(16_384) as i32;
    let rect = RECT {
        left: x,
        top: y,
        right: x.saturating_add(width),
        bottom: y.saturating_add(32),
    };
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) };
    !monitor.is_invalid()
}

#[cfg(test)]
mod tests {
    #[test]
    fn far_away_positions_are_off_screen() {
        assert!(!super::title_bar_visible(-1_000_000, -1_000_000, 800));
    }

    #[test]
    fn primary_work_area_is_usable() {
        let area = super::primary_work_area().expect("a primary monitor");
        assert!(area.width >= 640 && area.height >= 400);
        assert!(area.scale >= 1.0);
        assert!(super::title_bar_visible(area.x + 10, area.y + 10, 200));
    }
}

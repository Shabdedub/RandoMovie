//! Opening web links in the default browser.

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// Opens an `https://` (or `http://`) URL on a short-lived thread, since launching the
/// browser can take a moment. Other schemes are refused. Returns false if refused.
pub fn open_url(url: &str) -> bool {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    let url = url.to_owned();
    std::thread::Builder::new()
        .name("open-url".into())
        .spawn(move || unsafe {
            // ShellExecute may hand the URL to a COM-based handler.
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let target = HSTRING::from(url.as_str());
            ShellExecuteW(
                None,
                w!("open"),
                &target,
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
            if com.is_ok() {
                CoUninitialize();
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn refuses_other_schemes() {
        assert!(!super::open_url("file:///C:/Windows/System32/calc.exe"));
        assert!(!super::open_url("javascript:alert(1)"));
    }
}

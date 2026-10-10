//! Connectivity change notifications, so the app leaves offline mode without polling.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::{HANDLE, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::NotifyNetworkConnectivityHintChange;
use windows::Win32::Networking::WinSock::{
    NL_NETWORK_CONNECTIVITY_HINT, NetworkConnectivityLevelHintConstrainedInternetAccess,
    NetworkConnectivityLevelHintInternetAccess,
};

type Callback = Box<dyn Fn(bool) + Send + Sync>;

static CALLBACK: OnceLock<Callback> = OnceLock::new();

unsafe extern "system" fn on_hint_change(_context: *const c_void, hint: NL_NETWORK_CONNECTIVITY_HINT) {
    let level = hint.ConnectivityLevel;
    let online = level == NetworkConnectivityLevelHintInternetAccess
        || level == NetworkConnectivityLevelHintConstrainedInternetAccess;
    if let Some(callback) = CALLBACK.get() {
        callback(online);
    }
}

/// Calls `callback(online)` from a system thread whenever Windows' connectivity hint
/// changes. Only the first registration in a process takes effect. Returns false if
/// Windows refused the subscription.
pub fn watch(callback: impl Fn(bool) + Send + Sync + 'static) -> bool {
    if CALLBACK.set(Box::new(callback)).is_err() {
        return false;
    }
    let mut handle = HANDLE::default();
    let status = unsafe {
        NotifyNetworkConnectivityHintChange(Some(on_hint_change), None, false, &mut handle)
    };
    // The subscription lives until the process exits.
    status == NO_ERROR
}

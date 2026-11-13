//! The OS telling us the machine's network changed: an interface or address added or removed
//! (Wi-Fi joined or lost, a VPN up or down). The Voice engine and the Lobby's connection each
//! start one, with what to do about it.
//!
//! When the OS refuses to report, `start` returns `None` and both users fall back to the
//! connection's stale-heartbeat and resume probes, and ICE noticing a dead path.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod win;

#[cfg(target_os = "linux")]
pub(crate) use linux::NetworkWatch;
#[cfg(windows)]
pub(crate) use win::NetworkWatch;

/// While it lives, `on_change` runs after every change. Dropping it stops the reports. Where
/// nothing is reported there is no value of this type, and `start` returns `None`.
#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) enum NetworkWatch {}

#[cfg(not(any(windows, target_os = "linux")))]
impl NetworkWatch {
    pub(crate) fn start(_on_change: impl Fn() + Send + Sync + 'static) -> Option<NetworkWatch> {
        None
    }
}

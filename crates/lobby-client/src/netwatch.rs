//! The OS telling us the machine's network changed: an interface or address added or removed
//! (Wi-Fi joined or lost, a VPN up or down). The Voice engine and the Lobby's connection each
//! start one, with what to do about it.
//!
//! When the OS refuses to report, `start` returns `None` and both users fall back to the
//! connection's stale-heartbeat and resume probes, and ICE noticing a dead path.

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub(crate) use win::NetworkWatch;

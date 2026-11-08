#![allow(unsafe_code)]

use std::ffi::c_void;

use tracing::warn;
use windows::Win32::{
    Foundation::{HANDLE, NO_ERROR},
    NetworkManagement::IpHelper::{
        CancelMibChangeNotify2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, MibAddInstance,
        MibDeleteInstance, NotifyIpInterfaceChange,
    },
    Networking::WinSock::AF_UNSPEC,
};

type OnChange = Box<dyn Fn() + Send + Sync>;

/// While it lives, `on_change` runs after every interface or address added or removed, on a
/// Windows thread-pool thread, so it must not block. Dropping it cancels the registration.
pub(crate) struct NetworkWatch {
    handle: HANDLE,
    on_change: *mut OnChange,
}

// SAFETY: the registration handle may be cancelled from any thread, and `on_change` is a
// `Send + Sync` closure that only this watch and its callback touch.
unsafe impl Send for NetworkWatch {}
unsafe impl Sync for NetworkWatch {}

impl NetworkWatch {
    pub(crate) fn start(on_change: impl Fn() + Send + Sync + 'static) -> Option<NetworkWatch> {
        let on_change = Box::into_raw(Box::new(Box::new(on_change) as OnChange));
        let mut handle = HANDLE::default();
        // SAFETY: `on_change` stays allocated until `drop` has cancelled the registration;
        // `handle` is a live out pointer.
        let registered = unsafe {
            NotifyIpInterfaceChange(
                AF_UNSPEC,
                Some(on_changed),
                Some(on_change.cast_const().cast()),
                false,
                &raw mut handle,
            )
        };
        if registered != NO_ERROR {
            warn!(code = registered.0, "network changes not watched");
            // SAFETY: not registered, so nothing else holds the pointer.
            drop(unsafe { Box::from_raw(on_change) });
            return None;
        }
        Some(NetworkWatch { handle, on_change })
    }
}

impl Drop for NetworkWatch {
    fn drop(&mut self) {
        // SAFETY: the handle came from NotifyIpInterfaceChange.
        let cancelled = unsafe { CancelMibChangeNotify2(self.handle) };
        if cancelled == NO_ERROR {
            // SAFETY: cancelled, so no callback runs and nothing borrows the box.
            drop(unsafe { Box::from_raw(self.on_change) });
        } else {
            // A callback may still run: leak the box rather than free what it reads.
            warn!(
                code = cancelled.0,
                "network watch not cancelled; its callback is leaked"
            );
        }
    }
}

unsafe extern "system" fn on_changed(
    context: *const c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    kind: MIB_NOTIFICATION_TYPE,
) {
    // A parameter change (metric, MTU) moves no route; the initial notification is no change.
    if kind != MibAddInstance && kind != MibDeleteInstance {
        return;
    }
    // SAFETY: `context` is the closure's box, alive while the registration is.
    let on_change = unsafe { &*context.cast::<OnChange>() };
    on_change();
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use windows::Win32::NetworkManagement::IpHelper::{
        MibInitialNotification, MibParameterNotification,
    };

    use super::*;

    /// The registration succeeds and cancels cleanly. Whether Windows calls back on a real
    /// change is not tested: that needs an adapter to go up or down.
    #[test]
    fn the_network_watch_registers_and_cancels() {
        let watch = NetworkWatch::start(|| {});
        assert!(watch.is_some());
        drop(watch);
    }

    /// Windows' callback runs the closure for an added or removed interface, and for no other
    /// kind of notification.
    #[test]
    fn only_an_interface_added_or_removed_counts_as_a_change() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let on_change: OnChange = Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let context = (&raw const on_change).cast::<c_void>();
        for (kind, runs) in [
            (MibParameterNotification, 0),
            (MibInitialNotification, 0),
            (MibAddInstance, 1),
            (MibDeleteInstance, 1),
        ] {
            let before = calls.load(Ordering::SeqCst);
            // SAFETY: `context` points at `on_change`, which outlives the call; the row is unused.
            unsafe { on_changed(context, std::ptr::null(), kind) };
            assert_eq!(calls.load(Ordering::SeqCst) - before, runs, "{kind:?}");
        }
    }
}

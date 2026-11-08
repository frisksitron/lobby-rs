//! The Tokio runtime as a GPUI global. GPUI executors have no Tokio reactor, so reqwest,
//! tungstenite and `tokio::time` futures panic there. Network IO goes through
//! `lobby_client::Client`, which spawns on this runtime.

use std::time::Duration;

use gpui_kit::Global;
use tokio::runtime::{Handle, Runtime};

/// Last resort when the app is torn down with tasks still running.
pub(crate) const DROP_SHUTDOWN: Duration = Duration::from_millis(250);

pub struct TokioRuntime {
    owned: Option<Runtime>,
    handle: Handle,
}

impl Global for TokioRuntime {}

impl TokioRuntime {
    pub fn owned(rt: Runtime) -> Self {
        Self {
            handle: rt.handle().clone(),
            owned: Some(rt),
        }
    }

    pub fn borrowed(handle: Handle) -> Self {
        Self {
            owned: None,
            handle,
        }
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl Drop for TokioRuntime {
    fn drop(&mut self) {
        if let Some(rt) = self.owned.take() {
            rt.shutdown_timeout(DROP_SHUTDOWN);
        }
    }
}

//! The CPU semaphore's work: image decoding takes `App.cpu`, 2 permits, acquired within 5 s.
//! Always run before the write is handed to `crate::domain`.

use std::{sync::Arc, time::Duration};

use tokio::sync::Semaphore;

/// `App.cpu` acquire budget; beyond it the request gets 503 `UNAVAILABLE`.
pub const CPU_ACQUIRE: Duration = Duration::from_secs(5);
pub const CPU_PERMITS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Server is busy. Try again in a moment.")]
pub struct CpuBusy;

pub async fn on_cpu<T: Send + 'static>(
    cpu: &Arc<Semaphore>,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, CpuBusy> {
    let permit = tokio::time::timeout(CPU_ACQUIRE, cpu.clone().acquire_owned())
        .await
        .map_err(|_| CpuBusy)?
        .map_err(|_| CpuBusy)?;
    let out = tokio::task::spawn_blocking(move || {
        let out = f();
        drop(permit);
        out
    })
    .await
    .map_err(|_| CpuBusy)?;
    Ok(out)
}

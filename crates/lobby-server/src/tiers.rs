//! Task tiers. Every server task is spawned through one of them, so shutdown can stop and await
//! each tier in order: http → conns → writes → services.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone, Default)]
pub(crate) struct Tier {
    pub token: CancellationToken,
    pub tracker: TaskTracker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ConnId(pub u64);

#[derive(Clone, Default)]
pub(crate) struct ConnTier {
    pub tier: Tier,
    draining: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
}

impl ConnTier {
    pub fn set_draining(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    pub fn next_id(&self) -> ConnId {
        ConnId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

#[derive(Clone, Default)]
pub(crate) struct Tiers {
    pub http: Tier,
    pub conns: ConnTier,
    /// The writer task. Never cancelled: it ends once writes stop being admitted and the
    /// accepted ones are done.
    pub writes: TaskTracker,
    /// Jobs, mail sends, file deletes and the SFU thread.
    pub services: Tier,
}

impl Tiers {
    pub fn cancel_all(&self) {
        self.http.token.cancel();
        self.conns.tier.token.cancel();
        self.services.token.cancel();
    }
}

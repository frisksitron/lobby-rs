//! How the Lobby (or the connection itself) ends a connection: a [`CloseReason`] and the [`Kick`]
//! that carries it to the connection's tasks.

use std::sync::{Arc, OnceLock};

use lobby_protocol::close;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Shutdown,
    InternalError,
    ProtocolViolation,
    IdentifyTimeout,
    PongTimeout,
    AuthInvalid,
    SessionRevoked,
    Removed,
    SlowConsumer,
    Replaced,
    IdentifyRateLimited,
}

impl CloseReason {
    pub fn code(self) -> u16 {
        match self {
            Self::Shutdown => close::GOING_AWAY,
            Self::InternalError => close::INTERNAL_ERROR,
            Self::ProtocolViolation => close::PROTOCOL_VIOLATION,
            Self::IdentifyTimeout => close::IDENTIFY_TIMEOUT,
            Self::PongTimeout => close::PONG_TIMEOUT,
            Self::AuthInvalid => close::AUTH_INVALID,
            Self::SessionRevoked => close::SESSION_REVOKED,
            Self::Removed => close::REMOVED,
            Self::SlowConsumer => close::SLOW_CONSUMER,
            Self::Replaced => close::REPLACED,
            Self::IdentifyRateLimited => close::IDENTIFY_RATE_LIMITED,
        }
    }
}

/// The per-connection stop signal: a child of the conns-tier token (shutdown kicks every
/// connection with 1001) plus the first reason given.
#[derive(Clone)]
pub struct Kick {
    token: CancellationToken,
    reason: Arc<OnceLock<CloseReason>>,
}

impl Kick {
    pub fn child_of(parent: &CancellationToken) -> Self {
        Self {
            token: parent.child_token(),
            reason: Arc::default(),
        }
    }

    pub fn kick(&self, reason: CloseReason) {
        let _ = self.reason.set(reason);
        self.token.cancel();
    }

    pub fn stop(&self) {
        self.token.cancel();
    }

    /// A cancelled kick without a reason came from the parent: the server is shutting down.
    pub fn reason(&self) -> CloseReason {
        self.reason.get().copied().unwrap_or(CloseReason::Shutdown)
    }

    pub async fn kicked(&self) {
        self.token.cancelled().await;
    }
}

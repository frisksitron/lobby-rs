//! `GET /api/v1/ws`. Each connection is one reader future (in the upgrade task) and one tracked
//! writer task, joined by a bounded outbound queue that the `Lobby` also writes to. An upgrade
//! needs a pre-auth permit (unidentified sockets per client IP and in total), which the connection
//! gives back once it identifies.

mod budget;
mod conn;
mod ink;
pub(crate) mod refusal;
mod writer;

pub use budget::{PreAuthBudget, PreAuthPermit};
pub use ink::InkQueue;

use std::sync::{Arc, OnceLock};

use axum::{
    Extension,
    extract::{
        State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    response::{IntoResponse, Response},
};
use futures_util::StreamExt as _;
use lobby_protocol::close;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, info_span};

use crate::{client_ip::ClientIp, error::ApiError, server::App};

/// A refused upgrade may try again this soon: the cap counts sockets, not a time window.
const PREAUTH_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) async fn upgrade(
    State(app): State<App>,
    Extension(client): Extension<ClientIp>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return ApiError::invalid_request(rejection.body_text()).into_response(),
    };
    let Some(permit) = app.preauth.try_acquire(client) else {
        tracing::debug!(ip = %client.ip(), "too many unidentified sockets");
        return ApiError::rate_limited(
            "Too many connections waiting to identify",
            PREAUTH_RETRY_AFTER,
        )
        .into_response();
    };
    // Taken before the upgrade: axum spawns the upgraded task itself, untracked, so this token
    // is what makes shutdown wait for it.
    let tracked = app.conns.tier.tracker.token();
    let id = app.conns.next_id();
    let max = app.cfg.ws.max_frame_bytes;
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| {
            let span =
                info_span!("ws", conn = id.0, ip = %client.ip(), user = tracing::field::Empty);
            async move {
                let (sink, stream) = socket.split();
                conn::run(app, id, permit, sink, stream).await;
                drop(tracked);
            }
            .instrument(span)
        })
}

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

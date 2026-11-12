//! `GET /api/v1/ws`. Each connection is one reader future (in the upgrade task) and one tracked
//! writer task, joined by a bounded outbound queue that the `Lobby` also writes to. An upgrade
//! needs a pre-auth permit (unidentified sockets per client IP and in total), which the connection
//! gives back once it identifies.

mod budget;
mod conn;
mod writer;

pub use budget::{PreAuthBudget, PreAuthPermit};

use axum::{
    Extension,
    extract::{
        State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    response::{IntoResponse, Response},
};
use futures_util::StreamExt as _;
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

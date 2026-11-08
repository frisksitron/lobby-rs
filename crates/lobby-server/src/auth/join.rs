//! `POST /api/v1/auth/join`: a ticket and a name make the first Member of an empty Lobby
//! (bootstrap) or an Ask to Join; `domain::admission` decides which. An Ask to Join hands back a
//! wait token, which `POST /api/v1/auth/join/status` turns into a session once a Member Vouches.

use axum::{Extension, Json, extract::State};
use lobby_protocol::{JoinOutcome, JoinRequest, JoinStatus, JoinStatusRequest, UnixMs};

use super::tokens;
use crate::{
    auth::tokens::Minted,
    client_ip::ClientIp,
    db::{
        DbError,
        admission::{self, WaitState},
    },
    domain::{Join, WriteError},
    error::{ApiError, AppJson},
    server::App,
};

pub async fn join(
    State(app): State<App>,
    Extension(client): Extension<ClientIp>,
    AppJson(req): AppJson<JoinRequest>,
) -> Result<Json<JoinOutcome>, ApiError> {
    let join = Join {
        ticket_hash: tokens::digest(req.ticket.expose()),
        client,
        username: req.username,
        minted: Minted::new(UnixMs::now(), &app.cfg.auth),
    };
    Ok(Json(app.domain.join(join).await?))
}

/// `POST /api/v1/auth/join/status`: where the request behind a wait token stands. Once Vouched in,
/// the first check spends the token and signs this device in, so Waiting for a Vouch needs no new
/// code. Unknown, spent or expired ⇒ 401 `AUTH_FAILED`.
pub async fn status(
    State(app): State<App>,
    AppJson(req): AppJson<JoinStatusRequest>,
) -> Result<Json<JoinStatus>, ApiError> {
    let now = UnixMs::now();
    let hash = tokens::digest(req.wait_token.expose());
    // Most checks find the request still pending: answered from a reader.
    let state = {
        let mut conn = app.db.readers().acquire().await.map_err(DbError::from)?;
        admission::wait_state(&mut conn, &hash, now).await?
    };
    Ok(Json(match state.ok_or(WriteError::InvalidWait)? {
        WaitState::Pending => JoinStatus::PendingApproval,
        WaitState::Declined => JoinStatus::Declined,
        WaitState::VouchedIn { .. } => {
            let minted = Minted::new(now, &app.cfg.auth);
            app.domain.sign_in_after_vouch(hash, minted).await?
        }
    }))
}

//! Magic Codes: request a code by email and verify it with the request's challenge.
//! `domain::admission` stores the code and answers the verify by where its email stands.

use axum::{Extension, Json, extract::State};
use lobby_protocol::{
    MagicCodeRequest, MagicCodeRequested, MagicCodeVerify, RequestLabel, UnixMs, VerifyOutcome,
};

use super::tokens;
use crate::{
    auth::tokens::Minted,
    client_ip::ClientIp,
    domain::{CodeRequest, Verify},
    error::{ApiError, AppJson},
    server::App,
};

/// `POST /api/v1/auth/magic-code`. A new challenge, known email or not, locked or new Lobby alike,
/// so it cannot tell who is a Member. Over the client's send caps, for this email or any, it is
/// `RATE_LIMITED` instead.
pub async fn request_code(
    State(app): State<App>,
    Extension(client): Extension<ClientIp>,
    AppJson(req): AppJson<MagicCodeRequest>,
) -> Result<Json<MagicCodeRequested>, ApiError> {
    let (challenge, challenge_hash) = tokens::magic_challenge();
    let label = RequestLabel::from_digest(&challenge_hash);
    let request = CodeRequest {
        email: req.email,
        client,
        challenge_hash,
        label: label.clone(),
    };
    app.domain.issue_code(request).await?;
    Ok(Json(MagicCodeRequested { challenge, label }))
}

pub async fn verify_code(
    State(app): State<App>,
    AppJson(req): AppJson<MagicCodeVerify>,
) -> Result<Json<VerifyOutcome>, ApiError> {
    let (ticket, ticket_hash) = tokens::join_ticket();
    let verify = Verify {
        challenge_hash: tokens::digest(req.challenge.expose()),
        code: req.code,
        minted: Minted::new(UnixMs::now(), &app.cfg.auth),
        ticket,
        ticket_hash,
    };
    Ok(Json(app.domain.verify_code(verify).await?))
}

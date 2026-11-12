//! Auth and sessions: crash-safe refresh rotation, Sign Out, and the access-token check
//! shared by bearer REST and IDENTIFY. Magic Codes and admission (join, bootstrap, Ask to Join)
//! live in the submodules; Vouch and Decline in `crate::access`. Their writes are
//! `crate::domain`'s.

pub(crate) mod join;
pub(crate) mod magic;

use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{StatusCode, header, request::Parts},
};
use lobby_protocol::{ErrorCode, RefreshRequest, SessionTokens, SignOutRequest, UnixMs};
use tracing::info;

use crate::{
    db::{DbError, sessions},
    domain::{Authenticated, Refreshed, WriteError},
    error::{ApiError, AppJson},
    server::App,
    tokens::{self, Minted},
};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Invalid or expired access token")]
    InvalidToken,
    #[error("Access token expired")]
    AccessExpired,
    #[error("Invalid refresh token")]
    InvalidRefresh,
    #[error("Refresh token has expired")]
    RefreshExpired,
    #[error(transparent)]
    Db(#[from] DbError),
    /// The first use of a token pair is a write of its own.
    #[error(transparent)]
    Write(#[from] WriteError),
}

impl AuthError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidToken | Self::InvalidRefresh => ErrorCode::AuthFailed,
            Self::AccessExpired | Self::RefreshExpired => ErrorCode::AuthExpired,
            Self::Write(WriteError::Unavailable) => ErrorCode::Unavailable,
            Self::Db(_) | Self::Write(_) => ErrorCode::InternalError,
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> ApiError {
        let e = match e {
            AuthError::Db(db) => return db.into(),
            AuthError::Write(w) => return w.into(),
            e => e,
        };
        ApiError::new(StatusCode::UNAUTHORIZED, e.code(), e.to_string())
    }
}

/// Unknown or revoked ⇒ `AUTH_FAILED`, expired ⇒ `AUTH_EXPIRED`; the first use of a pair is
/// recorded, which ends its lost-response grace.
pub async fn authenticate(app: &App, token: &str) -> Result<Authenticated, AuthError> {
    let now = UnixMs::now();
    let access = tokens::digest(token);
    let row = sessions::by_access_hash(&app.db, &access)
        .await?
        .ok_or(AuthError::InvalidToken)?;
    if row.access_expires_at <= now {
        return Err(AuthError::AccessExpired);
    }
    if !row.used {
        app.domain.mark_used(access).await?;
    }
    Ok(Authenticated {
        session: row.session,
        user: row.user,
        access_expires_at: row.access_expires_at,
    })
}

pub struct AuthUser(pub Authenticated);

impl FromRequestParts<App> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AuthError::InvalidToken)?;
        Ok(AuthUser(authenticate(app, token.trim()).await?))
    }
}

/// `POST /api/v1/auth/sign-out`: this session, or with `everywhere` every session of the User.
pub async fn sign_out(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    AppJson(req): AppJson<SignOutRequest>,
) -> Result<StatusCode, ApiError> {
    app.domain.sign_out(me, req.everywhere).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn refresh(
    State(app): State<App>,
    AppJson(req): AppJson<RefreshRequest>,
) -> Result<Json<SessionTokens>, ApiError> {
    let minted = Minted::new(UnixMs::now(), &app.cfg.auth);
    let presented = tokens::digest(req.refresh_token.expose());
    let (outcome, tokens) = app.domain.refresh(presented, minted).await?;
    match outcome {
        Refreshed::Rotated { session, user } => {
            info!(%user, session = session.0, "refresh: rotated");
            Ok(Json(tokens))
        }
        Refreshed::Reissued { session, user } => {
            info!(%user, session = session.0, "refresh: reissued after a lost response");
            Ok(Json(tokens))
        }
        Refreshed::ReuseDetected { .. } | Refreshed::Unknown => {
            Err(AuthError::InvalidRefresh.into())
        }
        Refreshed::Expired => Err(AuthError::RefreshExpired.into()),
    }
}

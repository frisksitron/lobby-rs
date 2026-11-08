use std::collections::HashSet;

use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
};
use lobby_protocol::{
    HeldHistory, MessageChanges, MessageDelete, MessageEdit, MessagePage, MessageSend,
    ReadyHistory, Rid, UserId, content, limits,
    routes::{ChangesQuery, HistoryQuery},
};
use tokio::time::Instant;
use tracing::debug;

use crate::{
    auth::AuthUser,
    db::{
        DbError,
        messages::{self, Page},
    },
    domain::WriteError,
    error::ApiError,
    lobby::HistoryRead,
    server::App,
    tiers::ConnId,
};

/// `MESSAGE_SEND` rids become nonces, so they must be long and random.
const NONCE_LEN: std::ops::RangeInclusive<usize> = 16..=64;

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("MESSAGE_SEND needs a rid of 16-64 characters")]
    BadNonce,
    #[error("Too many attachments")]
    TooManyAttachments,
    #[error("Message is too long")]
    TooLong,
    #[error(transparent)]
    TooNested(#[from] content::ContentError),
    #[error("You are sending Messages too quickly")]
    RateLimited(Instant),
    #[error("Message is empty")]
    Empty,
    #[error(transparent)]
    Write(#[from] WriteError),
}

pub async fn send(
    app: &App,
    user: &UserId,
    conn: ConnId,
    rid: Option<&Rid>,
    cmd: MessageSend,
) -> Result<(), ChatError> {
    let nonce = rid
        .filter(|r| NONCE_LEN.contains(&r.as_str().len()))
        .ok_or(ChatError::BadNonce)?;
    let mut attachments = cmd.attachment_ids;
    let mut seen = HashSet::new();
    attachments.retain(|id| seen.insert(id.clone()));
    if attachments.len() > limits::ATTACHMENTS_MAX {
        return Err(ChatError::TooManyAttachments);
    }
    if cmd.content.chars().count() > limits::MESSAGE_MAX_CHARS {
        return Err(ChatError::TooLong);
    }
    if app.limits.enabled() {
        app.lobby
            .try_send_slot(user, Instant::now())
            .map_err(ChatError::RateLimited)?;
    }
    let content = cmd.content;
    if content.trim().is_empty() && attachments.is_empty() {
        return Err(ChatError::Empty);
    }
    content::parse(&content)?;
    app.domain
        .send_message(user.clone(), conn, nonce.clone(), content, attachments)
        .await?;
    Ok(())
}

/// A client that missed more Messages than this gets the newest page in READY instead of what
/// follows its newest one: the feed starts over rather than fill a long gap page by page.
pub const BACKFILL_MAX: u64 = 500;

/// The history READY carries for a client that holds `held`. Read before the registration, so a
/// Message committed in between is the client's to fetch, below READY's `last_message_seq`, and
/// an edit or a deletion is above the revision read with the page.
pub async fn ready_history(app: &App, held: Option<&HeldHistory>) -> Result<HistoryRead, DbError> {
    let (instance, last_seq) = app.lobby.history_point();
    let after = held
        .filter(|h| h.instance_id == instance)
        .map(|h| h.newest)
        .filter(|newest| last_seq.saturating_sub(newest.get() as u64) <= BACKFILL_MAX);
    let (page, limit) = match after {
        Some(newest) => (Page::After(newest), limits::HISTORY_MAX),
        None => (Page::Latest, limits::HISTORY_DEFAULT),
    };
    let (messages, has_more, rev) = messages::page_at_rev(&app.db, page, limit).await?;
    let page = MessagePage { messages, has_more };
    let history = match after {
        Some(_) => ReadyHistory::After(page),
        None => ReadyHistory::Latest(page),
    };
    Ok(HistoryRead { history, rev })
}

/// Validates one `MESSAGE_EDIT` as a send's text is, and spends a send slot. Edited to nothing,
/// a Message is refused even if it has attachments: Delete is for that. The terminal `ACK` is
/// the caller's, after the `MESSAGE_UPDATE`.
pub async fn edit(app: &App, user: &UserId, cmd: MessageEdit) -> Result<(), ChatError> {
    if cmd.content.chars().count() > limits::MESSAGE_MAX_CHARS {
        return Err(ChatError::TooLong);
    }
    if app.limits.enabled() {
        app.lobby
            .try_send_slot(user, Instant::now())
            .map_err(ChatError::RateLimited)?;
    }
    if cmd.content.trim().is_empty() {
        return Err(ChatError::Empty);
    }
    content::parse(&cmd.content)?;
    app.domain
        .edit_message(user.clone(), cmd.message_id, cmd.content)
        .await?;
    Ok(())
}

pub async fn delete(app: &App, user: &UserId, cmd: MessageDelete) -> Result<(), ChatError> {
    app.domain
        .delete_message(user.clone(), cmd.message_id)
        .await?;
    Ok(())
}

pub async fn history(
    State(app): State<App>,
    AuthUser(who): AuthUser,
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> Result<Json<MessagePage>, ApiError> {
    let Query(query) = query.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let limit = match query.limit {
        None => limits::HISTORY_DEFAULT,
        Some(l) if (1..=limits::HISTORY_MAX).contains(&l) => l,
        Some(_) => {
            return Err(ApiError::invalid_request(
                "Query parameter 'limit' must be between 1 and 100",
            ));
        }
    };
    let page = match (query.before, query.after) {
        (Some(_), Some(_)) => {
            return Err(ApiError::invalid_request("Use either 'before' or 'after'"));
        }
        (Some(before), None) => Page::Before(before),
        (None, Some(after)) => Page::After(after),
        (None, None) => Page::Latest,
    };
    let (messages, has_more) = messages::page(&app.db, page, limit).await?;
    debug!(user = %who.user, ?page, limit, returned = messages.len(), "history page");
    Ok(Json(MessagePage { messages, has_more }))
}

pub async fn changes(
    State(app): State<App>,
    AuthUser(who): AuthUser,
    query: Result<Query<ChangesQuery>, QueryRejection>,
) -> Result<Json<MessageChanges>, ApiError> {
    let Query(query) = query.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let changes = messages::changes(&app.db, query.after).await?;
    debug!(
        user = %who.user,
        after = query.after,
        updated = changes.updated.len(),
        removed = changes.removed.len(),
        "message changes"
    );
    Ok(Json(changes))
}

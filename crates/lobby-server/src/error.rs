//! The REST edge of the error taxonomy: domain errors become an `ApiError` exactly once, which
//! renders the JSON envelope and does the logging.

use std::{borrow::Cow, time::Duration};

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Path, Request, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use lobby_protocol::{ErrorBody, ErrorCode, ErrorDetail, ErrorEnvelope};
use serde::de::DeserializeOwned;

use crate::{blobs::BlobError, db::DbError, domain::WriteError};

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ErrorCode,
    pub message: Cow<'static, str>,
    pub detail: Option<ErrorDetail>,
    pub retry_after: Option<Duration>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            detail: None,
            retry_after: None,
        }
    }

    pub fn conflict(detail: ErrorDetail, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            detail: Some(detail),
            ..Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, message)
        }
    }

    pub fn unauthorized(code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    pub fn rate_limited(message: impl Into<Cow<'static, str>>, retry_after: Duration) -> Self {
        Self {
            retry_after: Some(retry_after),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::RateLimited,
                message,
            )
        }
    }

    pub fn invalid_request(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, message)
    }

    pub fn forbidden(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, message)
    }

    pub fn forbidden_because(detail: ErrorDetail, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            detail: Some(detail),
            ..Self::forbidden(message)
        }
    }

    pub fn not_found(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
    }

    pub fn unavailable(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            message,
        )
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::InternalError,
            "An internal error occurred",
        )
    }

    /// The envelope for a 4xx/5xx that no handler produced (timeouts, body limits, extractor
    /// rejections), keyed by status alone.
    pub fn for_status(status: StatusCode) -> Self {
        let (code, message) = match status {
            StatusCode::BAD_REQUEST => (ErrorCode::InvalidRequest, "Invalid request"),
            StatusCode::UNAUTHORIZED => (ErrorCode::AuthFailed, "Authentication required"),
            StatusCode::FORBIDDEN => (ErrorCode::Forbidden, "Forbidden"),
            StatusCode::NOT_FOUND => (ErrorCode::NotFound, "Not found"),
            StatusCode::METHOD_NOT_ALLOWED => (ErrorCode::InvalidRequest, "Method not allowed"),
            StatusCode::REQUEST_TIMEOUT => (ErrorCode::Unavailable, "Request timed out"),
            StatusCode::PAYLOAD_TOO_LARGE => {
                (ErrorCode::PayloadTooLarge, "Request body is too large")
            }
            StatusCode::TOO_MANY_REQUESTS => (ErrorCode::RateLimited, "Too many requests"),
            StatusCode::SERVICE_UNAVAILABLE => (ErrorCode::Unavailable, "Service unavailable"),
            s if s.is_server_error() => (ErrorCode::InternalError, "An internal error occurred"),
            _ => (ErrorCode::InvalidRequest, "Invalid request"),
        };
        Self::new(status, code, message)
    }
}

/// The cause is logged here, once, with the request's span; the client sees the fixed copy.
impl From<DbError> for ApiError {
    fn from(e: DbError) -> ApiError {
        tracing::error!(error = %e, "database error");
        match e {
            DbError::Timeout | DbError::Busy => ApiError::unavailable("Server is busy. Try again."),
            _ => ApiError::internal(),
        }
    }
}

impl From<WriteError> for ApiError {
    fn from(e: WriteError) -> ApiError {
        use WriteError as E;
        let message = e.to_string();
        match e {
            E::Db(db) => db.into(),
            E::Panicked => ApiError::internal(),
            E::Unavailable | E::Busy => ApiError::unavailable(message),
            E::CodesCapped(retry_after) => ApiError::rate_limited(message, retry_after),
            E::TooManyAttempts => ApiError {
                detail: Some(ErrorDetail::TooManyAttempts),
                ..ApiError::unauthorized(ErrorCode::AuthFailed, message)
            },
            E::InvalidCode | E::NoLiveCode | E::InvalidTicket | E::InvalidWait | E::Removed => {
                ApiError::unauthorized(ErrorCode::AuthFailed, message)
            }
            E::CodeExpired | E::TicketExpired => {
                ApiError::unauthorized(ErrorCode::AuthExpired, message)
            }
            E::NotYourMessage => ApiError::forbidden(message),
            // The client says these in its own words.
            E::NotSetUp => ApiError::forbidden_because(ErrorDetail::NotSetUp, message),
            E::Locked => ApiError::forbidden_because(ErrorDetail::Locked, message),
            E::TooManyRequests => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::LimitReached,
                message,
            ),
            E::NotOwner => ApiError::forbidden_because(ErrorDetail::NotOwner, message),
            E::UsernameTaken => ApiError::conflict(ErrorDetail::UsernameTaken, message),
            E::EmailTaken => ApiError::conflict(ErrorDetail::EmailTaken, message),
            E::CannotRemoveOwner | E::OwnerCannotLeave => {
                ApiError::conflict(ErrorDetail::CannotRemoveOwner, message)
            }
            E::AttachmentInvalid => ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::AttachmentInvalid,
                message,
            ),
            E::RequestNotFound | E::NotAMember | E::MessageMissing => ApiError::not_found(message),
        }
    }
}

impl From<BlobError> for ApiError {
    fn from(e: BlobError) -> ApiError {
        match e {
            BlobError::TooLarge => ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorCode::PayloadTooLarge,
                e.to_string(),
            ),
            BlobError::Empty
            | BlobError::Executable
            | BlobError::Unsupported
            | BlobError::InvalidImage => ApiError::invalid_request(e.to_string()),
            BlobError::Busy(_) => ApiError::unavailable(e.to_string()),
            BlobError::Aborted(_) => {
                tracing::debug!(error = %e, "upload cancelled by the client");
                ApiError::invalid_request("The upload was interrupted")
            }
            BlobError::Io(_) | BlobError::BadPath(_) => {
                tracing::error!(error = %e, "blob store error");
                ApiError::internal()
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(status = %self.status, code = ?self.code, message = %self.message, "request failed");
        } else {
            tracing::debug!(status = %self.status, code = ?self.code, message = %self.message, "request rejected");
        }
        let retry = self.retry_after;
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message.into_owned(),
                detail: self.detail,
                rid: None,
                retry_after_ms: retry.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(d) = retry {
            // Whole seconds, rounded up, at least 1 (RFC 9110).
            let secs = d.as_millis().div_ceil(1000).max(1);
            headers.insert(header::RETRY_AFTER, HeaderValue::from(secs as u64));
        }
        response
    }
}

/// `Json<T>` whose rejection is our 400 envelope with serde's reason.
pub struct AppJson<T>(pub T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(AppJson(value)),
            Err(rejection) => Err(json_rejection(&rejection)),
        }
    }
}

/// `Path<T>` whose rejection is our 400 envelope with the id's parse error.
pub struct AppPath<T>(pub T);

impl<S, T> FromRequestParts<S> for AppPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(AppPath(value)),
            Err(rejection) => Err(ApiError::invalid_request(rejection.body_text())),
        }
    }
}

fn json_rejection(rejection: &JsonRejection) -> ApiError {
    match rejection {
        JsonRejection::JsonDataError(e) => {
            // "Failed to deserialize the JSON body into the target type: username: <reason> at
            // line 1 column 9": keep the field and the reason.
            let text = e.body_text();
            let reason = text
                .split_once(": ")
                .map_or(text.as_str(), |(_, rest)| rest)
                .split(" at line ")
                .next()
                .expect("split always yields a first element")
                .to_owned();
            ApiError::invalid_request(reason)
        }
        JsonRejection::MissingJsonContentType(_) => {
            ApiError::invalid_request("Expected a JSON request body")
        }
        other => ApiError::invalid_request(other.body_text()),
    }
}

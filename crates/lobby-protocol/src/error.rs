use serde::{Deserialize, Serialize};

use crate::Rid;

/// Stable wire codes shared by REST and WS. The spelling is part of the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    AuthFailed,
    AuthExpired,
    Forbidden,
    InvalidRequest,
    PayloadTooLarge,
    NotFound,
    Conflict,
    RateLimited,
    LimitReached,
    Unavailable,
    InternalError,
    AttachmentInvalid,
    MessageTooLong,
    MessageTooNested,
    VoiceJoinCooldown,
    VoiceStateCooldown,
    VoiceJoinFailed,
    NotInVoice,
    VoiceStateInvalidTransition,
    VoiceNegotiationInvalidState,
    VoiceNegotiationFailed,
    VoiceNegotiationTimeout,
    SignalingRateLimited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorDetail {
    UsernameTaken,
    /// `CONFLICT`: a Member already uses the email.
    EmailTaken,
    /// `FORBIDDEN`: the Lobby takes no new Members.
    Locked,
    /// `FORBIDDEN`: the Lobby has no Members yet and this is not its bootstrap email.
    NotSetUp,
    /// `FORBIDDEN`: only the Lobby's owner may do this.
    NotOwner,
    CannotRemoveOwner,
    /// `AUTH_FAILED`: the code took too many wrong tries and is spent.
    TooManyAttempts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ErrorDetail>,
    /// The refused command's rid (WS only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<Rid>,
    /// How long until a retry is accepted, relative to the server clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>, rid: Option<Rid>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            rid,
            retry_after_ms: None,
        }
    }
}

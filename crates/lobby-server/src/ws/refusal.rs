//! A refusal on the WebSocket in the wire's words: the one place a domain outcome of a command
//! (`ChatError`, `VoiceRefusal`) becomes an `ErrorCode` and an `ERROR` frame. The REST routes have
//! their own edge, `error.rs`.

use lobby_protocol::{ErrorBody, ErrorCode, Rid};
use tokio::time::Instant;

use crate::{chat::ChatError, domain::WriteError, limits::retry_error, voice::VoiceRefusal};

pub(crate) fn chat(e: &ChatError, rid: Option<Rid>, now: Instant) -> ErrorBody {
    let code = match e {
        ChatError::BadNonce | ChatError::TooManyAttachments | ChatError::Empty => {
            ErrorCode::InvalidRequest
        }
        ChatError::TooLong => ErrorCode::MessageTooLong,
        ChatError::TooNested(_) => ErrorCode::MessageTooNested,
        ChatError::RateLimited(_) => ErrorCode::RateLimited,
        ChatError::Write(WriteError::AttachmentInvalid) => ErrorCode::AttachmentInvalid,
        ChatError::Write(WriteError::Removed) => ErrorCode::AuthFailed,
        ChatError::Write(WriteError::Unavailable) => ErrorCode::Unavailable,
        ChatError::Write(WriteError::MessageMissing) => ErrorCode::NotFound,
        ChatError::Write(WriteError::NotYourMessage) => ErrorCode::Forbidden,
        ChatError::Write(_) => ErrorCode::InternalError,
    };
    let message = match e {
        ChatError::Write(WriteError::Db(_)) => "An internal error occurred".to_owned(),
        e => e.to_string(),
    };
    match e {
        ChatError::RateLimited(retry_at) => retry_error(code, &message, rid, *retry_at, now),
        _ => ErrorBody::new(code, message, rid),
    }
}

/// The `ERROR` for a Voice command that was refused, or for a session that ended in an error.
pub(crate) fn voice(refusal: &VoiceRefusal, rid: Option<Rid>, now: Instant) -> ErrorBody {
    let code = match refusal {
        VoiceRefusal::JoinCooldown(_) => ErrorCode::VoiceJoinCooldown,
        VoiceRefusal::AlreadyInVoice | VoiceRefusal::NotConnected => {
            ErrorCode::VoiceStateInvalidTransition
        }
        VoiceRefusal::VoiceFull | VoiceRefusal::ShareLimit => ErrorCode::LimitReached,
        VoiceRefusal::JoinFailed => ErrorCode::VoiceJoinFailed,
        VoiceRefusal::Timeout | VoiceRefusal::ShareTimeout => ErrorCode::VoiceNegotiationTimeout,
        VoiceRefusal::UnmuteCooldown(_) => ErrorCode::VoiceStateCooldown,
        VoiceRefusal::StaleAnswer | VoiceRefusal::RestartInvalid => {
            ErrorCode::VoiceNegotiationInvalidState
        }
        VoiceRefusal::NegotiationFailed => ErrorCode::VoiceNegotiationFailed,
        VoiceRefusal::NotInVoice => ErrorCode::NotInVoice,
        VoiceRefusal::ShareNotAvailable => ErrorCode::NotFound,
        VoiceRefusal::WatchSelf => ErrorCode::InvalidRequest,
    };
    let message = refusal.to_string();
    match refusal.retry_at() {
        Some(at) => retry_error(code, &message, rid, at, now),
        None => ErrorBody::new(code, message, rid),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn rid() -> Option<Rid> {
        Some("r1".parse().unwrap())
    }

    /// Each refusal's code, and whether it says when to try again.
    #[test]
    fn every_voice_refusal_has_its_code_and_a_cooldown_says_when_it_ends() {
        let now = Instant::now();
        let ends = now + Duration::from_secs(15);
        let table = [
            (
                VoiceRefusal::JoinCooldown(ends),
                ErrorCode::VoiceJoinCooldown,
            ),
            (
                VoiceRefusal::UnmuteCooldown(ends),
                ErrorCode::VoiceStateCooldown,
            ),
            (
                VoiceRefusal::AlreadyInVoice,
                ErrorCode::VoiceStateInvalidTransition,
            ),
            (
                VoiceRefusal::NotConnected,
                ErrorCode::VoiceStateInvalidTransition,
            ),
            (VoiceRefusal::VoiceFull, ErrorCode::LimitReached),
            (VoiceRefusal::ShareLimit, ErrorCode::LimitReached),
            (VoiceRefusal::JoinFailed, ErrorCode::VoiceJoinFailed),
            (VoiceRefusal::Timeout, ErrorCode::VoiceNegotiationTimeout),
            (
                VoiceRefusal::ShareTimeout,
                ErrorCode::VoiceNegotiationTimeout,
            ),
            (
                VoiceRefusal::StaleAnswer,
                ErrorCode::VoiceNegotiationInvalidState,
            ),
            (
                VoiceRefusal::RestartInvalid,
                ErrorCode::VoiceNegotiationInvalidState,
            ),
            (
                VoiceRefusal::NegotiationFailed,
                ErrorCode::VoiceNegotiationFailed,
            ),
            (VoiceRefusal::NotInVoice, ErrorCode::NotInVoice),
            (VoiceRefusal::ShareNotAvailable, ErrorCode::NotFound),
            (VoiceRefusal::WatchSelf, ErrorCode::InvalidRequest),
        ];
        for (refusal, code) in table {
            let body = voice(&refusal, rid(), now);
            assert_eq!(body.code, code, "{refusal:?}");
            assert_eq!(body.message, refusal.to_string(), "{refusal:?}");
            assert_eq!(body.rid, rid());
            let cooldown = matches!(
                refusal,
                VoiceRefusal::JoinCooldown(_) | VoiceRefusal::UnmuteCooldown(_)
            );
            assert_eq!(
                body.retry_after_ms,
                cooldown.then_some(15_000),
                "{refusal:?}"
            );
        }
    }

    #[test]
    fn a_send_over_the_throttle_says_when_to_try_again() {
        let now = Instant::now();
        let body = chat(
            &ChatError::RateLimited(now + Duration::from_millis(400)),
            rid(),
            now,
        );
        assert_eq!(body.code, ErrorCode::RateLimited);
        assert_eq!(body.retry_after_ms, Some(400));
        let body = chat(&ChatError::TooLong, rid(), now);
        assert_eq!(body.code, ErrorCode::MessageTooLong);
        assert_eq!(body.retry_after_ms, None);
    }
}

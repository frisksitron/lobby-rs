//! The Lobby protocol: the REST and WebSocket contract shared by `lobby-server` and
//! `lobby-client`: serde types, the routes and the Message Markdown dialect. It never does IO.

pub mod auth;
pub mod close;
pub mod content;
pub mod drawing;
pub mod encoder;
pub mod entities;
pub mod error;
pub mod ids;
pub mod limits;
pub mod routes;
pub mod secret;
pub mod time;
pub mod values;
pub mod voice;
pub mod ws;

pub use auth::{
    JoinOutcome, JoinRequest, JoinStatus, JoinStatusRequest, MagicCodeRequest, MagicCodeRequested,
    MagicCodeVerify, RefreshRequest, SessionTokens, SignOutRequest, SignedIn, VerifyOutcome,
};
pub use drawing::{DrawingEpoch, DrawingSet, InkBatch, InkPoint, InkRelay, StrokeId};
pub use encoder::Quality;
pub use entities::{
    AccessRequest, AccessRequestOutcome, Attachment, AuthorRef, LobbyInfo, Member, Message,
    MessageChanges, MessagePage, OwnerTransfer, Preview, ScreenShare, User, UsernameChange,
    VoiceState,
};
pub use error::{ErrorBody, ErrorCode, ErrorDetail, ErrorEnvelope};
pub use ids::{
    AccessRequestId, BlobId, IdError, LobbyInstanceId, MessageId, Rid, ShareId, UserId,
    VoiceSessionId,
};
pub use secret::{AccessToken, JoinTicket, MagicChallenge, MagicCode, RefreshToken, WaitToken};
pub use time::UnixMs;
pub use values::{EVERYONE, Email, Presence, RequestLabel, Username, ValueError};
pub use voice::{
    RestartPolicy, RtcAnswer, RtcOffer, RtcRestart, RtcTrack, SPEAKING_HANGOVER, Sdp, ShareSource,
    ShareStart, ShareWatch, SourceSize, TrackPurpose, VoiceEndReason, VoiceEnded, VoiceFlags,
    VoiceSpeaking, WatchDropped,
};
pub use ws::{
    AccessRequestRemoved, Ack, ClientFrame, ClientMsg, DecodeError, HeldHistory, Hello, Identify,
    MemberRemoved, MessageDelete, MessageEdit, MessageRemoved, MessageSend, PresenceSet,
    RawClientFrame, Ready, ReadyHistory, ServerMsg, TypingStart,
};

/// Reported by `GET /api/v1/lobby` and `HELLO`, sent in `IDENTIFY`. REST lives under `/api/v1`.
pub const PROTOCOL_VERSION: u32 = 1;

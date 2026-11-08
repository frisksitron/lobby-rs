//! WebSocket frames. Server frames are `{"t","d"}`; client frames are `{"t","rid"?,"d"?}`.

use serde::{Deserialize, Serialize};

use crate::{
    AccessRequest, AccessRequestId, AccessRequestOutcome, AccessToken, BlobId, Email, ErrorBody,
    LobbyInfo, LobbyInstanceId, Member, Message, MessageId, MessagePage, Presence, Rid, UnixMs,
    UserId, VoiceSessionId,
    drawing::{DrawingSet, InkBatch, InkRelay},
    voice::{
        RtcAnswer, RtcOffer, RtcRestart, ShareSource, ShareStart, ShareWatch, VoiceEnded,
        VoiceFlags, VoiceSpeaking, WatchDropped,
    },
};

/// Defines `ALL_KINDS` and `kind()` from one list, so the exhaustive `match` in `kind()` stops
/// the build when a variant is added without its wire name (and therefore without a fixture).
macro_rules! wire_kinds {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            pub const ALL_KINDS: &'static [&'static str] = &[$($name),+];

            pub fn kind(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $name),+
                }
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub protocol_version: u32,
    pub server_version: String,
    pub heartbeat_ms: u32,
    pub identify_timeout_ms: u32,
}

/// The snapshot after the first valid IDENTIFY. It is built atomically with the connection's
/// registration, so no event falls between snapshot and live stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    pub conn_id: String,
    pub me: UserId,
    /// Where this Member's Magic Codes go. Only their own READY carries it.
    pub email: Email,
    pub lobby: LobbyInfo,
    pub members: Vec<Member>,
    pub access_requests: Vec<AccessRequest>,
    /// 0 if there are no Messages; every later `MESSAGE_CREATE` has a larger id.
    pub last_message_seq: u64,
    /// 0 if no Message was ever edited or deleted; every later `MESSAGE_UPDATE` and
    /// `MESSAGE_REMOVED` has a larger `rev`. A client that heard up to an older one reads the
    /// changes since from `GET /api/v1/messages/changes`.
    pub last_change_rev: u64,
    /// Read just before the snapshot, so a Message committed in between is above this page and
    /// at most `last_message_seq`: the client fetches it.
    pub history: ReadyHistory,
    /// The change revision read in one database read with `history`: the page shows every edit
    /// and deletion up to it. One committed after that read is above it and at most
    /// `last_change_rev`, and the client reads it from `GET /api/v1/messages/changes`.
    pub history_rev: u64,
}

/// The Messages READY carries, chosen by the Lobby from what IDENTIFY says the client holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "page", rename_all = "camelCase")]
pub enum ReadyHistory {
    /// The newest page, and the feed starts over: the client holds nothing, holds Messages of
    /// another instance of the Lobby, or missed more than the Lobby fills in.
    Latest(MessagePage),
    /// The Messages after the newest one the client holds, oldest first.
    After(MessagePage),
}

/// The newest Message a client holds, and of which instance of the Lobby.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldHistory {
    pub instance_id: LobbyInstanceId,
    pub newest: MessageId,
}

/// A command's success outcome. The optional fields belong to specific commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ack {
    pub rid: Rid,
    /// re-IDENTIFY: the new auth deadline of this connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_expires_at: Option<UnixMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_session_id: Option<VoiceSessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deafened: Option<bool>,
}

impl Ack {
    pub fn new(rid: Rid) -> Self {
        Self {
            rid,
            access_expires_at: None,
            voice_session_id: None,
            muted: None,
            deafened: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessRequestRemoved {
    pub id: AccessRequestId,
    pub outcome: AccessRequestOutcome,
    pub handled_by: UserId,
    pub handled_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberRemoved {
    pub user_id: UserId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRemoved {
    pub message_id: MessageId,
    /// The deletion's change revision, as [`crate::Message::rev`] is an edit's.
    pub rev: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypingStart {
    pub user_id: UserId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "d", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServerMsg {
    Hello(Hello),
    Ready(Box<Ready>),
    Ack(Ack),
    Error(ErrorBody),
    MemberUpsert {
        member: Member,
    },
    /// To everyone but the removed Member, whose connection closes with 4006 instead.
    MemberRemoved(MemberRemoved),
    /// To everyone, the sender included: the copy whose `nonce` is the sender's rid is the
    /// sender's terminal outcome.
    MessageCreate {
        message: Message,
    },
    /// To everyone: an edited Message, whole, with its `editedAt`.
    MessageUpdate {
        message: Message,
    },
    /// To everyone: a Message its author deleted, and its attachments with it.
    MessageRemoved(MessageRemoved),
    /// To everyone; kept apart from `MEMBER_UPSERT` because it is the hottest event.
    VoiceSpeaking(VoiceSpeaking),
    RtcOffer(RtcOffer),
    VoiceEnded(VoiceEnded),
    WatchDropped(WatchDropped),
    AccessRequestUpsert {
        request: AccessRequest,
    },
    /// To everyone: an Ask to Join was Vouched in (its `MEMBER_UPSERT` follows) or Declined.
    AccessRequestRemoved(AccessRequestRemoved),
    LobbyUpdate {
        lobby: LobbyInfo,
    },
    /// To everyone but the typist. Clients expire it after `TYPING_EXPIRY` and clear it on that
    /// author's `MESSAGE_CREATE` (there is no `TYPING_STOP`).
    TypingStart(TypingStart),
    /// To the Sharer and the Watchers of a share with drawing on, never to the author. Lossy:
    /// the server drops it before it kicks anyone.
    Ink(InkRelay),
}

wire_kinds!(ServerMsg {
    Hello => "HELLO",
    Ready => "READY",
    Ack => "ACK",
    Error => "ERROR",
    MemberUpsert => "MEMBER_UPSERT",
    MemberRemoved => "MEMBER_REMOVED",
    MessageCreate => "MESSAGE_CREATE",
    MessageUpdate => "MESSAGE_UPDATE",
    MessageRemoved => "MESSAGE_REMOVED",
    VoiceSpeaking => "VOICE_SPEAKING",
    RtcOffer => "RTC_OFFER",
    VoiceEnded => "VOICE_ENDED",
    WatchDropped => "WATCH_DROPPED",
    AccessRequestUpsert => "ACCESS_REQUEST_UPSERT",
    AccessRequestRemoved => "ACCESS_REQUEST_REMOVED",
    TypingStart => "TYPING_START",
    LobbyUpdate => "LOBBY_UPDATE",
    Ink => "INK",
});

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("frame is not a JSON object with a string \"t\": {0}")]
    Malformed(#[source] serde_json::Error),
    #[error("unknown frame type {0:?}")]
    UnknownType(String),
    #[error("invalid payload for {t}: {source}")]
    BadPayload {
        t: String,
        #[source]
        source: serde_json::Error,
    },
}

impl ServerMsg {
    /// One serialization per frame; these types cannot fail to serialize.
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("ServerMsg serializes")
    }

    /// An unknown `t` is an error: client and server versions are checked for equality at HELLO.
    pub fn decode(text: &str) -> Result<ServerMsg, DecodeError> {
        #[derive(Deserialize)]
        struct Head<'a> {
            #[serde(borrow)]
            t: std::borrow::Cow<'a, str>,
        }
        let head: Head<'_> = serde_json::from_str(text).map_err(DecodeError::Malformed)?;
        if !Self::ALL_KINDS.contains(&head.t.as_ref()) {
            return Err(DecodeError::UnknownType(head.t.into_owned()));
        }
        serde_json::from_str(text).map_err(|source| DecodeError::BadPayload {
            t: head.t.into_owned(),
            source,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identify {
    pub token: AccessToken,
    /// The presence others see once this connection registers: `online`, `idle` or `dnd`;
    /// never `offline`. Only the first IDENTIFY of a socket uses it, so a reconnect shows no
    /// flash of the wrong presence.
    pub presence: Presence,
    /// What READY's history follows on from; only the first IDENTIFY of a socket reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<HeldHistory>,
}

/// `MESSAGE_SEND`; the rid is required, 16–64 chars, and becomes `Message.nonce`. A rid stored
/// before is answered with that Message again, to the sender only, or with an `ACK` if it was
/// deleted since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSend {
    /// Markdown (see [`crate::content`]), at most 8000 chars and nested at most
    /// `MESSAGE_NESTING_MAX` deep.
    pub content: String,
    #[serde(default)]
    pub attachment_ids: Vec<BlobId>,
}

/// `MESSAGE_EDIT`: new content for one of the sender's own Messages; its attachments stay.
/// Answered with `ACK` after the `MESSAGE_UPDATE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEdit {
    pub message_id: MessageId,
    /// The same rules as [`MessageSend::content`].
    pub content: String,
}

/// `MESSAGE_DELETE`: one of the sender's own Messages. Answered with `ACK` after the
/// `MESSAGE_REMOVED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDelete {
    pub message_id: MessageId,
}

/// `PRESENCE_SET`: `idle` comes from the OS idle time, `dnd` is Do Not Disturb; `offline` is
/// refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceSet {
    pub presence: Presence,
}

/// Client commands. Encoded through [`ClientFrame`], decoded through [`RawClientFrame`]. A
/// command without a payload has no `d` (or `null`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "d", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClientMsg {
    Identify(Identify),
    PresenceSet(PresenceSet),
    MessageSend(MessageSend),
    MessageEdit(MessageEdit),
    MessageDelete(MessageDelete),
    VoiceJoin(VoiceFlags),
    VoiceLeave,
    VoiceStateSet(VoiceFlags),
    RtcAnswer(RtcAnswer),
    RtcRestart(RtcRestart),
    ScreenShareStart(ShareStart),
    ScreenShareStop,
    ScreenShareWatch(ShareWatch),
    ScreenShareUnwatch,
    /// Sent without a rid by the Sharer, first for the share's source and again when it changes.
    ScreenShareSource(ShareSource),
    DrawingSet(DrawingSet),
    /// Sent without a rid, like `TYPING`: never ACKed, and dropped silently when refused.
    Ink(InkBatch),
    /// No payload; sent without a rid, at most every `TYPING_SEND_INTERVAL` while composing.
    Typing,
}

wire_kinds!(ClientMsg {
    Identify => "IDENTIFY",
    PresenceSet => "PRESENCE_SET",
    MessageSend => "MESSAGE_SEND",
    MessageEdit => "MESSAGE_EDIT",
    MessageDelete => "MESSAGE_DELETE",
    VoiceJoin => "VOICE_JOIN",
    VoiceLeave => "VOICE_LEAVE",
    VoiceStateSet => "VOICE_STATE_SET",
    RtcAnswer => "RTC_ANSWER",
    RtcRestart => "RTC_RESTART",
    ScreenShareStart => "SCREEN_SHARE_START",
    ScreenShareStop => "SCREEN_SHARE_STOP",
    ScreenShareWatch => "SCREEN_SHARE_WATCH",
    ScreenShareUnwatch => "SCREEN_SHARE_UNWATCH",
    ScreenShareSource => "SCREEN_SHARE_SOURCE",
    DrawingSet => "DRAWING_SET",
    Ink => "INK",
    Typing => "TYPING",
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFrame {
    pub rid: Option<Rid>,
    pub msg: ClientMsg,
}

impl ClientFrame {
    pub fn new(msg: ClientMsg) -> Self {
        Self { rid: None, msg }
    }

    pub fn with_rid(rid: Rid, msg: ClientMsg) -> Self {
        Self {
            rid: Some(rid),
            msg,
        }
    }

    pub fn encode(&self) -> String {
        let mut value = serde_json::to_value(&self.msg).expect("ClientMsg serializes");
        if let Some(rid) = &self.rid {
            let obj = value
                .as_object_mut()
                .expect("ClientMsg serializes as an object");
            obj.insert("rid".into(), rid.as_str().into());
        }
        value.to_string()
    }
}

/// The server's decode in two stages: the head (`t` and `rid`) first, so an unknown type or a
/// bad `d` becomes a precise `ERROR` that still carries the rid, then the command itself.
#[derive(Debug)]
pub struct RawClientFrame<'a> {
    pub t: String,
    pub rid: Option<Rid>,
    text: &'a str,
}

impl<'a> RawClientFrame<'a> {
    pub fn parse(text: &'a str) -> Result<Self, DecodeError> {
        #[derive(Deserialize)]
        struct Head {
            t: String,
            #[serde(default)]
            rid: Option<Rid>,
        }
        let Head { t, rid } = serde_json::from_str(text).map_err(DecodeError::Malformed)?;
        Ok(Self { t, rid, text })
    }

    pub fn decode(self) -> Result<ClientFrame, DecodeError> {
        if !ClientMsg::ALL_KINDS.contains(&self.t.as_str()) {
            return Err(DecodeError::UnknownType(self.t));
        }
        match serde_json::from_str(self.text) {
            Ok(msg) => Ok(ClientFrame { rid: self.rid, msg }),
            Err(source) => Err(DecodeError::BadPayload { t: self.t, source }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_or_missing_payload_is_bad_payload() {
        for text in [
            r#"{"t":"IDENTIFY","d":{"token":5,"presence":"online"}}"#,
            r#"{"t":"IDENTIFY"}"#,
        ] {
            let err = RawClientFrame::parse(text).unwrap().decode().unwrap_err();
            assert!(
                matches!(&err, DecodeError::BadPayload { t, .. } if t == "IDENTIFY"),
                "{text}: {err:?}"
            );
        }
    }
}

//! Voice signalling payloads. The server is the only SDP offerer: it sends
//! `RTC_OFFER` with the full mid table, the client answers with `RTC_ANSWER`.

use std::{fmt, num::NonZeroU32, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{UserId, VoiceSessionId, encoder::Quality};

/// An SDP body. It carries ICE credentials and the DTLS fingerprint, so `Debug` prints only its
/// length: SDP reaches logs only deliberately, at `trace`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sdp(String);

impl Sdp {
    pub fn new(sdp: impl Into<String>) -> Self {
        Self(sdp.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for Sdp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sdp({} bytes)", self.0.len())
    }
}

/// `VOICE_JOIN` and `VOICE_STATE_SET`. The server normalizes: `deafened ⇒ muted`, and unmuting
/// while Deafened also undeafens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceFlags {
    pub muted: bool,
    pub deafened: bool,
}

/// How long a Member still counts as speaking after their last voice packet whose RFC 6464 V
/// flag was set, when no packet has said otherwise since: muted, DTX and loss send nothing. The
/// sender's VAD holds the flag through pauses itself; this only covers the gap.
pub const SPEAKING_HANGOVER: Duration = Duration::from_millis(200);

/// The server's `VOICE_SPEAKING`, to everyone, only while the Member is Active and not Muted.
/// The SFU decides it from the V flag of the Member's voice packets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceSpeaking {
    pub user_id: UserId,
    pub speaking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackPurpose {
    /// A Member's microphone: the Member's own sendrecv m-line, or a downlink carrying another
    /// Member's voice.
    VoiceAudio,
    ScreenVideo,
    ScreenAudio,
    WatchVideo,
    WatchAudio,
}

/// `SCREEN_SHARE_START`. Starting while already sharing only updates `audio`: the quality stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStart {
    pub audio: bool,
    pub quality: Quality,
}

/// The size of the shared content in pixels, from the capture's frames. The encoded size and ink
/// points refer to this source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSize {
    pub width: NonZeroU32,
    pub height: NonZeroU32,
}

/// `SCREEN_SHARE_SOURCE`: the Sharer reports the source size, and again when it changes. It
/// applies to the sender's current share, so it carries no share id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSource {
    pub size: SourceSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareWatch {
    pub sharer_id: UserId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtcTrack {
    pub mid: String,
    pub user_id: UserId,
    pub purpose: TrackPurpose,
}

/// The initial offer, every renegotiation and ICE restarts. `tracks` is the complete
/// mid table of this offer: pooled m-lines are re-pointed at other Members over time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtcOffer {
    pub voice_session_id: VoiceSessionId,
    pub rev: u32,
    pub sdp: Sdp,
    pub tracks: Vec<RtcTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtcAnswer {
    pub voice_session_id: VoiceSessionId,
    pub rev: u32,
    pub sdp: Sdp,
}

/// `RTC_RESTART`: the client's media path broke (or its network changed) while the WebSocket
/// lives. The SFU answers with an `RTC_OFFER` that restarts ICE; refused with
/// `VOICE_NEGOTIATION_INVALID_STATE` unless the session is the sender's current, active one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtcRestart {
    pub voice_session_id: VoiceSessionId,
}

/// How long a client tries to get a broken media path back before it leaves Voice: `attempts`
/// ICE restarts of `timeout` each. Not on the wire; the server waits for a restarting client
/// from the same numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    pub attempts: NonZeroU32,
    pub timeout: Duration,
}

impl RestartPolicy {
    pub const DEFAULT: RestartPolicy = RestartPolicy {
        attempts: NonZeroU32::new(3).unwrap(),
        timeout: Duration::from_secs(10),
    };

    /// Every attempt, back to back.
    pub const fn window(self) -> Duration {
        self.timeout.saturating_mul(self.attempts.get())
    }
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceEndReason {
    Timeout,
    Failed,
    ConnectionLost,
    Replaced,
    Kicked,
    Shutdown,
}

/// `VOICE_ENDED`: the client tears its media down. Never the answer to the client's own
/// `VOICE_LEAVE`, whose outcome is its `ACK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceEnded {
    pub voice_session_id: VoiceSessionId,
    pub reason: VoiceEndReason,
}

/// `WATCH_DROPPED`: to a Viewer, once a second while the SFU drops frames of the watched share
/// because the Viewer's downlink is behind, or while a hole on the Sharer's uplink waits for the
/// next keyframe. Those frames leave no hole the Viewer could see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchDropped {
    pub frames: u32,
    /// The SFU's estimate of the Viewer's downlink, in bits per second.
    pub downlink_bps: Option<u64>,
}

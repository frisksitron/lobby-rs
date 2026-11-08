//! Issues: things the person should know that no control on screen already says, such as a
//! cooldown with its countdown or a refusal with no field to pin it on. An Issue is an
//! [`IssueKind`] with its words, and the kind decides everything else: its severity, its source,
//! its action (Rejoin, Send again) and whether it belongs to a Lobby. Pure: keyed by kind (a repeat
//! updates in place), with an optional expiry on the driver's monotonic clock. The desktop sweeps
//! them every second.

use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

use lobby_protocol::UnixMs;

use crate::LobbyOrigin;

/// The longest a server's `retryAfterMs` is trusted for.
pub const MAX_ISSUE_EXPIRY: Duration = Duration::from_secs(600);
/// A rate-limit Issue whose server sent no `retryAfterMs`.
pub const FALLBACK_ISSUE_EXPIRY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Connection,
    Message,
    Voice,
    Presence,
    Admission,
    /// This device rather than a Lobby: the saved Lobbies in `state.json`. Such an Issue
    /// outlives a Lobby switch.
    Device,
}

/// What an Issue's button does. The window runs it in the Lobby the Issue came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IssueAction {
    Rejoin,
    SendAgain,
    /// Show the file that was moved aside.
    ShowInFolder,
}

/// Everything that is told as an Issue, each kind raised in one place: the Session for the
/// Lobby's refusals, the connection, Voice and Screen Share; the desktop for what only it knows (a
/// picture or a file that failed, a Message that could not be sent, the saved Lobbies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IssueKind {
    /// The Lobby's `ERROR` frame that no command of ours was waiting for.
    ServerError,
    /// A presence change over the Lobby's budget.
    PresenceRateLimited,
    /// A Voice session that ended: the connection, the Lobby, or a media fault with no cause of
    /// its own.
    VoiceEnded,
    /// The OS has no route to any of the SFU's addresses (a VPN, a firewall, an IPv6-only
    /// advertisement on an IPv4-only network).
    NoRoute,
    NoDevice,
    DeviceNotFound,
    DeviceInUse,
    MediaPermissionDenied,
    /// One kind each, so a microphone and speakers that both fell back raise two Issues.
    InputFellBack,
    OutputFellBack,
    /// The unmuted microphone sends only digital zeros: no control on screen says so.
    MicSilent,
    UnmuteCooldown,
    JoinCooldown,
    SignalingRateLimited,
    /// A Join, mute or deafen that was refused, here or by the Lobby, and left the call as it was.
    VoiceRefused,
    ShareFailed,
    /// A Message that did not go out; its draft is back in the composer.
    SendFailed,
    /// A Message the Lobby refused for what it is (too long, invalid): the same text would be
    /// refused again, so there is no "Send again".
    SendRefused,
    ChangeFailed,
    HistoryFailed,
    OlderFailed,
    VouchFailed,
    DeclineFailed,
    RemoveFailed,
    TransferFailed,
    RenameFailed,
    PictureFailed,
    LeaveFailed,
    PayloadTooLarge,
    AttachRefused,
    UploadFailed,
    SaveFailed,
    OpenFailed,
    /// The saved Lobbies (`state.json`) were moved aside, or could not be read or written.
    StateCorrupt,
    StateReadFailed,
    StateWriteFailed,
}

impl IssueKind {
    /// A stable name for logs, the automation model and widget ids.
    pub fn key(self) -> &'static str {
        match self {
            Self::ServerError => "ws.server_error",
            Self::PresenceRateLimited => "ws.rate_limited",
            Self::VoiceEnded => "voice.ended",
            Self::NoRoute => "webrtc.no_route",
            Self::NoDevice => "webrtc.no_device",
            Self::DeviceNotFound => "webrtc.device_not_found",
            Self::DeviceInUse => "webrtc.device_in_use",
            Self::MediaPermissionDenied => "webrtc.media_permission_denied",
            Self::InputFellBack => "webrtc.input_fell_back",
            Self::OutputFellBack => "webrtc.output_fell_back",
            Self::MicSilent => "voice.mic_silent",
            Self::UnmuteCooldown => "ws.voice_cooldown",
            Self::JoinCooldown => "ws.voice_join_cooldown",
            Self::SignalingRateLimited => "ws.signaling_rate_limited",
            Self::VoiceRefused => "voice.refused",
            Self::ShareFailed => "share.failed",
            Self::SendFailed => "message.send_failed",
            Self::SendRefused => "message.send_refused",
            Self::ChangeFailed => "message.change_failed",
            Self::HistoryFailed => "message.history_failed",
            Self::OlderFailed => "message.older_failed",
            Self::VouchFailed => "access_request.vouch_failed",
            Self::DeclineFailed => "access_request.decline_failed",
            Self::RemoveFailed => "member.remove_failed",
            Self::TransferFailed => "member.transfer_failed",
            Self::RenameFailed => "account.rename_failed",
            Self::PictureFailed => "account.picture_failed",
            Self::LeaveFailed => "account.leave_failed",
            Self::PayloadTooLarge => "api.payload_too_large",
            Self::AttachRefused => "attachment.refused",
            Self::UploadFailed => "attachment.upload_failed",
            Self::SaveFailed => "attachment.save_failed",
            Self::OpenFailed => "attachment.open_failed",
            Self::StateCorrupt => "settings.corrupt",
            Self::StateReadFailed => "settings.read_failed",
            Self::StateWriteFailed => "settings.write_failed",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            Self::SendFailed | Self::SendRefused | Self::SaveFailed | Self::OpenFailed => {
                Severity::Error
            }
            _ => Severity::Warning,
        }
    }

    pub fn source(self) -> Source {
        match self {
            Self::ServerError => Source::Connection,
            Self::PresenceRateLimited => Source::Presence,
            Self::VoiceEnded
            | Self::NoRoute
            | Self::NoDevice
            | Self::DeviceNotFound
            | Self::DeviceInUse
            | Self::MediaPermissionDenied
            | Self::InputFellBack
            | Self::OutputFellBack
            | Self::MicSilent
            | Self::UnmuteCooldown
            | Self::JoinCooldown
            | Self::SignalingRateLimited
            | Self::VoiceRefused
            | Self::ShareFailed => Source::Voice,
            Self::SendFailed
            | Self::SendRefused
            | Self::ChangeFailed
            | Self::HistoryFailed
            | Self::OlderFailed
            | Self::PayloadTooLarge
            | Self::AttachRefused
            | Self::UploadFailed
            | Self::SaveFailed
            | Self::OpenFailed => Source::Message,
            Self::VouchFailed
            | Self::DeclineFailed
            | Self::RemoveFailed
            | Self::TransferFailed
            | Self::RenameFailed
            | Self::PictureFailed
            | Self::LeaveFailed => Source::Admission,
            Self::StateCorrupt | Self::StateReadFailed | Self::StateWriteFailed => Source::Device,
        }
    }

    /// Voice is never rejoined by itself, so each way it ended offers it.
    pub fn action(self) -> Option<IssueAction> {
        match self {
            Self::VoiceEnded
            | Self::NoRoute
            | Self::NoDevice
            | Self::DeviceNotFound
            | Self::DeviceInUse
            | Self::MediaPermissionDenied => Some(IssueAction::Rejoin),
            Self::SendFailed => Some(IssueAction::SendAgain),
            Self::StateCorrupt => Some(IssueAction::ShowInFolder),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub kind: IssueKind,
    /// The Lobby it was about: set where the window takes the Issue in, and never for an Issue
    /// of this device.
    pub lobby: Option<LobbyOrigin>,
    pub message: Cow<'static, str>,
    pub expires_at: Option<Instant>,
    pub raised_at: UnixMs,
}

impl Issue {
    pub fn new(kind: IssueKind, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind,
            lobby: None,
            message: message.into(),
            expires_at: None,
            raised_at: UnixMs::now(),
        }
    }

    /// Says which Lobby this is about, unless it is about this device.
    pub fn in_lobby(mut self, lobby: &LobbyOrigin) -> Self {
        if self.kind.source() != Source::Device {
            self.lobby = Some(lobby.clone());
        }
        self
    }

    /// Expires after the server's relative `retryAfterMs`, converted once on receipt into a
    /// deadline on the driver's clock (immune to clock skew), clamped to 10 min.
    pub fn expiring(mut self, now: Instant, retry_after_ms: Option<u64>) -> Self {
        let after = retry_after_ms
            .map(Duration::from_millis)
            .unwrap_or(FALLBACK_ISSUE_EXPIRY)
            .min(MAX_ISSUE_EXPIRY);
        self.expires_at = Some(now + after);
        self
    }

    pub fn severity(&self) -> Severity {
        self.kind.severity()
    }

    pub fn source(&self) -> Source {
        self.kind.source()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issues {
    list: Vec<Issue>,
}

impl Issues {
    pub fn raise(&mut self, issue: Issue) -> bool {
        match self.list.iter_mut().find(|i| i.kind == issue.kind) {
            Some(existing) => {
                *existing = issue;
                false
            }
            None => {
                self.list.push(issue);
                true
            }
        }
    }

    pub fn dismiss(&mut self, kind: IssueKind) -> bool {
        let before = self.list.len();
        self.list.retain(|i| i.kind != kind);
        self.list.len() != before
    }

    pub fn sweep(&mut self, now: Instant) -> bool {
        let before = self.list.len();
        self.list.retain(|i| i.expires_at.is_none_or(|at| at > now));
        self.list.len() != before
    }

    pub fn retain(&mut self, keep: impl FnMut(&Issue) -> bool) -> bool {
        let before = self.list.len();
        self.list.retain(keep);
        self.list.len() != before
    }

    pub fn next_expiry(&self) -> Option<Instant> {
        self.list.iter().filter_map(|i| i.expires_at).min()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Issue> {
        self.list.iter()
    }

    pub fn get(&self, kind: IssueKind) -> Option<&Issue> {
        self.list.iter().find(|i| i.kind == kind)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

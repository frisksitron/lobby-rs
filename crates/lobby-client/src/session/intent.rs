//! What a Member does to a Lobby: the one way into its Session.

use std::{collections::BTreeMap, path::PathBuf};

use lobby_protocol::{AccessRequestId, MessageId, Quality, ShareId, UserId, Username};

use crate::{
    drafts::{AttachError, DraftId, DraftSource},
    ink::PenPoint,
    media::{DeviceChoice, DeviceKind, MediaConfig, ScreenSource},
    state::OwnerAction,
    volume::Volume,
};

/// A refusal is an Issue the Session raises; what changed is in its state.
#[derive(Debug)]
pub enum Intent {
    /// The composer's text, with the Ready drafts.
    Send(String),
    /// New text for one of this Member's Messages; its attachments stay. The feed changes when
    /// the Lobby says so.
    EditMessage {
        id: MessageId,
        content: String,
    },
    DeleteMessage(MessageId),
    Typing,
    LoadOlder,
    /// A picked, dropped or pasted file.
    Attach(DraftSource),
    /// A pick that gave nothing to attach (a folder, an unreadable file, a picture that did not
    /// decode): the Session says why, as it does for a file it refuses itself.
    AttachFailed(AttachError),
    RetryAttachment(DraftId),
    RemoveAttachment(DraftId),
    /// `volumes` are how loud the person saved each other Member.
    JoinVoice {
        media: Box<MediaConfig>,
        volumes: BTreeMap<UserId, Volume>,
    },
    LeaveVoice,
    /// Mute or unmute (Deafen or Undeafen), from what the Session last asked: two clicks before
    /// the window hears back turn it on and off again.
    ToggleMute,
    ToggleDeafen,
    MemberVolume {
        user: UserId,
        volume: Volume,
    },
    /// The running call's microphone or speakers move to `choice` now; the next call takes the
    /// choice it joins with.
    ChooseDevice {
        kind: DeviceKind,
        choice: DeviceChoice,
    },
    StartShare {
        source: ScreenSource,
        quality: Quality,
    },
    StopShare,
    /// Its feed comes with the next update, as [`SessionUpdate::watch_feed`](crate::SessionUpdate).
    Watch(UserId),
    Unwatch,
    /// The Sharer's "Let viewers draw".
    SetDrawing(bool),
    PenDown {
        share: ShareId,
        at: PenPoint,
    },
    PenTo(PenPoint),
    /// At `Some` point where the button came up, or where the pen last was.
    PenUp(Option<PenPoint>),
    SetDnd(bool),
    /// From the OS: the person stepped away or came back.
    SetIdle(bool),
    Vouch(AccessRequestId),
    Decline(AccessRequestId),
    Owner {
        user: UserId,
        action: OwnerAction,
    },
    Rename(Username),
    SetAvatar(PathBuf),
    RemoveAvatar,
    SetIcon(PathBuf),
    RemoveIcon,
    LeaveLobby,
    /// The owner's Leave: the Lobby makes this Member the owner, and once it confirms, the owner
    /// leaves. A refusal at either step is an Issue, and the owner stays.
    HandOverAndLeave(UserId),
    /// Reconnect now instead of at the next attempt.
    RetryNow,
    /// The network may have changed: check the connection now.
    ProbeNow,
}

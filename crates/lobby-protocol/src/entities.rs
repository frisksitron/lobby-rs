//! Entities. Optional fields are absent, never `null`.

use serde::{Deserialize, Serialize};

use crate::{
    AccessRequestId, BlobId, DrawingEpoch, LobbyInstanceId, MessageId, Presence, Quality, ShareId,
    SourceSize, UnixMs, UserId, Username,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LobbyInfo {
    pub instance_id: LobbyInstanceId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<BlobId>,
    pub upload_max_bytes: u64,
    /// Magic Codes mailed per email and client in `limits::MAGIC_CODE_SEND_WINDOW`.
    pub magic_code_sends: u32,
    /// `false` = the Lobby is locked against Ask to Join.
    pub join_requests_enabled: bool,
    /// `false` until the Lobby's first Member joined with its bootstrap email.
    pub set_up: bool,
    /// The one Member who may remove Members and hand ownership on; absent before set-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<UserId>,
    pub protocol_version: u32,
    pub server_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerTransfer {
    pub user_id: UserId,
}

/// `PATCH /api/v1/users/me`: the Member's new name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsernameChange {
    pub username: Username,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: UserId,
    pub username: Username,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<BlobId>,
    pub created_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: UserId,
    pub username: Username,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<BlobId>,
    pub created_at: UnixMs,
    pub presence: Presence,
    /// Absent: not In Voice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceState>,
}

/// A Member's Voice session as everyone sees it. The server keeps `deafened ⇒ muted` and
/// `speaking ⇒ !joining && !muted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceState {
    /// True until the SFU reports the Member's media connected.
    pub joining: bool,
    pub muted: bool,
    pub deafened: bool,
    pub speaking: bool,
    /// Present only while the Member's Screen Share is Streaming (its video reaches the SFU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_share: Option<ScreenShare>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenShare {
    pub share_id: ShareId,
    pub audio: bool,
    /// Absent until the Sharer reports the source size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceSize>,
    /// Present only while "Let viewers draw" is on; absent means off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawing: Option<DrawingEpoch>,
    /// The most it sends, as its Sharer picked.
    pub quality: Quality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: MessageId,
    pub author: AuthorRef,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// The sender's `MESSAGE_SEND` rid.
    pub nonce: String,
    pub created_at: UnixMs,
    /// When its author last edited it; absent if never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<UnixMs>,
    /// The Lobby's change revision of its last edit, absent if never: of two copies of a
    /// Message, the one with the larger `rev` is newer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
}

/// `GET /api/v1/messages`: ascending by id; `hasMore` = more exist beyond the page in the
/// requested direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub messages: Vec<Message>,
    pub has_more: bool,
}

/// `GET /api/v1/messages/changes?after=`: the Messages edited or deleted since change revision
/// `after`, each once as it is now, the oldest change first: at most
/// [`crate::limits::CHANGES_MAX`] edited ones and as many deleted ones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageChanges {
    pub updated: Vec<Message>,
    pub removed: Vec<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: BlobId,
    /// Sanitized by the server; clients still never trust it.
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    /// Absent (never `null`) unless the server made a JPEG preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Preview>,
}

/// The preview JPEG's exact pixel size, so a card can reserve it before any byte arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
}

/// Resolved from `users` when the Message is read, so a rename shows everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorRef {
    pub id: UserId,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<BlobId>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deactivated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessRequest {
    pub id: AccessRequestId,
    pub username: Username,
    pub requester_label: String,
    pub created_at: UnixMs,
}

impl AccessRequest {
    /// Where a request stands in the Lobby's list, newest first: READY lists in this order, and
    /// whatever shows requests beside rows that left the list sorts by it too.
    pub fn list_order(
        created_at: UnixMs,
        id: &AccessRequestId,
    ) -> (
        std::cmp::Reverse<UnixMs>,
        std::cmp::Reverse<&AccessRequestId>,
    ) {
        (std::cmp::Reverse(created_at), std::cmp::Reverse(id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRequestOutcome {
    VouchedIn,
    Declined,
}

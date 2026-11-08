//! The REST routes: each path as the server routes it (`{id}` marks a parameter) and, for those
//! with parameters, the concrete path a client requests.

use serde::{Deserialize, Serialize};

use crate::{AccessRequestId, BlobId, MessageId, UserId};

macro_rules! api {
    ($path:literal) => {
        concat!("/api/v1", $path)
    };
}

pub const HEALTH: &str = "/health";
pub const LOBBY: &str = api!("/lobby");
pub const WS: &str = api!("/ws");
pub const MAGIC_CODE: &str = api!("/auth/magic-code");
pub const MAGIC_CODE_VERIFY: &str = api!("/auth/magic-code/verify");
pub const JOIN: &str = api!("/auth/join");
pub const JOIN_STATUS: &str = api!("/auth/join/status");
pub const REFRESH: &str = api!("/auth/refresh");
pub const SIGN_OUT: &str = api!("/auth/sign-out");
pub const USERS_ME: &str = api!("/users/me");
pub const USERS_ME_AVATAR: &str = api!("/users/me/avatar");
pub const LOBBY_ICON: &str = api!("/lobby/icon");
pub const MESSAGES: &str = api!("/messages");
pub const MESSAGE_CHANGES: &str = api!("/messages/changes");
pub const VOUCH: &str = api!("/access-requests/{id}/vouch");
pub const DECLINE: &str = api!("/access-requests/{id}/decline");
pub const REMOVE_MEMBER: &str = api!("/members/{id}/remove");
pub const LOBBY_OWNER: &str = api!("/lobby/owner");
pub const UPLOADS: &str = api!("/uploads");
pub const MEDIA: &str = "/media/{id}";
pub const MEDIA_PREVIEW: &str = "/media/{id}/preview";

fn with_id(pattern: &str, id: &str) -> String {
    pattern.replace("{id}", id)
}

pub fn vouch(id: &AccessRequestId) -> String {
    with_id(VOUCH, id.as_str())
}

pub fn decline(id: &AccessRequestId) -> String {
    with_id(DECLINE, id.as_str())
}

pub fn remove_member(id: &UserId) -> String {
    with_id(REMOVE_MEMBER, id.as_str())
}

pub fn media(id: &BlobId) -> String {
    with_id(MEDIA, id.as_str())
}

pub fn media_preview(id: &BlobId) -> String {
    with_id(MEDIA_PREVIEW, id.as_str())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryQuery {
    /// 1 to `HISTORY_MAX`; absent means `HISTORY_DEFAULT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<MessageId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesQuery {
    /// The newest change revision the client heard.
    pub after: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

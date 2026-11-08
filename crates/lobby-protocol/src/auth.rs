//! Auth DTOs. Secrets travel only in the newtypes of [`crate::secret`].

use serde::{Deserialize, Serialize};

use crate::{
    AccessToken, Email, JoinTicket, MagicChallenge, MagicCode, RefreshToken, RequestLabel, UnixMs,
    User, Username, WaitToken,
};

/// A session's credentials. Also the client's keychain format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTokens {
    pub access_token: AccessToken,
    pub access_expires_at: UnixMs,
    pub refresh_token: RefreshToken,
    /// Sliding with every rotation.
    pub refresh_expires_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedIn {
    pub user: User,
    pub tokens: SessionTokens,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRequest {
    pub refresh_token: RefreshToken,
}

/// Answered with [`MagicCodeRequested`] whatever the email's standing, so it never tells who is a
/// Member. `RATE_LIMITED` once this device had its codes to the email for now: only its own
/// requests count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicCodeRequest {
    pub email: Email,
}

/// A new challenge for every request. The code it was mailed with is verified with it, and no
/// other request can spend or replace that code. The `label` is in the code's email, so a Member
/// whose address was asked for codes by others can tell which mail answers this request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicCodeRequested {
    pub challenge: MagicChallenge,
    pub label: RequestLabel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicCodeVerify {
    pub challenge: MagicChallenge,
    pub code: MagicCode,
}

/// What a right code leads to: the server decides from where the email stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "next",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum VerifyOutcome {
    /// A Member: signed in.
    Session { session: SignedIn },
    /// This email's Ask to Join waits for a Vouch; `wait_token` checks on it.
    PendingApproval { wait_token: WaitToken },
    /// The bootstrap email of a Lobby without Members: a name spends the ticket and sets it up.
    CreateLobby { ticket: JoinTicket },
    /// No Member uses this email: a name spends the ticket and files an Ask to Join.
    AskToJoin { ticket: JoinTicket },
}

/// The server decides the path again, so the same request serves `CreateLobby` and `AskToJoin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    pub ticket: JoinTicket,
    pub username: Username,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "next",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum JoinOutcome {
    /// Bootstrap: the first Member, signed in without a Vouch.
    Session {
        session: SignedIn,
    },
    PendingApproval {
        wait_token: WaitToken,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinStatusRequest {
    pub wait_token: WaitToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "next", rename_all = "snake_case")]
pub enum JoinStatus {
    PendingApproval,
    Session { session: SignedIn },
    Declined,
}

/// `POST /api/v1/auth/sign-out`. `everywhere` ends every session of the User, not only this one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignOutRequest {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub everywhere: bool,
}

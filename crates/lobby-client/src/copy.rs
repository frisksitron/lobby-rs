//! User-facing copy: one table, so each wording appears once.

use std::time::Duration;

use lobby_protocol::{ErrorCode, ErrorDetail, Quality, RequestLabel, VoiceEndReason, limits};

use crate::{
    ApiError, ApiKind, DeviceKind, IssueKind, MediaError, PictureError, Stage, StrainCause,
    StrainRole, StrainWarning,
};

pub const FAILED_TO_CONNECT: &str = "Couldn't connect to the Lobby";
pub const NOT_A_LOBBY_ADDRESS: &str = "A Lobby address looks like lobby.example.com.";
pub const PROTOCOL_MISMATCH: &str = "This app and the Lobby run different versions. \
    Update the app, or ask the owner to update the Lobby.";
pub const LOBBY_MUST_USE_HTTPS: &str =
    "This Lobby needs https://. Only a Lobby on this computer can use http://.";
/// What a refused sign in says when the Lobby gave no words of its own.
pub const SIGN_IN_FAILED: &str = "That didn't sign you in. Try again.";
/// Tokens are never kept in memory only, so a missing keychain is an error.
pub const KEYCHAIN_UNAVAILABLE: &str = "Secure token storage unavailable";
pub const SESSION_EXPIRED: &str = "Session expired. Sign in again.";
/// Close 4005: a Sign Out here, or everywhere from another device; which one is not on the wire.
pub const SIGNED_OUT_HERE: &str = "You were signed out here. Sign in again.";
pub const REMOVED_FROM_LOBBY: &str = "You were removed from this Lobby.";
pub const SIGNED_IN_ELSEWHERE: &str = "Signed in on another device";
pub const NETWORK_ERROR: &str = "Network error. Check your connection.";
pub const UNEXPECTED_RESPONSE: &str = "Unexpected response from the Lobby";
/// The Lobby reconnects by itself, so there is nothing to ask the person to do.
pub const NOT_CONNECTED: &str = "Not connected right now. Reconnecting by itself.";
pub const COMPOSER_PLACEHOLDER: &str = "Message";
pub const MESSAGE_NOT_DELIVERED: &str =
    "The Lobby didn't confirm it, so it may not have arrived. It's back in the composer.";
/// The reason leads, so a compact toast still shows it after its title.
pub const MESSAGE_TOO_LONG: &str = "Too long. Shorten it and try again.";
pub const MESSAGE_TOO_NESTED: &str =
    "Nested too deeply. Flatten its lists or quotes and try again.";
pub const SENDING_TOO_FAST: &str = "Sending too fast. Try again in a moment.";
pub const HISTORY_FAILED: &str = "Couldn't load the earlier ones.";
pub const OLDER_FAILED: &str = "The older ones didn't load. Try Load older Messages again.";
pub const LOAD_OLDER: &str = "Load older Messages";
pub const BEGINNING_OF_HISTORY: &str = "Beginning of Message history";
pub const NO_MESSAGES_YET: &str = "No Messages yet";
pub const TODAY: &str = "Today";
pub const YESTERDAY: &str = "Yesterday";

pub fn ago(secs: i64) -> String {
    match secs {
        ..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        86_400..172_800 => "yesterday".to_owned(),
        _ => format!("{} days ago", secs / 86_400),
    }
}
pub const SENDING: &str = "Sending…";
pub const WAITING_TO_SEND: &str = "Waiting to send…";
pub const NEW_DIVIDER: &str = "New";

pub const PAYLOAD_TOO_LARGE: &str = "File exceeds the server upload size limit.";
pub const TOO_MANY_ATTACHMENTS: &str = "Up to 10 files go with one Message.";
pub const NOT_A_FILE: &str = "Only files can be attached.";
pub const FILE_UNREADABLE: &str = "The file couldn't be read.";
pub const ATTACHMENT_GONE: &str = "No longer on the Lobby. Retry uploads it again.";
pub const MEDIA_NOT_FOUND: &str = "This file is no longer on the Lobby.";
pub const MEDIA_RATE_LIMITED: &str = "The Lobby is busy. Try again in a moment.";
pub const MEDIA_TOO_LARGE: &str = "This file is larger than the Lobby said, so it was not saved.";
pub const IMAGE_UNREADABLE: &str = "This image couldn't be shown.";
pub const OPEN_NOT_ALLOWED: &str = "This type can only be saved.";

pub fn say_hello(lobby: &str) -> String {
    format!("Say hello to everyone in {lobby}.")
}

pub fn typing_line(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [one] => Some(format!("{one} is typing…")),
        [a, b] => Some(format!("{a} and {b} are typing…")),
        _ => Some("Several Members are typing…".to_owned()),
    }
}

pub const JUMP_TO_LATEST: &str = "Jump to latest";

pub const COPY: &str = "Copy";
pub const COPY_MESSAGE: &str = "Copy Message";
pub const EDIT_MESSAGE: &str = "Edit";
pub const DELETE_MESSAGE: &str = "Delete";
pub const EDITED: &str = "edited";
pub const EDITING_MESSAGE: &str = "Editing your Message. Enter saves, Esc cancels.";
pub const CHANGE_EMPTY: &str = "A Message needs words. Delete it instead.";
pub const CHANGE_NOT_CONFIRMED: &str = "The Lobby didn't confirm it. Try again.";
pub const CUT: &str = "Cut";
pub const PASTE: &str = "Paste";
pub const SELECT_ALL: &str = "Select all";
pub const OPEN: &str = "Open";
pub const SAVE_FILE: &str = "Save…";

pub fn new_messages(count: usize) -> String {
    match count {
        0 | 1 => "New Message".to_owned(),
        n if n > 99 => "99+ New Messages".to_owned(),
        n => format!("{n} New Messages"),
    }
}
/// Voice ends with the realtime connection and is never rejoined by itself.
pub const VOICE_DISCONNECTED: &str = "The connection dropped. Rejoin once you're back online.";
pub const VOICE_TIMED_OUT: &str = "The connection timed out. Try joining again.";
pub const VOICE_CONNECTION_FAILED: &str = "The connection failed. Try joining again.";
/// The server refuses mute and unmute while Voice is still joining.
pub const VOICE_NOT_ACTIVE: &str = "Not connected yet. Try again in a moment.";
pub const VOICE_NOT_CONFIRMED: &str = "Mute wasn't confirmed. Try again.";

pub const SHARE_NOT_AVAILABLE: &str = "No longer available. It may have ended.";
pub const SHARE_NOT_CONFIRMED: &str = "The Lobby didn't respond. Try again.";
pub const WATCH_FAILED: &str = "The Screen Share couldn't be shown. Try watching again.";
pub const SHARE_NEEDS_VOICE: &str = "Join Voice to share or watch a screen.";
/// The same as a tooltip, which has no full stop.
pub const SHARE_NEEDS_VOICE_TIP: &str = "Join Voice to share or watch a screen";
pub const WATCH_NEEDS_VOICE: &str = "Join Voice to watch";
/// After the title of such a window in the picker, which cannot pick it.
pub const CANT_SHARE: &str = "can't be shared";

pub fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::UpTo1080 => "1080p",
        Quality::UpTo1440 => "1440p",
        Quality::UpTo2160 => "4K",
    }
}

/// What a quality costs, after its name in the picker's select.
pub fn quality_need(quality: Quality) -> String {
    format!("· needs {} up", mbps(u64::from(quality.bps())))
}

pub fn mbps(bits_per_second: u64) -> String {
    if bits_per_second >= 1_000_000 {
        format!("{} Mbps", (bits_per_second as f64 / 1e6).round())
    } else {
        format!("{} kbps", (bits_per_second as f64 / 1e3).round())
    }
}

pub fn strain_title(warning: &StrainWarning) -> &'static str {
    match (warning.role, warning.cause) {
        (StrainRole::Viewer, StrainCause::Network(_)) => {
            "Your connection can't keep up with this share"
        }
        (StrainRole::Viewer, StrainCause::Computer) => {
            "This computer can't show this share smoothly"
        }
        (StrainRole::Sharer, StrainCause::Network(_)) => {
            "Your upload can't carry this quality. Try a lower one."
        }
        (StrainRole::Sharer, StrainCause::Computer) => {
            "Your computer can't encode this quality smoothly"
        }
    }
}

pub fn strain_detail(warning: &StrainWarning) -> Option<String> {
    let StrainCause::Network(Some(s)) = warning.cause else {
        return None;
    };
    Some(format!(
        "About {} gets through. This share needs {}.",
        mbps(s.reaching),
        mbps(s.needs)
    ))
}
/// A window that keeps itself out of captures (Signal's does): a share of it would show nothing.
pub fn window_blocks_sharing(window: &str) -> String {
    format!("{window} doesn't allow its window to be shared.")
}
/// Windows sends no picture of a minimized window, so a share of one would show nothing.
pub fn window_minimized(window: &str) -> String {
    format!("{window} is minimized. Restore it, then share it.")
}

pub const VOICE_ENDED_REPLACED: &str = "You signed in on another device.";

pub const CONNECTION_LOST: &str = "Connection lost";
/// A Lobby switched back to: its feed is the one it showed when it was left.
pub const CONNECTION_CONNECTING: &str = "Connecting";
pub const CATCHING_UP: &str = "This is what you saw last. What you missed comes next.";
/// No attempt count: the app keeps trying, and a countdown would not calm anyone.
pub const KEEPS_TRYING: &str = "Trying to reconnect. Your Messages will wait.";
pub const TRYING_NOW: &str = "Trying now. Your Messages will wait.";
pub const OFFLINE: &str = "You're offline";
pub const OFFLINE_DETAIL: &str = "Check your internet connection. We'll keep trying.";
/// Retrying would repeat what the server refuses.
pub const REFUSED_BY_LOBBY: &str =
    "The Lobby keeps closing this connection. Update the app, then try again.";
pub const REPLACED_DETAIL: &str = "Reconnect to use this device again. The other one disconnects.";
pub const UPDATE_REQUIRED: &str = "Update required";

pub const KEEP_TRYING_FIRST: &str = "We'll keep trying. Your other Lobbies are in the switcher.";
pub const TRYING_NOW_FIRST: &str = "Trying now. Your other Lobbies are in the switcher.";

/// A Lobby that does not answer: a never-reached one and an Unavailable one say it alike.
pub fn isnt_answering(lobby: &str) -> String {
    format!("{lobby} isn't answering")
}

pub fn unavailable_detail(last_connected: Option<&str>) -> String {
    let next = "We'll try again when your PC wakes.";
    match last_connected {
        Some(t) => format!("Last connected {t}. {next}"),
        None => next.to_owned(),
    }
}

pub const LOBBIES: &str = "Lobbies";
pub const SWITCHER_SHORTCUT: &str = "Ctrl+1–9 switches Lobbies";
pub const SIGN_IN_NEEDED: &str = "Sign in needed";
pub const ADD_LOBBY: &str = "Add Lobby…";
pub const ADD_LOBBY_TITLE: &str = "Add Lobby";

pub fn add_lobby_hint(current: &str) -> String {
    format!("You'll stay in {current} until it answers.")
}

pub fn already_in(lobby: &str) -> String {
    format!("You're already in {lobby}.")
}

pub fn back_to(lobby: &str) -> String {
    format!("Back to {lobby}")
}
pub const STATE_WRITE_FAILED: &str = "Not saved on this device. We try again with the next change.";
pub const STATE_READ_FAILED: &str =
    "Couldn't load your saved Lobbies, so none are listed. Enter your Lobby's address again.";

pub const STATE_NOT_LOADED: &str =
    "They couldn't be loaded, so none are listed. Enter your Lobby's address again.";

pub const STATE_CORRUPT_NOTICE: &str =
    "Your saved Lobbies couldn't be read, so we started fresh. The old file was kept.";

pub fn state_corrupt(kept_as: &str) -> String {
    format!("They couldn't be read, so we started fresh. The old file is kept as {kept_as}.")
}

pub fn switch_title(to: &str) -> String {
    format!("Switch to {to}?")
}

/// Leaving costs Voice, and the Screen Share too (everyone watching it loses it).
pub fn switch_leaves_voice(from: &str, sharing: bool) -> String {
    if sharing {
        format!("Switching Lobbies will stop your Screen Share and leave Voice in {from}.")
    } else {
        format!("Switching Lobbies will leave Voice in {from}.")
    }
}

pub const SWITCH_AND_LEAVE: &str = "Leave Voice and switch";
pub const STAY: &str = "Stay";

// Entry's screens, in the order a person meets them.

pub fn stage_label(stage: Stage) -> &'static str {
    match stage {
        Stage::Lobby => "Lobby",
        Stage::Email => "Email",
        Stage::Code => "Code",
        Stage::Name => "Name",
    }
}

pub const CONNECT_TITLE: &str = "Connect to a Lobby";
pub const CONNECT_SUBTITLE: &str = "Enter the address your friends gave you.";
pub const ADDRESS_LABEL: &str = "Lobby address";
pub const ADDRESS_PLACEHOLDER: &str = "lobby.example.com";
pub const CONNECT: &str = "Connect";
pub const CONNECTING: &str = "Connecting…";

pub fn welcome_to(lobby: &str) -> String {
    format!("Welcome to {lobby}")
}

pub const EMAIL_LABEL: &str = "Email";
pub const EMAIL_PLACEHOLDER: &str = "you@example.com";
pub const EMAIL_HINT: &str = "We'll email you an 8-digit code.";

/// Added to [`EMAIL_HINT`] while the Lobby has no Members. The title already names the Lobby.
pub const NOT_SET_UP_HINT: &str = "It's new: its owner signs in first.";

/// Added to [`EMAIL_HINT`] while the Lobby is locked against Ask to Join.
pub const EMAIL_LOCKED_HINT: &str = "It isn't taking new Members right now.";

pub const SEND_CODE: &str = "Send code";
pub const SENDING_CODE: &str = "Sending…";
pub const CHANGE_LOBBY: &str = "Change Lobby";
pub const ENTER_EMAIL: &str = "That doesn't look like an email address.";

pub const CHECK_YOUR_EMAIL: &str = "Check your email";
/// Anyone can ask for a code to an address, so the mail that answers this request is named.
pub fn code_hint(label: Option<&RequestLabel>) -> String {
    match label {
        Some(label) => {
            format!("Use the code from the email for request {label}. It expires in 10 minutes.")
        }
        None => "Paste it or type it in. It expires in 10 minutes.".into(),
    }
}
pub const CONTINUE: &str = "Continue";
pub const CHECKING: &str = "Checking…";
pub const SEND_NEW_CODE: &str = "Send a new code";
pub const CODE_RESENT: &str = "A new code is on its way.";

pub fn set_up_title(lobby: &str) -> String {
    format!("Set up {lobby}")
}

pub fn ask_to_join_title(lobby: &str) -> String {
    format!("Ask to Join {lobby}")
}

pub const NAME_LABEL: &str = "Name";
pub const NAME_PLACEHOLDER: &str = "alice";
pub const CREATE_LOBBY_HINT: &str = "You'll be its owner and Vouch for everyone who Asks to Join.";
pub const ASK_TO_JOIN_HINT: &str = "Members see this name when they Vouch for you.";

pub const CREATE_LOBBY: &str = "Create Lobby";
pub const NAME_LENGTH: &str = "A name is 3 to 32 characters.";
pub const NAME_CHARACTERS: &str = "A name uses letters, numbers, _ and -.";

/// What a name the protocol refused says, by the rule it broke.
pub fn name_rule(typed: &str) -> &'static str {
    if limits::USERNAME_LEN.contains(&typed.trim().chars().count()) {
        NAME_CHARACTERS
    } else {
        NAME_LENGTH
    }
}

pub const SETTING_UP: &str = "Setting up…";
pub const ASK_TO_JOIN: &str = "Ask to Join";
pub const ASKING: &str = "Asking…";
pub const LOCKED_HINT: &str = "This Lobby isn't taking join requests right now.";
/// A join refused because nobody has set the Lobby up yet (a race the verify step misses).
pub const NOT_OPEN_YET: &str = "This Lobby isn't open yet. Try again once its owner set it up.";

pub const WAITING_TITLE: &str = "Waiting for a Vouch";
pub const AWAITING_VOUCH_LATER: &str =
    "You can close the app. Enter your email here later to see where it stands.";
pub const CHECK_NOW: &str = "Check now";
pub const OPENING_LOBBY: &str = "Opening the Lobby…";
pub const USE_DIFFERENT_EMAIL: &str = "Use a different email";

pub fn not_open_yet_title(lobby: &str) -> String {
    format!("{lobby} isn't open yet")
}

pub fn closed_for_now_title(lobby: &str) -> String {
    format!("{lobby} is closed for now")
}

pub const DECLINED_TITLE: &str = "Not this time";
pub const NOT_OPEN_YET_LINE: &str = "Its owner sets it up first. Try again once they have.";
pub const LOCKED_LINE: &str = "It isn't taking new Members. Ask someone in it to open requests.";
/// The server mails no more once this device reached the per-email cap.
pub fn codes_capped(sent: u32, label: Option<&RequestLabel>) -> String {
    let codes = if sent == 1 {
        "1 code"
    } else {
        &format!("{sent} codes")
    };
    match label {
        Some(label) => {
            format!("We've sent {codes}. Use the one for request {label}; check your spam folder.")
        }
        None => format!("We've sent {codes}. Check your spam folder."),
    }
}
/// The resend button's words while it waits out the minute after the last code.
pub const SEND_NEW_CODE_WAIT: &str = "You can send a new code in a minute.";
pub const CODE_WRONG: &str = "That code isn't right. Check the email and try again.";
pub const CODE_EXPIRED: &str = "That code has expired. Send a new one.";
pub const CODE_TOO_MANY: &str = "Too many tries for this code. Send a new one.";
pub const TICKET_EXPIRED: &str = "Your email check expired. Go back and send a new code.";
pub const USERNAME_TAKEN: &str = "That name is taken. Try another.";
/// The email became a Member's between its code and the name.
pub const EMAIL_TAKEN: &str = "You're already a Member here. Go back and send a new code.";
pub const AWAITING_VOUCH: &str =
    "Your request is in. When a Member Vouches for you, you're signed in right here.";
pub const NOT_YET: &str = "Not yet. This screen keeps checking by itself.";
pub const VOUCHED_IN: &str = "You're in!";
pub const DECLINED: &str = "A Member declined your request to join. You can ask again later.";
pub const WAIT_EXPIRED: &str = "Enter your email again to see where your request stands.";
pub const SIGNED_OUT: &str = "Signed out. Sign in again whenever you like.";

/// A 429's wait in words, never a ticking number.
fn try_again_in(wait: Option<Duration>) -> &'static str {
    match wait.map(|w| w.as_secs()) {
        Some(0..=15) => "Try again in a few seconds.",
        Some(16..=75) | None => "Try again in a minute.",
        Some(_) => "Try again in a few minutes.",
    }
}

pub fn too_many_tries(e: &ApiError) -> String {
    format!(
        "Too many tries from this network. {}",
        try_again_in(e.retry_after)
    )
}

pub fn request_code_error(e: &ApiError) -> String {
    auth_error(e, |_| None)
}

pub fn code_error(e: &ApiError) -> String {
    auth_error(e, |e| match e.kind {
        ApiKind::Server(ErrorCode::AuthFailed)
            if e.detail == Some(ErrorDetail::TooManyAttempts) =>
        {
            Some(CODE_TOO_MANY.to_owned())
        }
        ApiKind::Server(ErrorCode::AuthFailed) => Some(CODE_WRONG.to_owned()),
        ApiKind::Server(ErrorCode::AuthExpired) => Some(CODE_EXPIRED.to_owned()),
        _ => None,
    })
}

pub fn join_error(e: &ApiError) -> String {
    auth_error(e, |e| match (e.kind, e.detail) {
        (ApiKind::Server(ErrorCode::Conflict), Some(ErrorDetail::UsernameTaken)) => {
            Some(USERNAME_TAKEN.to_owned())
        }
        (ApiKind::Server(ErrorCode::Conflict), _) => Some(EMAIL_TAKEN.to_owned()),
        (ApiKind::Server(ErrorCode::AuthFailed | ErrorCode::AuthExpired), _) => {
            Some(TICKET_EXPIRED.to_owned())
        }
        (ApiKind::Server(ErrorCode::Forbidden), Some(ErrorDetail::Locked)) => {
            Some(LOCKED_HINT.to_owned())
        }
        (ApiKind::Server(ErrorCode::Forbidden), Some(ErrorDetail::NotSetUp)) => {
            Some(NOT_OPEN_YET.to_owned())
        }
        (ApiKind::Server(ErrorCode::Forbidden), _) => Some(e.message.clone()),
        _ => None,
    })
}

fn auth_error(e: &ApiError, step: impl FnOnce(&ApiError) -> Option<String>) -> String {
    if let Some(copy) = step(e) {
        return copy;
    }
    match e.kind {
        ApiKind::Server(ErrorCode::RateLimited) => too_many_tries(e),
        ApiKind::Server(_) => e.message.clone(),
        ApiKind::Network | ApiKind::Timeout | ApiKind::Decode => FAILED_TO_CONNECT.to_owned(),
        ApiKind::KeychainUnavailable => KEYCHAIN_UNAVAILABLE.to_owned(),
        ApiKind::SessionExpired => SESSION_EXPIRED.to_owned(),
    }
}

/// Who it was about, then why: the notification's title already says what failed.
fn about(name: Option<&str>, reason: String) -> String {
    match name {
        Some(name) => format!("{name}: {reason}"),
        None => {
            let mut chars = reason.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

pub fn decision_failed(name: Option<&str>, e: &ApiError) -> String {
    let reason = match (e.kind, e.detail) {
        (ApiKind::Server(ErrorCode::Conflict), Some(ErrorDetail::UsernameTaken)) => {
            "their username was taken meanwhile.".to_owned()
        }
        (ApiKind::Server(ErrorCode::Conflict), _) => {
            "their email is in use by a Member.".to_owned()
        }
        (ApiKind::Server(ErrorCode::RateLimited), _) => {
            "too many at once. Try again in a minute.".to_owned()
        }
        (ApiKind::Network | ApiKind::Timeout, _) => NO_CONNECTION_REASON.to_owned(),
        _ => e.message.clone(),
    };
    about(name, reason)
}

const NO_CONNECTION_REASON: &str = "couldn't reach the Lobby. Check your connection.";

pub fn remove_member_confirm(name: &str) -> String {
    format!("Remove {name}? They're signed out right away. Their Messages stay.")
}

/// A Member Card's line under their name; `date` is the day they joined.
pub fn member_since(date: &str) -> String {
    format!("Member since {date}")
}

pub fn make_owner_confirm(name: &str) -> String {
    format!("Make {name} the owner? You'll stay a Member.")
}

/// The owner's two lines on a Member's card end in "…": each asks once more.
pub fn make_owner_line(name: &str) -> String {
    format!("Make {name} the owner…")
}

pub fn remove_member_line(name: &str) -> String {
    format!("Remove {name}…")
}

pub const OWNER_PILL: &str = "Owner";
pub const RENAME: &str = "Rename";
pub const SAVE: &str = "Save";
pub const REMOVE: &str = "Remove";
pub const CHANGE_PICTURE: &str = "Change picture…";
pub const DO_NOT_DISTURB: &str = "Do Not Disturb";
pub const SHOW_VOICE_STATS: &str = "Show Voice stats";
pub const SIGN_OUT: &str = "Sign out";
pub const MAGIC_CODES_GO_HERE: &str = "Your Magic Codes go here";

pub fn owner_action_failed(name: Option<&str>, e: &ApiError) -> String {
    let reason = match (e.kind, e.detail) {
        (ApiKind::Server(ErrorCode::Forbidden), Some(ErrorDetail::NotOwner)) => {
            "only the Lobby's owner can do that.".to_owned()
        }
        (ApiKind::Server(ErrorCode::Conflict), Some(ErrorDetail::CannotRemoveOwner)) => {
            "the Lobby's owner can't be removed.".to_owned()
        }
        (ApiKind::Server(ErrorCode::NotFound), _) => "they are no longer a Member.".to_owned(),
        (ApiKind::Server(ErrorCode::RateLimited), _) => {
            "too many at once. Try again in a minute.".to_owned()
        }
        (ApiKind::Network | ApiKind::Timeout, _) => NO_CONNECTION_REASON.to_owned(),
        _ => e.message.clone(),
    };
    about(name, reason)
}

/// A Lobby's own menu, beside its row in the switcher.
pub const LOBBY_MENU: &str = "Change icon, Leave";
pub const CHANGE_ICON: &str = "Change icon…";
pub const ICON_SEEN_BY_ALL: &str = "Every Member sees it";
pub const LEAVE_LOBBY_LINE: &str = "Leave Lobby…";
/// Why a Lobby's menu is disabled while it needs a new Sign In.
pub const SIGN_IN_FIRST: &str = "Sign in to it first";

pub const LEAVE_LOBBY: &str = "Leave Lobby";

pub fn leave_title(lobby: &str) -> String {
    format!("Leave {lobby}?")
}

pub const LEAVE_CONFIRM: &str = "Your Messages stay. You can Ask to Join again.";
pub const HAND_OVER_DETAIL: &str =
    "You own it, so pick who owns it next. Your Messages stay, and you can Ask to Join again.";
pub const ONLY_MEMBER: &str = "You're its only Member, so nobody can own it next.";

/// The owner's Leave button, once `heir` is picked.
pub fn hand_over_and_leave(heir: Option<&str>) -> String {
    match heir {
        Some(name) => format!("Make {name} the owner and leave"),
        None => "Make owner and leave".to_owned(),
    }
}

/// Another Lobby's action could not start: its Session never connected.
pub fn lobby_unreachable(lobby: &str) -> String {
    format!("Couldn't reach {lobby}. Check your connection.")
}

/// Another Lobby's action could not start: it asked for a new Sign In.
pub fn sign_in_to(lobby: &str) -> String {
    format!("Sign in to {lobby} first.")
}

/// What a refused Leave, rename or picture says; the Member stays as they were. A taken name is
/// told under the field, not as an Issue: see [`name_is_taken`].
pub fn account_failed(e: &ApiError) -> String {
    match e.kind {
        ApiKind::Server(ErrorCode::RateLimited) => too_many_tries(e),
        ApiKind::Network | ApiKind::Timeout => about(None, NO_CONNECTION_REASON.to_owned()),
        _ => e.message.clone(),
    }
}

pub const NAME_TAKEN: &str = "That name is taken.";

/// What a refused avatar says. The Lobby's own words name what is wrong with the file ("Invalid
/// image file", "Unsupported file type").
pub fn picture_failed(e: &PictureError) -> String {
    match e {
        PictureError::Api(e) => account_failed(e),
        e => e.to_string(),
    }
}

pub fn name_is_taken(e: &ApiError) -> bool {
    e.kind == ApiKind::Server(ErrorCode::Conflict) && e.detail == Some(ErrorDetail::UsernameTaken)
}

pub fn issue_title(kind: IssueKind) -> &'static str {
    match kind {
        // Short, so the reason after it fits a compact toast's line.
        IssueKind::SendFailed | IssueKind::SendRefused => "Message not sent",
        IssueKind::ChangeFailed => "Message not changed",
        IssueKind::HistoryFailed | IssueKind::OlderFailed => "Messages",
        IssueKind::VoiceEnded => "Voice ended",
        IssueKind::NoRoute => "Can't reach Voice",
        IssueKind::NoDevice => "No audio device",
        IssueKind::DeviceNotFound => "Device unavailable",
        IssueKind::DeviceInUse => "Device in use",
        IssueKind::MediaPermissionDenied => "Microphone blocked",
        IssueKind::MicSilent => "Microphone silent",
        IssueKind::InputFellBack | IssueKind::OutputFellBack => "Device changed",
        IssueKind::UnmuteCooldown
        | IssueKind::JoinCooldown
        | IssueKind::SignalingRateLimited
        | IssueKind::VoiceRefused => "Voice",
        IssueKind::ShareFailed => "Screen Share",
        IssueKind::PresenceRateLimited => "Presence",
        IssueKind::VouchFailed => "Couldn't Vouch",
        IssueKind::DeclineFailed => "Couldn't decline",
        IssueKind::RemoveFailed => "Couldn't remove",
        IssueKind::TransferFailed => "Ownership",
        IssueKind::RenameFailed => "Couldn't rename",
        IssueKind::PictureFailed => "Couldn't set picture",
        IssueKind::LeaveFailed => "Couldn't leave",
        IssueKind::StateCorrupt | IssueKind::StateReadFailed | IssueKind::StateWriteFailed => {
            "Your Lobbies"
        }
        IssueKind::PayloadTooLarge | IssueKind::AttachRefused => "Not attached",
        IssueKind::UploadFailed => "Upload failed",
        IssueKind::SaveFailed => "Couldn't save",
        IssueKind::OpenFailed => "Couldn't open",
        IssueKind::ServerError => "Lobby",
    }
}

pub fn sent_attachments(count: usize) -> String {
    match count {
        1 => "Sent an attachment".to_owned(),
        n => format!("Sent {n} attachments"),
    }
}

/// An OS toast's title: who wrote, and in which Lobby when the person has more than one.
pub fn message_toast_title(author: &str, lobby: Option<&str>) -> String {
    match lobby {
        Some(lobby) => format!("{author} in {lobby}"),
        None => author.to_owned(),
    }
}

/// The title of the toast for a Message that names this Member, in which Lobby when the person
/// has more than one.
pub fn mention_toast_title(author: &str, lobby: Option<&str>) -> String {
    match lobby {
        Some(lobby) => format!("{author} mentioned you in {lobby}"),
        None => format!("{author} mentioned you"),
    }
}

pub fn asks_to_join(name: &str) -> String {
    format!("{name} asks to join")
}

/// What an Ask to Join says once a Member has decided it (`DECLINED` is the requester's side).
pub const DECISION_VOUCHED: &str = "Vouched";
pub const DECISION_DECLINED: &str = "Declined";

/// No number: the control shows when it is available again.
pub fn issue_copy(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::VoiceStateCooldown => {
            "Easy on the mute button: you can unmute again in a moment."
        }
        ErrorCode::VoiceJoinCooldown => "Joining too often. You can join again in a moment.",
        ErrorCode::RateLimited => "Too many changes. Yours applies in a moment.",
        other => error_code(other),
    }
}

/// The routing detail is for the log, not the person.
pub const NO_ROUTE: &str = "No route from this network. A VPN or firewall may be in the way.";

/// The Issue of a media fault, which is Voice ended with the reason that fits it best.
pub fn media_failure(e: &MediaError) -> (IssueKind, String) {
    match e {
        MediaError::NoRoute(_) => (IssueKind::NoRoute, NO_ROUTE.to_owned()),
        MediaError::NoDevice | MediaError::NoOutputDevice => (IssueKind::NoDevice, e.to_string()),
        MediaError::DeviceNotFound => (IssueKind::DeviceNotFound, e.to_string()),
        MediaError::DeviceBusy => (IssueKind::DeviceInUse, e.to_string()),
        MediaError::PermissionDenied => (IssueKind::MediaPermissionDenied, e.to_string()),
        e => (IssueKind::VoiceEnded, e.to_string()),
    }
}

/// What a device that was left says, with the Issue it is raised as.
pub fn device_fell_back(kind: DeviceKind, wanted: &str) -> (IssueKind, String) {
    match kind {
        DeviceKind::Input => (
            IssueKind::InputFellBack,
            format!("{wanted} isn't connected, so Voice uses the system default microphone."),
        ),
        DeviceKind::Output => (
            IssueKind::OutputFellBack,
            format!("{wanted} isn't connected, so Voice uses the system default speakers."),
        ),
    }
}

pub const DEVICE_SYSTEM_DEFAULT: &str = "System default";
pub const DEVICE_COMMUNICATIONS: &str = "Communications default";
pub const DEVICE_INPUT: &str = "Microphone";
pub const DEVICE_OUTPUT: &str = "Speakers";
pub const DEVICE_NOT_CONNECTED: &str = "not connected";
pub const MIC_SILENT: &str = "Your microphone is sending silence. Check its mute switch and \
     Windows microphone privacy settings.";
/// Under the meter, in the line that is kept for them.
pub const MIC_TOO_QUIET: &str = "Very quiet. Raise the volume.";
pub const MIC_TOO_LOUD: &str = "Too loud. Lower the volume.";
pub const MIC_CLIPPING: &str = "Clipping. Lower the volume.";
pub const NOISE_SUPPRESSION: &str = "Reduce background noise";
pub const ECHO_CANCELLATION: &str = "Cancel echo from speakers";
pub const COMPRESSOR: &str = "Even out loud and quiet speech";
/// It is the microphone's level in Windows, so it changes what every app hears.
pub const MIC_GAIN: &str = "Mic volume, for every app";
pub const MONITOR: &str = "Hear yourself";
/// Under the meter while Monitor is on: speakers would feed the voice back.
pub const MONITOR_HEADPHONES: &str = "Use headphones to avoid feedback.";
pub const SOUNDS: &str = "Play join and leave sounds";

pub fn voice_ended(reason: VoiceEndReason) -> &'static str {
    match reason {
        VoiceEndReason::Timeout => VOICE_TIMED_OUT,
        VoiceEndReason::Failed => VOICE_CONNECTION_FAILED,
        VoiceEndReason::ConnectionLost => "The connection was lost. Rejoin when ready.",
        // The newer device may never have joined Voice: say only what is known.
        VoiceEndReason::Replaced => VOICE_ENDED_REPLACED,
        VoiceEndReason::Kicked => "Your session was ended.",
        VoiceEndReason::Shutdown => "The Lobby is restarting. Rejoin when it is back.",
    }
}

pub fn couldnt_reach(host: &str) -> String {
    format!("Couldn't reach {host}. Check the address, or whether the Lobby is running.")
}

fn not_a_lobby(host: &str) -> String {
    format!("{host} answered, but not as a Lobby. Check the address.")
}

pub fn lobby_info_failed(origin: &crate::LobbyOrigin, e: &crate::LobbyInfoError) -> String {
    let host = origin.host();
    match e {
        crate::LobbyInfoError::Api(api) if api.status.is_some() => not_a_lobby(host),
        crate::LobbyInfoError::Api(_) => couldnt_reach(host),
        _ => e.to_string(),
    }
}

/// Used when the server's own message is empty.
pub fn error_code(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::AuthFailed => SIGN_IN_FAILED,
        ErrorCode::AuthExpired => SESSION_EXPIRED,
        ErrorCode::Forbidden => "This action is not allowed.",
        ErrorCode::InvalidRequest => "The request was invalid.",
        ErrorCode::PayloadTooLarge => PAYLOAD_TOO_LARGE,
        ErrorCode::NotFound => "Not found.",
        ErrorCode::Conflict => "That is already taken.",
        ErrorCode::RateLimited => SENDING_TOO_FAST,
        ErrorCode::Unavailable => "The Lobby is busy or restarting. Try again in a moment.",
        ErrorCode::InternalError => "Something went wrong on the Lobby. Try again.",
        ErrorCode::MessageTooLong => MESSAGE_TOO_LONG,
        ErrorCode::MessageTooNested => MESSAGE_TOO_NESTED,
        ErrorCode::AttachmentInvalid => "Attachment not available. Re-attach and try again.",
        ErrorCode::LimitReached => "A limit was reached. Try again later.",
        ErrorCode::VoiceJoinFailed => "Couldn't join. Try again.",
        ErrorCode::VoiceStateInvalidTransition => "Not possible right now. Try again in a moment.",
        ErrorCode::VoiceNegotiationInvalidState | ErrorCode::VoiceNegotiationFailed => {
            VOICE_CONNECTION_FAILED
        }
        ErrorCode::VoiceNegotiationTimeout => VOICE_TIMED_OUT,
        ErrorCode::VoiceJoinCooldown | ErrorCode::VoiceStateCooldown => {
            "Too many changes. Try again in a moment."
        }
        ErrorCode::NotInVoice => "You're not in Voice.",
        ErrorCode::SignalingRateLimited => "Too many changes at once. Try again in a moment.",
    }
}

pub fn server_error(code: ErrorCode, message: &str) -> String {
    server_error_with(code, message, error_code)
}

/// Generic codes read as what they mean for a Join.
pub fn voice_join_error(code: ErrorCode, message: &str) -> String {
    server_error_with(code, message, |code| match code {
        ErrorCode::LimitReached => "No room left. Try again when someone leaves.",
        ErrorCode::VoiceStateInvalidTransition => "Can't join right now. Try again in a moment.",
        other => error_code(other),
    })
}

pub fn voice_state_error(code: ErrorCode, message: &str) -> String {
    server_error_with(code, message, |code| match code {
        ErrorCode::VoiceStateInvalidTransition => VOICE_NOT_ACTIVE,
        other => error_code(other),
    })
}

pub fn share_error(code: ErrorCode, message: &str) -> String {
    server_error_with(code, message, |code| match code {
        ErrorCode::LimitReached => "Too many at once. Try again when one ends.",
        ErrorCode::NotInVoice => SHARE_NEEDS_VOICE,
        other => error_code(other),
    })
}

pub fn watch_error(code: ErrorCode, message: &str) -> String {
    server_error_with(code, message, |code| match code {
        ErrorCode::NotFound => SHARE_NOT_AVAILABLE,
        ErrorCode::NotInVoice => SHARE_NEEDS_VOICE,
        other => error_code(other),
    })
}

fn server_error_with(
    code: ErrorCode,
    message: &str,
    copy: impl FnOnce(ErrorCode) -> &'static str,
) -> String {
    if message.trim().is_empty() {
        copy(code).to_owned()
    } else {
        message.to_owned()
    }
}

/// The title bar's update control.
pub const UPDATE_RESTART: &str = "Restart to update";
pub const UPDATE_RESTART_SHORT: &str = "Restart";
pub const UPDATE_FAILED: &str = "Update failed";
pub const UPDATING_LABEL: &str = "Updating";
/// The window while the app quits to start its update.
pub const RESTARTING: &str = "Updating Lobby…";

pub fn update_downloading(version: &str, received: u64, total: u64) -> String {
    let mb = |bytes: u64| bytes as f64 / 1e6;
    format!(
        "Downloading Lobby {version}: {:.1} of {:.1} MB",
        mb(received),
        mb(total)
    )
}

pub fn update_ready(version: &str) -> String {
    format!("Lobby {version} is ready. Restart to start using it.")
}

pub fn update_failed(version: &str) -> String {
    format!("Lobby {version} couldn't be downloaded. Click to try again.")
}

/// Settings' Updates section: one line that says where the updater stands.
pub const UPDATES: &str = "Updates";
pub const UPDATE_CHECK_NOW: &str = "Check now";

pub fn updates_local_build(version: &str) -> String {
    format!("Lobby {version}. This build doesn't update itself.")
}

pub fn updates_current(version: &str, found: crate::update::Check) -> String {
    use crate::update::Check;
    match found {
        Check::NotYet => format!("Lobby {version}. It checks for updates every hour."),
        Check::Checking => format!("Lobby {version}. Checking for updates…"),
        Check::UpToDate => format!("Lobby {version} is up to date."),
        Check::Unreachable => {
            format!("Lobby {version}. Couldn't reach GitHub. It tries again within the hour.")
        }
    }
}

pub fn updates_downloading(version: &str, received: u64, total: u64) -> String {
    let percent = (received.min(total) * 100).checked_div(total).unwrap_or(0);
    format!("Downloading Lobby {version}… {percent}%")
}

pub fn updates_ready(version: &str) -> String {
    format!("Lobby {version} is ready.")
}

pub fn updates_failed(version: &str) -> String {
    format!("Lobby {version} couldn't be downloaded.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new `ErrorCode` fails to compile here until it has a place in the walk.
    fn next(code: ErrorCode) -> Option<ErrorCode> {
        use ErrorCode::*;
        match code {
            AuthFailed => Some(AuthExpired),
            AuthExpired => Some(Forbidden),
            Forbidden => Some(InvalidRequest),
            InvalidRequest => Some(PayloadTooLarge),
            PayloadTooLarge => Some(NotFound),
            NotFound => Some(Conflict),
            Conflict => Some(RateLimited),
            RateLimited => Some(LimitReached),
            LimitReached => Some(Unavailable),
            Unavailable => Some(InternalError),
            InternalError => Some(AttachmentInvalid),
            AttachmentInvalid => Some(MessageTooLong),
            MessageTooLong => Some(MessageTooNested),
            MessageTooNested => Some(VoiceJoinCooldown),
            VoiceJoinCooldown => Some(VoiceStateCooldown),
            VoiceStateCooldown => Some(VoiceJoinFailed),
            VoiceJoinFailed => Some(NotInVoice),
            NotInVoice => Some(VoiceStateInvalidTransition),
            VoiceStateInvalidTransition => Some(VoiceNegotiationInvalidState),
            VoiceNegotiationInvalidState => Some(VoiceNegotiationFailed),
            VoiceNegotiationFailed => Some(VoiceNegotiationTimeout),
            VoiceNegotiationTimeout => Some(SignalingRateLimited),
            SignalingRateLimited => None,
        }
    }

    #[test]
    fn every_error_code_has_words_and_none_say_backend() {
        let mut code = Some(ErrorCode::AuthFailed);
        while let Some(c) = code {
            for words in [error_code(c), issue_copy(c)] {
                assert!(!words.is_empty(), "{c:?} has no words");
                assert!(!words.contains("Backend"), "{c:?} leaks jargon: {words:?}");
            }
            code = next(c);
        }
    }

    #[test]
    fn the_cap_says_how_many_codes_went_out_in_the_singular_and_the_plural() {
        let said = |sent| codes_capped(sent, None);
        assert!(said(1).starts_with("We've sent 1 code."), "{}", said(1));
        assert!(said(2).starts_with("We've sent 2 codes."), "{}", said(2));
        assert!(said(5).starts_with("We've sent 5 codes."), "{}", said(5));
    }
}

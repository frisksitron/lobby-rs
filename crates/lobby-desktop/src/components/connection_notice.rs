//! The live connection entry of the Notification Center, and the title bar's tint. Both are a pure
//! function of the Session's [`Phase`]: [`connection_notice`] is `None` while all is well, and
//! otherwise says what is going on in a title and one sentence, with the action that helps. The
//! entry is not an Issue: it cannot be dismissed, and it goes away by itself once the Lobby is
//! connected again, which the clearing tint says on its own.

use gpui_kit::{App, Hsla, SharedString, component::ActiveTheme as _};
use lobby_client::{LobbyState, Phase, Severity, copy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionTone {
    /// Finding its way back: the tint alone says so.
    Busy,
    Attention,
    Danger,
}

impl ConnectionTone {
    /// How the entry and its toast are drawn: like an Issue of that severity.
    pub fn severity(self) -> Severity {
        match self {
            Self::Danger => Severity::Error,
            Self::Busy | Self::Attention => Severity::Warning,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionAction {
    TryNow,
    Reconnect,
    ChangeLobby,
}

impl ConnectionAction {
    pub fn id(self) -> &'static str {
        match self {
            Self::TryNow => "try-now",
            Self::Reconnect => "reconnect",
            Self::ChangeLobby => "change-lobby",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TryNow => "Try now",
            Self::Reconnect => "Reconnect",
            Self::ChangeLobby => "Change Lobby",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionNotice {
    pub tone: ConnectionTone,
    pub title: SharedString,
    pub detail: SharedString,
    pub action: Option<ConnectionAction>,
    /// A dial in progress: its Try now waits for the outcome.
    pub action_enabled: bool,
    pub secondary: Option<ConnectionAction>,
    /// The person has to act, so it also arrives as a toast, once per entering the state.
    pub toast: bool,
}

pub fn connection_notice(
    state: &LobbyState,
    lobby: &str,
    last_connected: Option<&str>,
) -> Option<ConnectionNotice> {
    let lobby = if lobby.trim().is_empty() {
        "The Lobby"
    } else {
        lobby
    };
    let notice = |tone, title: String, detail: &str| ConnectionNotice {
        tone,
        title: title.into(),
        detail: detail.to_owned().into(),
        action: None,
        action_enabled: true,
        secondary: None,
        toast: false,
    };
    let try_now = Some(ConnectionAction::TryNow);
    let change = Some(ConnectionAction::ChangeLobby);
    // Not connected since this view opened. A Lobby switched back to already shows what it
    // showed when it was left.
    let opened = last_connected.is_none();
    Some(match &state.phase {
        Phase::Connected | Phase::NeedsAuth => return None,
        // The first dial of a Lobby just opened usually succeeds within a moment: saying so would
        // flash the bar and the bell every time.
        Phase::Connecting { attempt: 0 } if state.me.is_none() => return None,
        // The feed is not live yet, and the shimmering label says so.
        Phase::Connecting { attempt: 0 } if opened => notice(
            ConnectionTone::Busy,
            copy::CONNECTION_CONNECTING.to_owned(),
            copy::CATCHING_UP,
        ),
        // Never reached since it was opened (a restored Lobby whose server is down): nothing was
        // lost, so "Connection lost" does not fit. Other Lobbies are one click away.
        Phase::Connecting { .. } | Phase::Reconnecting { .. } if opened => {
            let dialing = matches!(state.phase, Phase::Connecting { .. });
            ConnectionNotice {
                action: try_now,
                action_enabled: !dialing,
                ..notice(
                    ConnectionTone::Attention,
                    copy::isnt_answering(lobby),
                    if dialing {
                        copy::TRYING_NOW_FIRST
                    } else {
                        copy::KEEP_TRYING_FIRST
                    },
                )
            }
        }
        Phase::Connecting { .. } => ConnectionNotice {
            action: try_now,
            action_enabled: false,
            ..notice(
                ConnectionTone::Busy,
                copy::CONNECTION_LOST.to_owned(),
                copy::TRYING_NOW,
            )
        },
        Phase::Reconnecting { .. } => ConnectionNotice {
            action: try_now,
            ..notice(
                ConnectionTone::Busy,
                copy::CONNECTION_LOST.to_owned(),
                copy::KEEPS_TRYING,
            )
        },
        Phase::Offline { .. } => ConnectionNotice {
            action: try_now,
            toast: true,
            ..notice(
                ConnectionTone::Attention,
                copy::OFFLINE.to_owned(),
                copy::OFFLINE_DETAIL,
            )
        },
        Phase::Unavailable { attempts: 0 } => ConnectionNotice {
            action: try_now,
            secondary: change,
            toast: true,
            ..notice(
                ConnectionTone::Danger,
                copy::isnt_answering(lobby),
                copy::REFUSED_BY_LOBBY,
            )
        },
        Phase::Unavailable { .. } => ConnectionNotice {
            action: try_now,
            secondary: change,
            toast: true,
            ..notice(
                ConnectionTone::Danger,
                copy::isnt_answering(lobby),
                &copy::unavailable_detail(last_connected),
            )
        },
        Phase::SessionReplaced => ConnectionNotice {
            action: Some(ConnectionAction::Reconnect),
            toast: true,
            ..notice(
                ConnectionTone::Attention,
                copy::SIGNED_IN_ELSEWHERE.to_owned(),
                copy::REPLACED_DETAIL,
            )
        },
        Phase::ProtocolMismatch => ConnectionNotice {
            secondary: change,
            toast: true,
            ..notice(
                ConnectionTone::Danger,
                copy::UPDATE_REQUIRED.to_owned(),
                copy::PROTOCOL_MISMATCH,
            )
        },
    })
}

pub fn tone_color(tone: ConnectionTone, cx: &App) -> Hsla {
    let theme = cx.theme();
    match tone {
        ConnectionTone::Busy => theme.warning,
        ConnectionTone::Attention => theme.info,
        ConnectionTone::Danger => theme.danger,
    }
}

/// The title bar's wash: the glanceable signal that the Lobby is not calm.
pub fn bar_tint(tone: ConnectionTone, cx: &App) -> Hsla {
    let theme = cx.theme();
    match tone {
        ConnectionTone::Busy => theme.warning,
        ConnectionTone::Attention => theme.muted_foreground,
        ConnectionTone::Danger => theme.danger,
    }
}

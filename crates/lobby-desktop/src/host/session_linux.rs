//! Lobby on Linux is a Wayland app. GPUI is built with its X11 backend too (gpui-kit turns it on
//! and no manifest here can turn it off), so what keeps Lobby off X11 is that `host::application`
//! allows GPUI no other mode, and that [`exit_unless_wayland_session`] ends the process before
//! GPUI would panic for want of a session.

use std::{env, process};

use tracing::debug;
use wayland_client::Connection;

/// What `exit_unless_wayland_session` finds wrong. The message is the sentence the person reads.
#[derive(Debug, thiserror::Error)]
enum NoWayland {
    #[error("Lobby needs a Wayland session, and WAYLAND_DISPLAY is not set.")]
    NotSet,
    #[error(
        "Lobby needs a Wayland session, and the compositor named by WAYLAND_DISPLAY={name} \
         does not answer ({reason})."
    )]
    Unreachable { name: String, reason: String },
}

/// Ends the process at once, with a sentence on stderr and exit code 1, when there is no Wayland
/// session to open a window in. Call it once a start is known to want a window: setup, uninstall
/// and a second launch need no session.
pub fn exit_unless_wayland_session() {
    if let Err(error) = check() {
        eprintln!("{error}");
        process::exit(1);
    }
}

/// Says whether the launcher passed an activation token (the desktop's permission to take the
/// focus, which GPUI uses for the first window), never what it is: it is as good as a password
/// until it is used.
pub fn note_launch_token() {
    debug!(
        activation_token =
            env::var_os("XDG_ACTIVATION_TOKEN").is_some_and(|token| !token.is_empty()),
        "the launcher's startup notification"
    );
}

fn check() -> Result<(), NoWayland> {
    let name = env::var_os("WAYLAND_DISPLAY")
        .filter(|name| !name.is_empty())
        .ok_or(NoWayland::NotSet)?;
    // A socket the parent opened for us is GPUI's to take, and connecting here would use it up.
    if env::var_os("WAYLAND_SOCKET").is_some() {
        return Ok(());
    }
    Connection::connect_to_env()
        .map(drop)
        .map_err(|error| NoWayland::Unreachable {
            name: name.to_string_lossy().into_owned(),
            reason: error.to_string(),
        })
}

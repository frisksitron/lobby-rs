//! The Sharer's ink overlay on the real screen. It captures the primary monitor with Windows
//! Graphics Capture as a share does, and checks three things:
//!
//! - A monitor share: a magenta marker window near the right edge, the overlay over the monitor
//!   with ink across the marker and beside it. Left out of captures, the picture shows the marker
//!   whole with no ink and no black patch; let back in, it shows the ink. A click on the ink
//!   lands on the marker.
//! - A watched share's picture: the overlay paints a stroke over a spot of the marker, as it
//!   paints over the picture in a Lobby window on the shared monitor. The ink must land on the
//!   spot's pixels.
//! - A window share: a second process opens a magenta window; the overlay covers it, sits just
//!   above it and follows it as it moves, changes size, is minimised and restored, and is
//!   raised. Left out of captures, neither the monitor's nor the window's own picture shows ink.
//!
//! The focus must never move. The pictures hold whatever else is on the screen: keep them local.
//! It shows windows on the person's screen for several seconds and never takes the focus, nor
//! moves the pointer. Run it only with the person's leave:
//!
//! ```sh
//! cargo run -p lobby-desktop --example overlay_check -- <out dir>
//! ```

#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    win::main()
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("overlay check: runs on Windows only")
}

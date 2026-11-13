//! The session's idle time, asked on a thread of its own: the UI only reads the last answer. A bus
//! or a compositor that hangs, stops or restarts must never stall a GPUI update
//! (`Shell::check_idle` runs inside one).
//!
//! Wayland's `ext-idle-notify-v1` is asked first. GNOME's compositor (Mutter) does not offer it,
//! so there, and only there, the answer comes from Mutter's own idle monitor on the session bus.
//! `work` is the one place that decides; the rest of the app reads `idle_time` and cannot tell
//! which source answered.

mod mutter;
mod wayland;

use std::{
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use tracing::{debug, info, warn};

/// How often a source answers again: an answer is a sample of the moment it was taken.
const POLL: Duration = Duration::from_secs(5);
/// Between a Wayland connection for the idle time that failed or was lost and the next try.
const WAYLAND_RETRY: Duration = Duration::from_secs(30);
/// An answer older than this is no answer: while the worker is stuck in a hung connect, the UI
/// reads `None`. Three polls, so one slow call does not make it flicker.
const STALE_AFTER: Duration = Duration::from_secs(15);

/// The last answer and when the worker read it.
struct Sample {
    idle: Duration,
    read_at: Instant,
}

static LATEST: Mutex<Option<Sample>> = Mutex::new(None);

/// The worker thread's owner: dropping it stops the loop (its wait is a channel that closes). The
/// thread is not joined: it may sit in a connect that has no timeout, and then it ends with the
/// process.
pub struct IdleWatch {
    _stop: mpsc::Sender<()>,
    _thread: thread::JoinHandle<()>,
}

impl IdleWatch {
    /// `idle_after` is the idle time the app cares about: a compositor that reports only "idle"
    /// and "active" is asked to say "idle" once the session has been without input that long.
    /// `None` when there is no thread for it: idle presence is then off.
    pub fn start(idle_after: Duration) -> Option<Self> {
        let (stop, stopped) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("idle-time".into())
            .spawn(move || work(idle_after, &stopped));
        match spawned {
            Ok(thread) => Some(Self {
                _stop: stop,
                _thread: thread,
            }),
            Err(error) => {
                warn!(%error, "no thread for the idle time: idle presence is off");
                None
            }
        }
    }
}

/// How long the session has had no keyboard or mouse input, as of the worker's last answer. The
/// Wayland protocol answers only "at least the idle limit" or "no time at all". `None` while there
/// is no answer (the compositor or the bus is away), when the last one is stale and when no
/// [`IdleWatch`] runs. Never blocks.
pub(super) fn idle_time() -> Option<Duration> {
    let latest = LATEST.lock().unwrap_or_else(|e| e.into_inner());
    let sample = latest.as_ref()?;
    (sample.read_at.elapsed() <= STALE_AFTER).then_some(sample.idle)
}

fn work(idle_after: Duration, stopped: &mpsc::Receiver<()>) {
    decide(
        || wayland::watch(idle_after, stopped),
        || mutter::watch(stopped),
        || {
            // Nothing is ever sent: the wait ends early only when the owner is dropped.
            matches!(
                stopped.recv_timeout(WAYLAND_RETRY),
                Err(mpsc::RecvTimeoutError::Timeout)
            )
        },
    );
}

/// The one place that decides which source answers. Only a compositor that says it has no
/// `ext-idle-notify-v1` sends the worker to Mutter for good; a connection that failed or was lost
/// is tried again after [`WAYLAND_RETRY`], for as long as `keep_waiting` says the owner is there.
fn decide(
    mut wayland: impl FnMut() -> wayland::Ended,
    mutter: impl FnOnce(),
    mut keep_waiting: impl FnMut() -> bool,
) {
    loop {
        match wayland() {
            wayland::Ended::Stopped => return publish(None),
            wayland::Ended::NoProtocol => {
                info!(
                    "idle time from Mutter's idle monitor: the compositor has no ext-idle-notify-v1"
                );
                return mutter();
            }
            wayland::Ended::Failed => {
                publish(None);
                if !keep_waiting() {
                    return;
                }
                debug!("trying the compositor's idle protocol again");
            }
        }
    }
}

fn publish(idle: Option<Duration>) {
    let sample = idle.map(|idle| Sample {
        idle,
        read_at: Instant::now(),
    });
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = sample;
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use wayland::Ended;

    /// Runs `decide` over `answers` and says how many waits and Mutter starts it made.
    fn run(answers: &[Ended], owner_stays: usize) -> (usize, usize, usize) {
        let asked = Cell::new(0);
        let waits = Cell::new(0);
        let mutters = Cell::new(0);
        decide(
            || {
                let at = asked.get();
                asked.set(at + 1);
                answers[at]
            },
            || mutters.set(mutters.get() + 1),
            || {
                waits.set(waits.get() + 1);
                waits.get() <= owner_stays
            },
        );
        (asked.get(), waits.get(), mutters.get())
    }

    #[test]
    fn a_lost_connection_is_tried_again_and_only_a_missing_protocol_goes_to_mutter() {
        // KDE or sway: the connection fails twice, then answers until the owner goes.
        assert_eq!(
            run(&[Ended::Failed, Ended::Failed, Ended::Stopped], 5),
            (3, 2, 0)
        );
        // The owner is dropped while waiting to try again: no further try, no Mutter.
        assert_eq!(run(&[Ended::Failed], 0), (1, 1, 0));
        // GNOME: the compositor answers and has no protocol. Mutter, once, with no waiting.
        assert_eq!(run(&[Ended::NoProtocol], 5), (1, 0, 1));
        // A failed connection that then turns out to be GNOME ends at Mutter too.
        assert_eq!(run(&[Ended::Failed, Ended::NoProtocol], 5), (2, 1, 1));
    }
}

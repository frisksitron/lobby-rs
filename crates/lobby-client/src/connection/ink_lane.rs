//! Where outgoing ink waits for the socket, apart from the reliable commands: it never takes a
//! place in their queue, and it keeps only the newest two frames. A frame that waited for a socket
//! that has gone is dropped, not sent on the next.

use std::{collections::VecDeque, sync::Mutex};

use tokio::sync::Notify;
use tracing::debug;

use super::Command;

/// Frames waiting for the socket. A pen outruns a slow link, and old ink is worth nothing.
pub(crate) const INK_UNSENT_MAX: usize = 2;

#[derive(Default)]
pub(crate) struct InkLane {
    waiting: Mutex<VecDeque<Command>>,
    ready: Notify,
}

impl InkLane {
    /// Queues `cmd`, dropping the oldest frame beyond [`INK_UNSENT_MAX`].
    pub(crate) fn push(&self, cmd: Command) {
        {
            let mut waiting = self.waiting.lock().expect("ink lane lock");
            waiting.push_back(cmd);
            while waiting.len() > INK_UNSENT_MAX {
                waiting.pop_front();
            }
        }
        self.ready.notify_one();
    }

    /// The oldest frame made for `socket`; frames made for an earlier one are dropped.
    pub(crate) fn pop(&self, socket: u64) -> Option<String> {
        let mut waiting = self.waiting.lock().expect("ink lane lock");
        while let Some(cmd) = waiting.pop_front() {
            if cmd.socket == socket {
                return Some(cmd.text);
            }
            debug!(
                socket = cmd.socket,
                "dropped ink queued for an earlier socket"
            );
        }
        None
    }

    /// Resolves when a frame was pushed since the last time it resolved.
    pub(crate) async fn ready(&self) {
        self.ready.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(socket: u64, text: &str) -> Command {
        Command {
            socket,
            text: text.into(),
        }
    }

    #[test]
    fn only_the_newest_two_frames_wait() {
        let lane = InkLane::default();
        for text in ["a", "b", "c", "d"] {
            lane.push(frame(1, text));
        }
        assert_eq!(lane.pop(1).as_deref(), Some("c"));
        assert_eq!(lane.pop(1).as_deref(), Some("d"));
        assert_eq!(lane.pop(1), None);
    }

    #[test]
    fn frames_made_for_an_earlier_socket_are_dropped_after_a_reconnect() {
        let lane = InkLane::default();
        lane.push(frame(1, "old"));
        lane.push(frame(1, "older"));
        assert_eq!(lane.pop(2), None);
        lane.push(frame(2, "new"));
        assert_eq!(lane.pop(2).as_deref(), Some("new"));
    }
}

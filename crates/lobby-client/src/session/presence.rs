//! This client's presence: Do Not Disturb and idle, re-sent whenever the server's snapshot differs.

use std::time::{Duration, Instant};

use lobby_protocol::{ClientMsg, ErrorBody, Presence, PresenceSet};
use tracing::{info, warn};

use super::{Command, Session};
use crate::Phase;

/// A refused `PRESENCE_SET` is retried after the server's `retryAfterMs`, but never later than
/// this.
pub(super) const PRESENCE_RETRY_MAX: Duration = Duration::from_secs(10);

impl Session {
    pub fn desired_presence(&self) -> Presence {
        if self.dnd {
            Presence::Dnd
        } else if self.idle {
            Presence::Idle
        } else {
            Presence::Online
        }
    }

    pub(super) fn set_dnd(&mut self, on: bool, now: Instant) {
        if self.dnd != on {
            info!(dnd = on, "presence choice");
        }
        self.dnd = on;
        self.sync_presence(now);
    }

    pub fn dnd(&self) -> bool {
        self.dnd
    }

    pub fn idle(&self) -> bool {
        self.idle
    }

    pub(super) fn set_idle(&mut self, idle: bool, now: Instant) {
        if self.idle != idle {
            info!(idle, "idle changed");
        }
        self.idle = idle;
        self.sync_presence(now);
    }

    pub(super) fn sync_presence(&mut self, now: Instant) {
        let desired = self.desired_presence();
        // The next IDENTIFY says it in any case.
        self.conn.set_wanted_presence(desired);
        if self.state.phase != Phase::Connected || self.presence_retry_at.is_some() {
            return;
        }
        let shown = self.state.me_presence();
        let on_its_way = self.presence_sent.as_ref().map(|(_, p)| *p);
        if shown.is_none() || shown == Some(desired) || on_its_way == Some(desired) {
            return;
        }
        let set = ClientMsg::PresenceSet(PresenceSet { presence: desired });
        match self.command(set, Command::PresenceSet, now) {
            Ok(rid) => {
                info!(presence = ?desired, "presence set");
                self.presence_sent = Some((rid.as_str().to_owned(), desired));
            }
            Err(e) => warn!(error = %e, "could not send PRESENCE_SET"),
        }
    }

    pub(super) fn forget_presence_sent(&mut self, rid: &str) {
        if self.presence_sent.as_ref().is_some_and(|(r, _)| r == rid) {
            self.presence_sent = None;
        }
    }

    /// The snapshot came first; if the wish changed meanwhile, send it now.
    pub(super) fn presence_acked(&mut self, rid: &str, now: Instant) {
        self.forget_presence_sent(rid);
        self.sync_presence(now);
    }

    pub(super) fn presence_refused(&mut self, rid: &str, e: &ErrorBody, now: Instant) {
        warn!(code = ?e.code, "presence refused");
        self.forget_presence_sent(rid);
        self.presence_retry_at = e
            .retry_after_ms
            .map(|ms| now + Duration::from_millis(ms).min(PRESENCE_RETRY_MAX));
    }
}

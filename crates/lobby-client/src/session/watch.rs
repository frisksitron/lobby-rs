//! The call's Watch: what the viewer shows, what the server was asked, and which Sharer the
//! media moves to. One Watch runs from the first click to Stop Watching, with one decoder and
//! one feed; a click on another Sharer is an ask within it.

use std::collections::VecDeque;

use lobby_protocol::UserId;

use super::strain::Strain;
use crate::{media::ShareOpId, state::WatchUi};

#[derive(Debug)]
struct Ask {
    op: ShareOpId,
    sharer: UserId,
    /// No answer came in time. It may still come, and counts when it does.
    late: bool,
}

#[derive(Debug)]
pub(super) struct Watch {
    /// Its decoder's: a report that names another came from an ended Watch.
    pub(super) id: ShareOpId,
    /// The Watches sent and not answered, oldest first. The server answers in that order.
    asked: VecDeque<Ask>,
    /// The Sharer the server watches for this Member, by its last answer.
    held: Option<UserId>,
    /// Whose pictures the feed carries.
    shown: Option<UserId>,
    pub(super) strain: Strain,
}

impl Watch {
    pub(super) fn new(id: ShareOpId, op: ShareOpId, sharer: UserId) -> Watch {
        let mut watch = Watch {
            id,
            asked: VecDeque::new(),
            held: None,
            shown: None,
            strain: Strain::viewer(),
        };
        watch.ask(op, sharer);
        watch
    }

    /// The Sharer the media is to move to: the newest asked for, else the one the server
    /// holds. `None` once nothing is asked or held, and the Watch is over.
    pub(super) fn wanted(&self) -> Option<&UserId> {
        self.asked.back().map(|a| &a.sharer).or(self.held.as_ref())
    }

    /// Whether a click on `sharer` has nothing to send: it is the newest ask, or what the server
    /// holds with nothing asked since. An ask with no answer in time is asked again.
    pub(super) fn has(&self, sharer: &UserId) -> bool {
        match self.asked.back() {
            Some(ask) => ask.sharer == *sharer && !ask.late,
            None => self.held.as_ref() == Some(sharer),
        }
    }

    pub(super) fn ask(&mut self, op: ShareOpId, sharer: UserId) {
        self.asked.push_back(Ask {
            op,
            sharer,
            late: false,
        });
    }

    /// The ask `op`, out of the list, with the ones before it, which went by without an answer.
    fn answer(&mut self, op: ShareOpId) -> Option<UserId> {
        let at = self.asked.iter().position(|a| a.op == op)?;
        self.asked.drain(..at);
        self.asked.pop_front().map(|a| a.sharer)
    }

    /// The server took `op`. `false` if it is not one of this Watch's.
    pub(super) fn accepted(&mut self, op: ShareOpId) -> bool {
        let Some(sharer) = self.answer(op) else {
            return false;
        };
        self.held = Some(sharer);
        true
    }

    /// The server refused `op`, so it still holds what it held.
    pub(super) fn refused(&mut self, op: ShareOpId) -> bool {
        self.answer(op).is_some()
    }

    /// No answer to `op` came in time. It stays asked, so the media keeps wanting it: the server
    /// may have switched. `false` if it is not one of this Watch's.
    pub(super) fn timed_out(&mut self, op: ShareOpId) -> bool {
        let Some(ask) = self.asked.iter_mut().find(|a| a.op == op) else {
            return false;
        };
        ask.late = true;
        true
    }

    pub(super) fn shows(&mut self, sharer: UserId) {
        if self.shown.as_ref() != Some(&sharer) {
            self.strain = Strain::viewer();
        }
        self.shown = Some(sharer);
    }

    /// Drops the Sharers whose shares no longer stream: the server forgets a Watch of an ended
    /// share, and refuses an ask of one. Returns the asks dropped, whose refusal is still to come.
    pub(super) fn keep_streaming(&mut self, streams: impl Fn(&UserId) -> bool) -> Vec<ShareOpId> {
        self.held = self.held.take().filter(&streams);
        self.shown = self.shown.take().filter(&streams);
        let mut gone = Vec::new();
        self.asked.retain(|ask| {
            let streams = streams(&ask.sharer);
            if !streams {
                gone.push(ask.op);
            }
            streams
        });
        gone
    }

    pub(super) fn ui(&self) -> Option<WatchUi> {
        let wanted = self.wanted();
        let sharer = self.shown.as_ref().or(wanted)?.clone();
        Some(WatchUi {
            live: self.shown.is_some(),
            coming: wanted.filter(|w| **w != sharer).cloned(),
            sharer,
            strain: self.strain.warning(),
        })
    }
}

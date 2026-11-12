//! Ink: the strokes on the shares this Member sees, and their own pen.

use std::time::Instant;

use lobby_protocol::{ClientFrame, ClientMsg, DrawingEpoch, DrawingSet, InkRelay, ShareId};
use tracing::{debug, warn};

use super::{Command, Session};
use crate::ink::{InkBoard, PenPoint};

impl Session {
    /// The Sharer's "Let viewers draw". The share's roster entry says whether it is on. Only a
    /// Sharer has the switch, so a refusal here says nothing.
    pub(super) fn set_drawing(&mut self, enabled: bool, now: Instant) {
        if !self.sharing() {
            debug!("drawing set without a share");
            return;
        }
        let msg = ClientMsg::DrawingSet(DrawingSet { enabled });
        if let Err(e) = self.command(msg, Command::DrawingSet, now) {
            warn!(error = %e, "drawing not set");
        }
    }

    pub(super) fn pen_down(&mut self, share: ShareId, at: PenPoint, now: Instant) {
        let watch = self.watch_ui();
        let drawable = self
            .state
            .drawable_shares(watch.as_ref())
            .find(|(id, _)| *id == share);
        let Some((_, epoch)) = drawable else {
            return;
        };
        let Some(me) = self.state.me.clone() else {
            return;
        };
        let (id, point) = self.pen.down(share, epoch, at, now);
        self.board(share, epoch).echo(&me, id, point, now);
        self.send_pen_batches();
    }

    pub(super) fn pen_to(&mut self, at: PenPoint, now: Instant) {
        let (Some(me), Some((share, epoch, _))) = (self.state.me.clone(), self.pen.drawing())
        else {
            return;
        };
        if let Some((id, point)) = self.pen.to(at, now) {
            self.board(share, epoch).echo(&me, id, point, now);
        }
        self.send_pen_batches();
    }

    /// Lifts the pen: at `at`, where the button came up, or where the pen last was when the
    /// stroke ends some other way (Esc, a button that came up unheard).
    pub(super) fn pen_up(&mut self, at: Option<PenPoint>, now: Instant) {
        let (Some(me), Some((share, epoch, _))) = (self.state.me.clone(), self.pen.drawing())
        else {
            return;
        };
        if let Some((id, point)) = self.pen.ends_at(at, now) {
            self.board(share, epoch).echo(&me, id, point, now);
        }
        if let Some(id) = self.pen.up(now)
            && let Some(board) = self.state.ink.get_mut(&share)
        {
            board.lift(&me, id, now);
        }
        self.send_pen_batches();
    }

    pub fn pen_is_down(&self) -> bool {
        self.pen.drawing().is_some()
    }

    fn board(&mut self, share: ShareId, epoch: DrawingEpoch) -> &mut InkBoard {
        self.state
            .ink
            .entry(share)
            .or_insert_with(|| InkBoard::new(share, epoch))
    }

    fn send_pen_batches(&mut self) {
        for batch in self.pen.take() {
            let frame = ClientFrame::new(ClientMsg::Ink(batch));
            if let Err(e) = self.conn.send_ink(&frame) {
                debug!(error = %e, "ink not sent");
            }
        }
    }

    pub(super) fn on_ink(&mut self, relay: &InkRelay, now: Instant) {
        let batch = &relay.batch;
        if self.state.me.as_ref() == Some(&relay.author_id) {
            return;
        }
        let Some(epoch) = self
            .state
            .drawing_epoch(batch.share_id)
            .filter(|epoch| *epoch == batch.epoch)
        else {
            return;
        };
        self.board(batch.share_id, epoch)
            .draw(&relay.author_id, batch, now);
    }

    pub(super) fn follow_ink(&mut self) {
        let Some((share, epoch, _)) = self.pen.drawing() else {
            return;
        };
        let watch = self.watch_ui();
        let mut drawable = self.state.drawable_shares(watch.as_ref());
        if !drawable.any(|pair| pair == (share, epoch)) {
            self.pen.abandon();
        }
    }

    pub(super) fn tick_ink(&mut self, now: Instant) {
        self.follow_ink();
        self.pen.flush(now);
        self.send_pen_batches();
        for board in self.state.ink.values_mut() {
            board.prune(now);
        }
        self.state.ink.retain(|_, board| !board.is_empty());
    }

    /// Voice ended: nothing is left to draw on.
    pub(super) fn end_ink(&mut self) {
        self.pen.abandon();
        self.state.ink.clear();
    }
}

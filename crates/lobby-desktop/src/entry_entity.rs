//! `EntryEntity`: the window's view of a running Entry, as `SessionEntity` is of a Session. It
//! holds the handle and the newest view its driver published, and turns each into an
//! `EntryChanged` and one `cx.notify()`. It owns no rule, timer or IO: views act through
//! [`EntryEntity::act`] and read [`EntryEntity::view`]. Dropping it ends the Entry.

use std::sync::Arc;

use gpui_kit::{Context, EventEmitter, Task};
use lobby_client::{EntryHandle, EntryIntent, EntryUpdates, EntryView, Said};

use crate::components::Feedback;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryChanged;

pub struct EntryEntity {
    handle: EntryHandle,
    view: Arc<EntryView>,
    /// The last Intent that asks the Lobby something: until Entry has taken it, the step shows
    /// busy, so the button answers the press at once.
    asked: u64,
    _updates: Task<()>,
}

impl EventEmitter<EntryChanged> for EntryEntity {}

impl EntryEntity {
    pub fn new((handle, mut updates): (EntryHandle, EntryUpdates), cx: &mut Context<Self>) -> Self {
        let view = updates.view();
        // Awaiting the watch needs no reactor, so this runs on the GPUI executor.
        let task = cx.spawn(async move |this, cx| {
            while updates.changed().await {
                let view = updates.view();
                let installed = this.update(cx, |entity, cx| {
                    entity.view = view;
                    cx.emit(EntryChanged);
                    cx.notify();
                });
                if installed.is_err() {
                    break;
                }
            }
        });
        Self {
            handle,
            view,
            asked: 0,
            _updates: task,
        }
    }

    pub fn view(&self) -> &EntryView {
        &self.view
    }

    pub fn act(&mut self, intent: EntryIntent, cx: &mut Context<Self>) {
        let asks = !matches!(intent, EntryIntent::Edited | EntryIntent::WindowActive);
        let n = self.handle.dispatch(intent);
        if asks {
            self.asked = n;
            cx.notify();
        }
    }

    pub fn busy(&self) -> bool {
        self.view.busy || self.view.taken < self.asked
    }

    /// What the step says, in the card's feedback line.
    pub fn said(&self) -> Option<Feedback> {
        self.view.said.as_ref().map(feedback)
    }
}

pub fn feedback(said: &Said) -> Feedback {
    match said {
        Said::Error(m) => Feedback::Error(m.clone().into()),
        Said::Info(m) => Feedback::Info(m.clone().into()),
        Said::Notice(m) => Feedback::Notice(m.clone().into()),
        Said::Hint(m) => Feedback::Hint(m.clone().into()),
    }
}

//! The Linux side of [`super::post`]: toasts go to the desktop's notification daemon through
//! `host::notify`, and a click or a close comes back here, on the UI thread.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use gpui_kit::{App, Context, Global, Task, Window};
use tracing::debug;

use super::OsToast;
use crate::{
    Shell,
    host::notify::{Click, Event, MAX_TRACKED, Notifier, Ticket, Toast, with_activation_token},
};

type OnClick = Rc<dyn Fn(&mut Window, &mut App)>;

/// The toasts on the desktop's list, by tag, with what their click does. It is what "this toast
/// exists" means on the UI side, so a retract that matches none sends nothing to the worker.
///
/// A handler stays until the worker says its post is gone, so the worker says so for every post
/// that does not reach the desktop as well, and the list is bounded all the same: past
/// [`MAX_TRACKED`] the oldest goes.
struct Handlers<H> {
    entries: VecDeque<(Ticket, H)>,
}

impl<H> Default for Handlers<H> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}

impl<H> Handlers<H> {
    /// Files `handler` under the tag of `ticket`, in place of an older post's. Returns what was
    /// let go to make room, oldest first.
    fn insert(&mut self, ticket: Ticket, handler: H) -> Vec<Ticket> {
        self.entries.retain(|(t, _)| t.tag != ticket.tag);
        self.entries.push_back((ticket, handler));
        let mut dropped = Vec::new();
        while self.entries.len() > MAX_TRACKED {
            dropped.extend(self.entries.pop_front().map(|(ticket, _)| ticket));
        }
        dropped
    }

    fn get(&self, tag: &str) -> Option<&H> {
        self.entries
            .iter()
            .find(|(t, _)| t.tag == tag)
            .map(|(_, handler)| handler)
    }

    /// The worker says `ticket`'s post is not on the desktop. Only that post's handler goes: a
    /// newer post under the same tag has its own, which this says nothing about.
    fn gone(&mut self, ticket: &Ticket) {
        self.entries.retain(|(t, _)| t != ticket);
    }

    /// Takes the tag's handler out, and says which post it was.
    fn take(&mut self, tag: &str) -> Option<Ticket> {
        let at = self.entries.iter().position(|(t, _)| t.tag == tag)?;
        self.entries.remove(at).map(|(ticket, _)| ticket)
    }

    fn tags_starting_with(&self, prefixes: &[String]) -> Vec<String> {
        self.entries
            .iter()
            .map(|(t, _)| &t.tag)
            .filter(|tag| prefixes.iter().any(|p| tag.starts_with(p.as_str())))
            .cloned()
            .collect()
    }
}

#[derive(Clone, Default)]
struct Shown(Rc<RefCell<Handlers<OnClick>>>);

struct Desktop {
    /// `None` when the worker has no thread: toasts are lost, as with no daemon.
    notifier: Option<Notifier>,
    shown: Shown,
    /// The generation of the latest post.
    generation: u64,
    _events: Option<Task<()>>,
}

impl Global for Desktop {}

/// Starts the worker on the first toast: a Member who never gets one never connects to the bus.
fn desktop<'a>(cx: &'a mut Context<Shell>) -> &'a mut Desktop {
    if !cx.has_global::<Desktop>() {
        let shown = Shown::default();
        let started = Notifier::start();
        let (notifier, events) = match started {
            Some((notifier, events)) => (Some(notifier), Some(events)),
            None => (None, None),
        };
        let task = events.map(|mut events| {
            let shown = shown.clone();
            cx.spawn(async move |shell, cx| {
                while let Some(event) = events.recv().await {
                    match event {
                        Event::Gone(ticket) => shown.0.borrow_mut().gone(&ticket),
                        Event::Clicked(click) => {
                            let Ok(window) = shell.read_with(cx, |shell, _| shell.window()) else {
                                break;
                            };
                            let shown = shown.clone();
                            let _ = window
                                .update(cx, |_, window, cx| clicked(&shown, click, window, cx));
                        }
                    }
                }
            })
        });
        cx.set_global(Desktop {
            notifier,
            shown,
            generation: 0,
            _events: task,
        });
    }
    cx.global_mut::<Desktop>()
}

/// A click: the toast's own handler runs, with the desktop's activation token readable for the
/// length of it. Most daemons close a clicked notification and say so (`Event::Gone`); one that
/// keeps it may be clicked again.
fn clicked(shown: &Shown, click: Click, window: &mut Window, cx: &mut App) {
    let handler = shown.0.borrow().get(&click.ticket.tag).cloned();
    let Some(handler) = handler else {
        debug!("a click on a notification that is already taken back");
        return;
    };
    debug!(
        tag = click.ticket.tag,
        activation_token = click.token.is_some(),
        "notification clicked"
    );
    with_activation_token(click.token, cx, |cx| handler(window, cx));
}

pub(super) fn post(toast: OsToast, on_click: OnClick, cx: &mut Context<Shell>) {
    let desktop = desktop(cx);
    desktop.generation += 1;
    let generation = desktop.generation;
    let ticket = Ticket {
        tag: toast.tag.clone(),
        generation,
    };
    let let_go = desktop.shown.0.borrow_mut().insert(ticket, on_click);
    let Some(notifier) = &desktop.notifier else {
        return;
    };
    // What is let go loses its handler, so its banner is taken off the desktop too.
    for ticket in let_go {
        notifier.retract(ticket);
    }
    notifier.post(Toast {
        tag: toast.tag,
        generation,
        summary: toast.title,
        body: toast.body,
        kind: toast.kind,
    });
}

pub(super) fn retract(tag: &str, cx: &mut App) {
    let Some(desktop) = cx.try_global::<Desktop>() else {
        return;
    };
    let taken = desktop.shown.0.borrow_mut().take(tag);
    if let Some(ticket) = taken
        && let Some(notifier) = &desktop.notifier
    {
        notifier.retract(ticket);
    }
}

pub(super) fn retract_prefixes(prefixes: &[String], cx: &mut App) {
    let Some(desktop) = cx.try_global::<Desktop>() else {
        return;
    };
    let tags = desktop.shown.0.borrow().tags_starting_with(prefixes);
    for tag in tags {
        retract(&tag, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(tag: &str, generation: u64) -> Ticket {
        Ticket {
            tag: tag.into(),
            generation,
        }
    }

    #[test]
    fn a_late_close_of_an_older_post_leaves_the_newer_posts_handler() {
        let mut handlers = Handlers::default();
        handlers.insert(ticket("m/alice", 1), "first");
        // Alice writes again before the UI has heard that the first one expired.
        handlers.insert(ticket("m/alice", 2), "second");
        handlers.gone(&ticket("m/alice", 1));
        assert_eq!(handlers.get("m/alice"), Some(&"second"));

        // The close of the post that is on the list does take its handler.
        handlers.gone(&ticket("m/alice", 2));
        assert_eq!(handlers.get("m/alice"), None);
    }

    #[test]
    fn handlers_of_posts_that_never_arrive_do_not_pile_up() {
        let mut handlers = Handlers::default();
        let mut let_go = Vec::new();
        // No daemon: a unique mention each time, and the worker never says anything.
        for i in 0..MAX_TRACKED * 3 {
            let_go.extend(handlers.insert(ticket(&format!("mention/{i}"), 1 + i as u64), i));
        }
        assert_eq!(handlers.entries.len(), MAX_TRACKED);
        assert_eq!(let_go.len(), MAX_TRACKED * 2);
        assert_eq!(let_go[0], ticket("mention/0", 1), "the oldest goes first");
        assert_eq!(handlers.get("mention/0"), None);
        let newest = format!("mention/{}", MAX_TRACKED * 3 - 1);
        assert!(handlers.get(&newest).is_some(), "the newest stays");
    }

    #[test]
    fn a_retract_names_the_post_it_takes_back() {
        let mut handlers = Handlers::default();
        handlers.insert(ticket("m/alice", 4), ());
        handlers.insert(ticket("r/1", 5), ());
        assert_eq!(handlers.tags_starting_with(&["r/".to_owned()]), ["r/1"]);
        assert_eq!(handlers.take("m/alice"), Some(ticket("m/alice", 4)));
        assert_eq!(handlers.take("m/alice"), None);
    }
}

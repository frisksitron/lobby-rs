//! Toasts in the operating system's own notification center, for what happens while the window
//! cannot be seen. They go through gpui-kit's system delivery: one tag per thing, so a repeat
//! replaces the toast before it, and a click comes back as a call. A driven or tested app records
//! them instead (the [`OsToastRecorder`] global): the person's Action Center is never touched.

use std::{cell::RefCell, rc::Rc};

use gpui_kit::{
    App, Context, Global, Window,
    component::{WindowExt as _, notification::Notification},
};
use lobby_client::{
    LobbyOrigin, copy,
    message_text::MessageText,
    os_toast::{Moment, message_body, should_os_toast},
};
use lobby_protocol::{AccessRequest, AccessRequestId, Message};

use crate::Shell;

/// Marks the toasts this module posts, so their tags never meet another kind's.
struct OsToastTag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsToast {
    /// What a repeat replaces.
    pub tag: String,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Default)]
pub struct OsToastRecorder(Rc<RefCell<Vec<OsToast>>>);

impl Global for OsToastRecorder {}

/// A Message that arrived while the person may not be looking: a toast per author and Lobby, so
/// a burst replaces rather than stacks. Its click brings the window back and shows the latest.
pub(crate) fn message_arrived(
    shell: &Shell,
    message: &Message,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    let (Some(session), Some(view), Some(origin)) =
        (shell.session(), shell.lobby_view(), shell.current_origin())
    else {
        return;
    };
    let session = session.read(cx);
    let mine = session
        .state()
        .and_then(|s| s.me.as_ref())
        .is_some_and(|me| *me == message.author.id);
    let mentioned = session.state().is_some_and(|s| {
        s.my_username()
            .is_some_and(|me| MessageText::of_in(&message.content, &s.mention_names()).mentions(me))
    });
    let moment = Moment {
        mine,
        window_active: window.is_window_active(),
        hidden_to_tray: shell.hidden_to_tray(),
        dnd: session.dnd(),
        center_open: view.read(cx).center_open,
        mentioned,
        feed_hidden: shell.settings.is_some() || view.read(cx).viewing.is_fullscreen(),
    };
    if !should_os_toast(moment) {
        return;
    }
    let many = shell.lobbies.read(cx).list().len() > 1;
    let lobby = view.read(cx).lobby_name(cx);
    let lobby = many.then_some(lobby.as_str()).filter(|n| !n.is_empty());
    let author = &message.author.username;
    // A mention keeps its own toast, so the author's next Message does not replace it.
    let toast = if mentioned {
        OsToast {
            tag: format!("mention/{origin}/{}", message.id.get()),
            title: copy::mention_toast_title(author, lobby),
            body: message_body(message),
        }
    } else {
        OsToast {
            tag: format!("message/{origin}/{}", message.author.id.as_str()),
            title: copy::message_toast_title(author, lobby),
            body: message_body(message),
        }
    };
    let (shell, view, origin) = (cx.weak_entity(), view.downgrade(), origin.clone());
    post(
        toast,
        move |_, cx| {
            let _ = shell.update(cx, |shell, cx| {
                shell.show_window(cx);
                if shell.current_origin() == Some(&origin) {
                    let _ = view.update(cx, |v, cx| v.jump_to_latest(cx));
                }
            });
        },
        window,
        cx,
    );
}

/// A new Ask to Join while the person may not be looking. Its click brings the window back and
/// opens the Center, where Vouch and Decline are.
pub(crate) fn access_requested(
    shell: &Shell,
    request: &AccessRequest,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    let (Some(session), Some(view), Some(origin)) =
        (shell.session(), shell.lobby_view(), shell.current_origin())
    else {
        return;
    };
    let moment = Moment {
        mine: false,
        window_active: window.is_window_active(),
        hidden_to_tray: shell.hidden_to_tray(),
        dnd: session.read(cx).dnd(),
        center_open: view.read(cx).center_open,
        mentioned: false,
        feed_hidden: false,
    };
    if !should_os_toast(moment) {
        return;
    }
    let toast = OsToast {
        tag: decision_tag(origin, &request.id),
        title: copy::asks_to_join(request.username.as_str()),
        body: request.requester_label.clone(),
    };
    let (shell, view) = (cx.weak_entity(), view.downgrade());
    post(
        toast,
        move |window, cx| {
            let _ = shell.update(cx, |shell, cx| shell.show_window(cx));
            let _ = view.update(cx, |v, cx| v.set_center_open(true, window, cx));
        },
        window,
        cx,
    );
}

/// The request is handled: its toast has nothing left to ask.
pub(crate) fn access_request_gone(
    shell: &Shell,
    id: &AccessRequestId,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(origin) = shell.current_origin() {
        retract(&decision_tag(origin, id), window, cx);
    }
}

fn decision_tag(origin: &LobbyOrigin, id: &AccessRequestId) -> String {
    format!("decision/{origin}/{}", id.as_str())
}

/// Posts the toast; `on_click` runs when the person clicks it.
pub fn post(
    toast: OsToast,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(recorder) = cx.try_global::<OsToastRecorder>() {
        let mut shown = recorder.0.borrow_mut();
        shown.retain(|t| t.tag != toast.tag);
        shown.push(toast);
        return;
    }
    window.push_notification(
        Notification::new()
            .id1::<OsToastTag>(toast.tag)
            .title(toast.title)
            .message(toast.body)
            .system()
            .on_click(move |_, window, cx| on_click(window, cx)),
        cx,
    );
}

/// Takes the toast away: what it announced is gone.
pub fn retract(tag: &str, window: &mut Window, cx: &mut App) {
    if let Some(recorder) = cx.try_global::<OsToastRecorder>() {
        recorder.0.borrow_mut().retain(|t| t.tag != tag);
        return;
    }
    window.remove_notification1::<OsToastTag>(tag.to_owned(), cx);
}

pub fn recorded(cx: &App) -> Vec<OsToast> {
    cx.try_global::<OsToastRecorder>()
        .map(|r| r.0.borrow().clone())
        .unwrap_or_default()
}

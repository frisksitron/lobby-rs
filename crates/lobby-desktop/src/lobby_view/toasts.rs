//! The toasts of notifications, in the Notification Center's one layout: the title in full, the
//! detail on hover (an Ask to Join's email always), the actions under them, a click opening the
//! Center.

use gpui_kit::{
    AnyElement, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window, div,
};
use lobby_client::{Issue, Severity, Source, copy};
use lobby_protocol::AccessRequest;

use super::{LobbyView, SidebarLayout};
use crate::components::{
    ActionContext, ConnectionNotice, Decide, Detail, Lead, NotificationEntry, action_button,
    notification_icon, row_action,
};
use crate::kit::{WindowExt as _, notification::Notification};
use crate::ui;

/// Toasts of Issues, keyed by kind (a repeat replaces its toast).
pub(super) struct IssueToast;

/// The toast of a connection state the person must act on: one at a time, so a new state replaces
/// it.
pub(super) struct ConnectionToast;

/// The toast of an Ask to Join, keyed by request id: it stays until the request is handled.
pub(super) struct DecisionToast;

const CONNECTION_TOAST: &str = "connection";

/// A toast's frame. The kit clips its content at the padding, so half the bottom padding is
/// [`toast_body`]'s: room under a button for its lip and its focus ring.
fn toast() -> Notification {
    Notification::new().pt_2().pb_1().px_3()
}

/// The toast of an Issue. Even one with an action (Rejoin, Send again) goes away by itself after
/// a few seconds, so nothing sits on the member panel for a whole outage.
pub(crate) fn issue_toast(issue: &Issue, lobby: WeakEntity<LobbyView>) -> Notification {
    let key = issue.kind.key();
    let spec = ToastSpec {
        code: key.into(),
        who: Who::Icon(issue.severity(), issue.source()),
        title: copy::issue_title(issue.kind).into(),
        detail: issue.message.to_string().into(),
    };
    let open = open_center(lobby.clone());
    let issue = issue.clone();
    toast()
        .id1::<IssueToast>(key)
        .content(move |_, window, cx| {
            let action = issue_action(&issue, &lobby, cx);
            toast_body(&spec, action.into_iter().collect(), window, cx)
        })
        .on_click(open)
}

/// The toast of the connection entry, with its one action (Try now, Reconnect or Change Lobby).
pub(crate) fn connection_toast(
    notice: &ConnectionNotice,
    lobby: WeakEntity<LobbyView>,
) -> Notification {
    let spec = ToastSpec {
        code: CONNECTION_TOAST.into(),
        who: Who::Icon(notice.tone.severity(), Source::Connection),
        title: notice.title.clone(),
        detail: notice.detail.clone(),
    };
    let action = notice.action.or(notice.secondary);
    let open = open_center(lobby.clone());
    toast()
        .id1::<ConnectionToast>(CONNECTION_TOAST)
        .content(move |_, window, cx| {
            let button = action.map(|a| {
                let lobby = lobby.clone();
                ui::Button::new(SharedString::from(format!("toast-{}", a.id())))
                    .variant(ui::Variant::Primary)
                    .size(ui::Size::Word)
                    .label(a.label())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = lobby.update(cx, |v, cx| v.connection_action(a, window, cx));
                    })
            });
            toast_body(&spec, button.into_iter().collect(), window, cx)
        })
        .on_click(open)
}

/// The toast of an Ask to Join, with Vouch and Decline. It does not autohide: the Member decides,
/// or opens the Center, or another Member handles it first.
pub(crate) fn decision_toast(
    request: &AccessRequest,
    lobby: WeakEntity<LobbyView>,
) -> Notification {
    let key = request.id.as_str().to_owned();
    let spec = ToastSpec {
        code: format!("decision-{}", request.username).into(),
        who: Who::Member(request.username.to_string().into()),
        title: copy::asks_to_join(request.username.as_str()).into(),
        detail: request.requester_label.clone().into(),
    };
    let (id, name) = (request.id.clone(), request.username.to_string());
    let open = open_center(lobby.clone());
    toast()
        .id1::<DecisionToast>(key)
        .autohide(false)
        .content(move |_, window, cx| {
            let decide = |what: Decide, button: ui::Button| {
                let (lobby, id) = (lobby.clone(), id.clone());
                button.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let _ = lobby.update(cx, |v, cx| v.decide(id.clone(), what, window, cx));
                })
            };
            let buttons = vec![
                decide(
                    Decide::Vouch,
                    ui::Button::new(SharedString::from(format!("toast-vouch-{name}")))
                        .variant(ui::Variant::Primary)
                        .size(ui::Size::Word)
                        .label("Vouch"),
                ),
                decide(
                    Decide::Decline,
                    ui::Button::new(SharedString::from(format!("toast-decline-{name}")))
                        .variant(ui::Variant::Outline)
                        .size(ui::Size::Word)
                        .label("Decline"),
                ),
            ];
            toast_body(&spec, buttons, window, cx)
        })
        .on_click(open)
}

/// A click on a toast's body opens the Center, once the toast's click handler has returned:
/// gpui-component holds the toast while its handler runs, and opening the Center closes it.
fn open_center(
    lobby: WeakEntity<LobbyView>,
) -> impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    move |_, window, cx| {
        let lobby = lobby.clone();
        window.defer(cx, move |window, cx| {
            let _ = lobby.update(cx, |v, cx| v.set_center_open(true, window, cx));
        });
    }
}

/// Takes the connection toast down, once the Lobby is calm or the state changed.
pub(super) fn remove_connection_toast(window: &mut Window, cx: &mut gpui_kit::App) {
    window.remove_notification1::<ConnectionToast>(CONNECTION_TOAST, cx);
}

fn issue_action(
    issue: &Issue,
    lobby: &WeakEntity<LobbyView>,
    cx: &mut gpui_kit::App,
) -> Option<ui::Button> {
    let ctx = lobby
        .upgrade()
        .map(|v| v.read(cx).action_context(cx))
        .unwrap_or(ActionContext {
            connected: false,
            can_resend: false,
        });
    let action = issue.kind.action()?;
    let button = action_button(
        format!("toast-action-{}", issue.kind.key()),
        row_action(action, ctx),
    );
    Some(button.on_click({
        let (lobby, issue) = (lobby.clone(), issue.clone());
        move |_, window, cx| {
            cx.stop_propagation();
            let _ = lobby.update(cx, |v, cx| v.notification_action(&issue, window, cx));
        }
    }))
}

/// The toasts' right margin in real pixels (the theme's own unit) at `scale`: beside the member
/// sheet, their right edge on the composer's.
pub fn toast_right_margin(layout: SidebarLayout, scale: f32) -> gpui_kit::Pixels {
    gpui_kit::px((layout.taken() + 16.) * scale) // px: theme unit
}

/// What leads a toast: a Member for a person, an icon disc for everything else.
enum Who {
    Member(SharedString),
    Icon(Severity, Source),
}

struct ToastSpec {
    code: SharedString,
    who: Who,
    title: SharedString,
    detail: SharedString,
}

fn toast_body(
    spec: &ToastSpec,
    actions: Vec<ui::Button>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> AnyElement {
    let ToastSpec {
        ref code,
        ref who,
        ref title,
        ref detail,
    } = *spec;
    // Hover is state, not a hover style: an element must not appear or vanish (`display`) in a
    // style that only applies at paint time.
    let hovered = window.use_keyed_state(
        SharedString::from(format!("toast-hover-{code}")),
        cx,
        |_, _| false,
    );
    let expanded = *hovered.read(cx);
    let (lead, detail) = match who {
        // Who asks is the point, so the email is always there, on one line.
        Who::Member(name) => (
            Lead::Member(name.clone()),
            Some(Detail::Line(detail.clone())),
        ),
        Who::Icon(severity, source) => (
            Lead::Icon(notification_icon(*severity, *source, cx)),
            expanded.then(|| Detail::Wrapped(detail.clone())),
        ),
    };
    let entry = actions.into_iter().fold(
        NotificationEntry::new(code.clone(), lead, title.clone())
            .beside_close_button()
            .detail(detail),
        NotificationEntry::action,
    );
    div()
        .id(SharedString::from(format!("toast-{code}")))
        .on_hover(move |over, _, cx| {
            hovered.update(cx, |h, cx| {
                if *h != *over {
                    *h = *over;
                    cx.notify();
                }
            });
        })
        .w_full()
        .pb_1()
        .child(entry)
        .test_support()
        .into_any_element()
}

use std::rc::Rc;

use gpui_kit::{AnyElement, App, Context, ParentElement as _, Styled as _, Window, div, px};
use lobby_client::{Issue, IssueKind, Phase};
use lobby_protocol::{AccessRequestId, UnixMs};

use crate::components::{
    Callback, ConnectionAction, ConnectionNotice, DecisionRow, NotificationRow, Settled,
    notification_center, row_action,
};
use crate::gallery::{Gallery, Section, fixtures::notice};
use std::time::Duration;

use super::title_bar::bell;
use super::{caption, popover_frame, wrap};

pub(super) fn request(n: u8, name: &str, ago: &str) -> DecisionRow {
    DecisionRow {
        id: format!("acr_{n:024x}")
            .parse::<AccessRequestId>()
            .expect("an id"),
        username: name.to_owned().into(),
        label: format!("{name}@example.com").into(),
        created_at: UnixMs(0),
        ago: ago.to_owned().into(),
        when: "Sat 24 Oct, 21:40".into(),
        deciding: false,
        settled: None,
        folding: false,
    }
}

pub(super) fn notification(
    kind: IssueKind,
    title: &str,
    message: &str,
    connected: bool,
) -> NotificationRow {
    let ctx = crate::components::ActionContext {
        connected,
        can_resend: true,
    };
    NotificationRow {
        issue: Issue::new(kind, message.to_owned()),
        title: title.to_owned().into(),
        message: message.to_owned().into(),
        time: "21:40".into(),
        unseen: true,
        action: kind.action().map(|a| row_action(a, ctx)),
    }
}

pub(super) fn center(
    rows: &[NotificationRow],
    decisions: &[DecisionRow],
    connection: Option<&ConnectionNotice>,
    g: &Gallery,
    cx: &App,
) -> AnyElement {
    let dismiss: Callback<Option<IssueKind>> = Rc::new(|_, _, _| {});
    let act: Callback<Issue> = Rc::new(|_, _, _| {});
    let decide: notification_center::DecideHandler = Rc::new(
        |_: AccessRequestId, _: notification_center::Decide, _: &mut Window, _: &mut App| {},
    );
    let connect: Callback<ConnectionAction> = Rc::new(|_, _, _| {});
    popover_frame(
        div().w(px(356.)).child(notification_center::render_list(
            notification_center::Contents {
                connection: connection.map(|n| (n, &connect)),
                decisions,
                rows,
            },
            &dismiss,
            &act,
            Some(&decide),
            notification_center::Viewport {
                max_h: 760.,
                scroll: &g.held.center_scroll,
                heights: &g.held.center_heights,
            },
            cx,
        )),
        cx,
    )
}

pub(super) fn notifications(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let rows = vec![
        notification(
            IssueKind::VoiceEnded,
            "Voice ended",
            "The connection to the Lobby dropped.",
            true,
        ),
        notification(
            IssueKind::SendFailed,
            "Message not sent",
            "It is back in the composer.",
            true,
        ),
        NotificationRow {
            unseen: false,
            ..notification(
                IssueKind::StateWriteFailed,
                "Settings not saved",
                "The Lobbies on this device could not be written.",
                true,
            )
        },
    ];
    // Issues are keyed by kind, so one Center never lists a kind twice: the variant whose Rejoin
    // waits for the Lobby gets a Center of its own.
    let reconnecting = notice(
        Phase::Reconnecting {
            attempt: 1,
            max: 10,
            retry_in: Duration::from_secs(4),
        },
        true,
        None,
    );
    let offline = [notification(
        IssueKind::VoiceEnded,
        "Voice ended",
        "Rejoin waits for the Lobby.",
        false,
    )];
    let decisions = vec![
        request(1, "carol", "5 min ago"),
        DecisionRow {
            deciding: true,
            ..request(2, "dave", "1 h ago")
        },
        DecisionRow {
            settled: Some(Settled::VouchedIn),
            ..request(3, "erin", "2 h ago")
        },
        DecisionRow {
            settled: Some(Settled::Declining),
            ..request(4, "mallory", "1 d ago")
        },
    ];
    vec![
        Section::new(
            "bell",
            "The bell",
            wrap([
                caption("nothing new", bell(0, Vec::new(), None), cx),
                caption("unseen", bell(3, Vec::new(), None), cx),
                caption(
                    "waiting to join",
                    bell(0, decisions[..2].to_vec(), None),
                    cx,
                ),
                caption(
                    "Lobby not calm",
                    bell(0, Vec::new(), Some(reconnecting.clone())),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "center",
            "The Center: the connection while it is not calm, requests to join (waiting, vouching, vouched, declined) and notifications",
            center(&rows, &decisions, Some(&reconnecting), g, cx),
        ),
        Section::new(
            "center-offline",
            "Rejoin waits while the Lobby is not reached",
            center(&offline, &[], None, g, cx),
        ),
        Section::new(
            "center-empty",
            "All caught up",
            center(&[], &[], None, g, cx),
        ),
    ]
}

/// The connection entry in each phase it shows, written by the app's own function.
pub(super) fn connection(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let retry_in = Duration::from_secs(4);
    let reconnecting = Phase::Reconnecting {
        attempt: 1,
        max: 10,
        retry_in,
    };
    let phases = [
        (
            "Never reached, waiting",
            notice(reconnecting.clone(), false, None),
        ),
        (
            "Never reached, dialing",
            notice(Phase::Connecting { attempt: 1 }, false, None),
        ),
        ("Reconnecting", notice(reconnecting, true, None)),
        (
            "Reconnecting, dialing",
            notice(Phase::Connecting { attempt: 1 }, true, None),
        ),
        ("Offline", notice(Phase::Offline { retry_in }, true, None)),
        (
            "Refused by the Lobby",
            notice(Phase::Unavailable { attempts: 0 }, true, None),
        ),
        (
            "Unavailable",
            notice(Phase::Unavailable { attempts: 10 }, true, Some("21:40")),
        ),
        (
            "Signed in elsewhere",
            notice(Phase::SessionReplaced, true, None),
        ),
        (
            "Update required",
            notice(Phase::ProtocolMismatch, true, None),
        ),
    ];
    vec![Section::new(
        "phases",
        "Each phase that is not calm. A calm Lobby (connected, or back online) has no entry, and the bar's tint clears. Those with a toast: Offline, Refused, Unavailable, Signed in elsewhere, Update required.",
        wrap(phases.map(|(name, n)| caption(name, center(&[], &[], Some(&n), g, cx), cx))),
    )]
}

pub(super) fn toasts(g: &mut Gallery, _: &mut Window, _: &mut Context<Gallery>) -> Vec<Section> {
    vec![Section::new(
        "toasts",
        "Voice ended (Rejoin waits: no Lobby here), a send failed, an upload failed, a device notice, then the two connection toasts (Offline, Unavailable). Hover one for the whole message.",
        div().flex().flex_col().gap_3().children(
            g.held
                .toasts
                .iter()
                .map(|t| div().w(px(360.)).child(t.clone())),
        ),
    )]
}

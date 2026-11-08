//! The member list without headings: who is In Voice in the purple box at the top, then everyone
//! else online, then offline (dimmed). Rows keep a gap between them that the box grows into, so the
//! box never moves or covers another row. The box is the app's own [`voice_box`].

use gpui_kit::{
    App, Context, Div, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    component::{
        ActiveTheme as _, Sizable as _,
        button::{Button, ButtonVariants as _},
    },
    div, px,
};
use lobby_protocol::Presence;

use crate::components::{MEMBER_ROW_GAP, MemberRowModel, card_surface, voice_box};
use crate::gallery::{
    Gallery, Section,
    fixtures::{member, muted, speaking, voice},
};

use super::{caption, members::row, panel, wrap};

struct ListState {
    name: &'static str,
    known: bool,
    /// Online in order, each with whether it sits in the Voice box.
    online: Vec<(MemberRowModel, bool)>,
    offline: Vec<MemberRowModel>,
}

fn list(state: ListState, cx: &App) -> Div {
    let sidebar = cx.theme().sidebar;
    let inside = card_surface(cx);
    let mut items = Vec::new();
    let (boxed, rest): (Vec<_>, Vec<_>) = state.online.into_iter().partition(|(_, b)| *b);
    if !boxed.is_empty() {
        let rows = boxed
            .into_iter()
            .map(|(m, _)| row(m, state.known, true, inside))
            .collect();
        items.push(voice_box(rows, inside));
    }
    items.extend(
        rest.into_iter()
            .map(|(m, _)| row(m, state.known, false, sidebar)),
    );
    items.extend(
        state
            .offline
            .into_iter()
            .map(|m| row(m, state.known, false, sidebar)),
    );
    panel(220., sidebar, cx)
        .gap(px(MEMBER_ROW_GAP))
        .p_2()
        .children(items)
}

fn states() -> Vec<ListState> {
    let me = |v: Option<lobby_protocol::VoiceState>| MemberRowModel {
        is_me: true,
        voice: v,
        ..member("bob")
    };
    let in_voice = |name: &str, v| MemberRowModel {
        voice: Some(v),
        ..member(name)
    };
    let dave = MemberRowModel {
        presence: Presence::Idle,
        ..member("dave")
    };
    let offline = || {
        vec![MemberRowModel {
            presence: Presence::Offline,
            ..member("frank")
        }]
    };
    let joined = || {
        vec![
            (me(Some(voice())), true),
            (in_voice("alice", speaking()), true),
            (in_voice("carol", muted()), true),
            (dave.clone(), false),
        ]
    };
    vec![
        ListState {
            name: "Nobody In Voice",
            known: true,
            online: vec![
                (me(None), false),
                (member("alice"), false),
                (member("carol"), false),
                (dave.clone(), false),
            ],
            offline: offline(),
        },
        ListState {
            name: "alice and carol In Voice",
            known: true,
            online: vec![
                (in_voice("alice", speaking()), true),
                (in_voice("carol", muted()), true),
                (me(None), false),
                (dave.clone(), false),
            ],
            offline: offline(),
        },
        ListState {
            name: "You join",
            known: true,
            online: joined(),
            offline: offline(),
        },
        ListState {
            name: "Only you In Voice",
            known: true,
            online: vec![
                (me(Some(voice())), true),
                (member("alice"), false),
                (member("carol"), false),
                (dave.clone(), false),
            ],
            offline: offline(),
        },
        ListState {
            name: "Reconnecting",
            known: false,
            online: joined(),
            offline: offline(),
        },
    ]
}

fn sequence(cx: &App) -> Div {
    wrap(states().into_iter().map(|s| {
        let name = s.name;
        caption(name, list(s, cx), cx)
    }))
}

const PLAYERS: [&str; 4] = ["bob", "alice", "carol", "dave"];

/// The playground's list: whoever is In Voice (you first), then the rest (you first), each in a
/// fixed order.
fn playground_state(in_voice: [bool; 4]) -> ListState {
    let model = |i: usize| {
        let v = in_voice[i].then(|| if i == 1 { speaking() } else { voice() });
        MemberRowModel {
            is_me: i == 0,
            voice: v,
            presence: if i == 3 {
                Presence::Idle
            } else {
                Presence::Online
            },
            ..member(PLAYERS[i])
        }
    };
    let (boxed, rest): (Vec<_>, Vec<_>) = (0..4).partition(|&i| in_voice[i]);
    let online = boxed
        .into_iter()
        .map(|i| (model(i), true))
        .chain(rest.into_iter().map(|i| (model(i), false)))
        .collect();
    ListState {
        name: "playground",
        known: true,
        online,
        offline: vec![MemberRowModel {
            presence: Presence::Offline,
            ..member("frank")
        }],
    }
}

fn playground(g: &Gallery, cx: &mut Context<Gallery>) -> Div {
    let toggles = PLAYERS.iter().enumerate().map(|(i, name)| {
        let joined = g.held.in_voice[i];
        let label = match (i, joined) {
            (0, false) => "You join".to_owned(),
            (0, true) => "You leave".to_owned(),
            (_, false) => format!("{name} joins"),
            (_, true) => format!("{name} leaves"),
        };
        let button = Button::new(SharedString::from(format!("play-{name}")))
            .small()
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.held.in_voice[i] = !this.held.in_voice[i];
                cx.notify();
            }));
        if joined {
            button.primary()
        } else {
            button.outline()
        }
        .into_any_element()
    });
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().flex().gap_2().children(toggles))
        .child(caption(
            "Playground",
            list(playground_state(g.held.in_voice), cx),
            cx,
        ))
}

pub(super) fn member_list(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    vec![
        Section::new(
            "playground",
            "Join and leave to watch the box grow and shrink",
            playground(g, cx),
        ),
        Section::new(
            "states",
            "In Voice first, in the purple box; then everyone online, then offline",
            sequence(cx),
        ),
    ]
}

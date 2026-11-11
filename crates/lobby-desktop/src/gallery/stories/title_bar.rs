use std::rc::Rc;

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, assets::IconName,
    div, px,
};
use lobby_client::{HttpPolicy, LobbyOrigin};

use crate::components::{
    BarButton, ConnectionNotice, DecisionRow, LobbySwitcher, NotificationBell, SwitcherCurrent,
    SwitcherEntry, UpdatePill, mark_color,
};
use crate::gallery::{Gallery, Section};
use crate::kit::ActiveTheme as _;
use crate::ui;
use lobby_client::update::{UpdateStatus, Version};

use super::{bar, caption, label, wrap};

pub(super) fn dev_origin() -> LobbyOrigin {
    LobbyOrigin::parse("https://dev.lobby.example", HttpPolicy::LoopbackOnly).expect("an origin")
}

pub(super) fn current() -> SwitcherCurrent {
    let origin = dev_origin();
    SwitcherCurrent {
        name: "Dev Lobby".into(),
        icon: None,
        color: mark_color(origin.as_str()),
        origin,
    }
}

pub(super) fn bell(
    unseen: usize,
    decisions: Vec<DecisionRow>,
    connection: Option<ConnectionNotice>,
) -> NotificationBell {
    NotificationBell::new(
        Vec::new(),
        unseen,
        Rc::new(|_, _, _| {}),
        false,
        Rc::new(|_, _, _| {}),
        Rc::new(|_, _, _| {}),
    )
    .decisions(decisions, Rc::new(|_, _, _, _| {}))
    .connection(connection, Rc::new(|_, _, _| {}))
}

pub(super) fn members_toggle() -> AnyElement {
    BarButton::slot(
        ui::Button::new("members-toggle")
            .size(ui::Size::Bar)
            .icon(IconName::Users)
            .label("Members"),
        None,
    )
    .into_any_element()
}

fn bell_button(id: &'static str) -> ui::Button {
    ui::Button::new(id).size(ui::Size::Bar).icon(IconName::Bell)
}

pub(super) fn bar_button(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let theme = cx.theme();
    let (danger, warning) = (theme.danger, theme.warning);
    let switcher = || LobbySwitcher::new(Some(current()), Vec::new(), Rc::new(|_, _, _| {}));
    vec![
        Section::new(
            "kinds",
            "Icon, labelled, selected",
            wrap([
                caption("icon", BarButton::slot(bell_button("b-icon").tooltip("Notifications"), None), cx),
                caption("labelled", members_toggle(), cx),
                caption(
                    "selected",
                    BarButton::slot(
                        ui::Button::new("b-selected")
                            .size(ui::Size::Bar)
                            .icon(IconName::Users)
                            .label("Members")
                            .selected(true),
                        None,
                    ),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "badges",
            "Badges: a plain dot, never a number",
            wrap([
                caption("none", BarButton::slot(bell_button("b-none"), None), cx),
                caption("something went wrong", BarButton::slot(bell_button("b-danger"), Some(danger)), cx),
                caption("not calm", BarButton::slot(bell_button("b-warning"), Some(warning)), cx),
            ]),
        ),
        Section::new(
            "in-the-bar",
            "In the title bar: the switcher, the bell, the members toggle",
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(bar(
                    [
                        switcher().into_any_element(),
                        div().flex_1().into_any_element(),
                        bell(2, Vec::new(), None).into_any_element(),
                        members_toggle(),
                    ],
                    cx,
                ))
                .child(label("While the window closes the Lobby (quitting, signing out), the mark and name stay put:", cx))
                .child(bar([LobbySwitcher::still(Some(current())).into_any_element()], cx))
                .child(label("With an update ready to run:", cx))
                .child(bar(
                    [
                        switcher().into_any_element(),
                        div().flex_1().into_any_element(),
                        UpdatePill::new(
                            UpdateStatus::Ready {
                                version: Version { major: 0, minor: 1, patch: 2 },
                                file: "LobbySetup.exe".into(),
                            },
                            true,
                            Rc::new(|_| {}),
                            Rc::new(|_| {}),
                        )
                        .into_any_element(),
                        bell(2, Vec::new(), None).into_any_element(),
                        members_toggle(),
                    ],
                    cx,
                )),
        ),
    ]
}

pub(super) fn switcher(_: &mut Gallery, _: &mut Window, cx: &mut Context<Gallery>) -> Vec<Section> {
    let entry = |origin: &str, name: &str, current, signed_in| {
        let origin = LobbyOrigin::parse(origin, HttpPolicy::LoopbackOnly).expect("an origin");
        SwitcherEntry {
            color: mark_color(origin.as_str()),
            origin,
            name: name.to_owned().into(),
            icon: None,
            current,
            last_used: false,
            signed_in,
        }
    };
    let entries = vec![
        entry("https://dev.lobby.example", "Dev Lobby", true, true),
        entry(
            "https://saturday.example",
            "The Saturday Night Board Game and Pizza Society",
            false,
            true,
        ),
        entry("http://localhost:8080", "Local", false, false),
    ];
    vec![
        Section::new(
            "in-a-lobby",
            "In a Lobby (click it for the menu)",
            bar(
                [
                    LobbySwitcher::new(Some(current()), entries.clone(), Rc::new(|_, _, _| {}))
                        .into_any_element(),
                ],
                cx,
            ),
        ),
        Section::new(
            "long-name",
            "A long name truncates",
            div().w(px(260.)).child(bar(
                [LobbySwitcher::new(
                    Some(SwitcherCurrent {
                        name: "The Saturday Night Board Game and Pizza Society".into(),
                        ..current()
                    }),
                    entries.clone(),
                    Rc::new(|_, _, _| {}),
                )
                .into_any_element()],
                cx,
            )),
        ),
        Section::new(
            "url-step",
            "On the URL step: the app's mark",
            bar(
                [LobbySwitcher::new(None, entries, Rc::new(|_, _, _| {})).into_any_element()],
                cx,
            ),
        ),
        Section::new(
            "still",
            "Still, while the window closes the Lobby",
            bar(
                [LobbySwitcher::still(Some(current())).into_any_element()],
                cx,
            ),
        ),
    ]
}

//! The Lobby view's title bar: the Lobby and its switcher, the Members button, the Notification
//! Center's bell and the cog.

use std::rc::Rc;

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, assets::IconName, div};
use lobby_client::{Issue, LobbyState};

use super::{LobbyView, LobbyViewEvent, SidebarLayout, forward};
use crate::components::{
    AppTitleBar, BarButton, LobbySwitcher, NotificationBell, bar_tint, connection_notice,
    notification_rows,
};
use crate::kit::WindowExt as _;
use crate::lobby_icons::icons_of;
use crate::ui;

impl LobbyView {
    /// The Lobby and its switcher, the folded sidebar's button and the Notification Center's bell,
    /// on a bar tinted by the connection.
    pub(super) fn title_bar(
        &mut self,
        state: &LobbyState,
        layout: SidebarLayout,
        window: &gpui_kit::Window,
        cx: &Context<Self>,
    ) -> AppTitleBar {
        let origin = self.session.read(cx).origin().cloned();
        let lobby_name: SharedString = self.lobby_name(cx).into();
        let (switcher_entries, switcher_current) = {
            let lobbies = self.lobbies.read(cx);
            let current = origin
                .as_ref()
                .map(|o| lobbies.switcher_current(o, Some(&lobby_name)));
            (lobbies.switcher_entries(origin.as_ref()), current)
        };
        self.bar_lobby = switcher_current.clone();
        let notice = connection_notice(state, &lobby_name, self.last_connected.as_deref());
        if state.phase == lobby_client::Phase::ProtocolMismatch {
            crate::updates::AppUpdates::lobby_is_newer(cx);
        }
        let tint = notice.as_ref().map(|n| bar_tint(n.tone, cx));
        let tone = notice.as_ref().map(|n| n.tone);
        let ctx = self.action_context(cx);
        let rows = notification_rows(self.issues.read(cx), ctx);
        let unseen = self.issues.read(cx).unseen();
        let bell = {
            let (center, lobby) = (self.issues.clone(), cx.weak_entity());
            NotificationBell::new(
                rows,
                unseen,
                Rc::new(move |kind, window, cx| match kind {
                    Some(kind) => {
                        let _ = lobby.update(cx, |v, cx| v.dismiss(kind, window, cx));
                    }
                    None => {
                        center.update(cx, |c, cx| c.clear(cx));
                        window.clear_notifications(cx);
                    }
                }),
                self.center_open,
                forward(cx, |v: &mut Self, open, window, cx| {
                    v.set_center_open(open, window, cx)
                }),
                forward(cx, |v: &mut Self, issue: Issue, window, cx| {
                    v.notification_action(&issue, window, cx)
                }),
            )
            .decisions(self.decisions(cx), {
                let lobby = cx.weak_entity();
                Rc::new(move |id, what, window, cx| {
                    let _ = lobby.update(cx, |v, cx| v.decide(id, what, window, cx));
                })
            })
            .connection(
                notice,
                forward(cx, |v: &mut Self, action, window, cx| {
                    v.connection_action(action, window, cx)
                }),
            )
        };
        AppTitleBar::new()
            .tint(format!("title-tint-{tone:?}"), tint)
            .left(
                LobbySwitcher::new(
                    switcher_current,
                    switcher_entries,
                    forward(cx, |_: &mut Self, pick, _, cx| {
                        cx.emit(LobbyViewEvent::Switch(pick))
                    }),
                )
                .icons(icons_of(self.icons.clone())),
            )
            .right(
                div()
                    .children((layout.docked().is_none()).then(|| self.members_toggle(layout, cx))),
            )
            .right(div().children(crate::updates::AppUpdates::bar_control(window, cx)))
            .right(bell)
            .right(BarButton::settings(self.page.is_some(), {
                let lobby = cx.weak_entity();
                move |_, cx| {
                    let _ = lobby.update(cx, |v, cx| {
                        cx.emit(if v.page.is_some() {
                            LobbyViewEvent::LeaveSettings
                        } else {
                            LobbyViewEvent::OpenSettings
                        })
                    });
                }
            }))
    }

    /// The button for a sidebar that is not docked, folded in a narrow window or put away: people
    /// and the word, which says what it opens. Who is In Voice is the Voice bar's to say. Put
    /// away, it docks the sidebar again; folded, it opens the sheet, and is selected while it is
    /// out.
    fn members_toggle(&self, layout: SidebarLayout, cx: &Context<Self>) -> impl IntoElement {
        let put_away = layout == SidebarLayout::Collapsed;
        let open = !put_away && self.sidebar.read(cx).is_open();
        let sidebar = self.sidebar.clone();
        let lobby = cx.weak_entity();
        let button = ui::Button::new("members-toggle")
            .size(ui::Size::Bar)
            .icon(IconName::Users)
            .label("Members")
            .selected(open)
            .on_click(move |_, window, cx| {
                let _ = lobby.update(cx, |v, cx| {
                    if v.page.is_some() {
                        cx.emit(LobbyViewEvent::LeaveSettings);
                    }
                });
                sidebar.update(cx, |s, cx| {
                    if put_away {
                        s.expand(cx);
                    } else {
                        let open = !s.is_open();
                        s.set_open(open, window, cx);
                    }
                });
            });
        BarButton::slot(button, None)
    }
}

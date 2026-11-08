//! The member list in the right sidebar: no headings. Whoever is In Voice comes first, inside the
//! purple box; then everyone else online, then the offline Members, dimmed by their rows. The
//! signed-in Member is first in the box or, outside it, first of the rest. Every Member, the
//! signed-in one included, is a [`MemberRow`] whose right-click opens their Member Card at the
//! pointer, and so is every face in the small window's Voice strip.
//!
//! Rows are a gap apart, inside the box as well as outside it. The box's padding and border sit in
//! that gap (see [`voice_box`]), so nothing moves another Member's row: joining or leaving Voice
//! only grows or shrinks the box around the rows, going idle, Do Not Disturb or becoming the owner
//! changes only that row's marks.

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, AppContext as _, Context, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, SharedString,
    Styled as _, TestSupportExt as _, Window,
    base::{POPUP_PRIORITY, Positioner},
    component::{ActiveTheme as _, ThemeStyled as _, skeleton::Skeleton},
    deferred, div, ease_out_quint, relative,
};
use lobby_client::{Intent, LobbyState, Phase, Volume, local_date};
use lobby_protocol::{Presence, UserId, VoiceState};

use super::{
    forward,
    own_card::{OwnCard, Renamed},
    sidebar::{MemberSidebar, SidebarEvent},
    voice_active,
};
use crate::components::{
    AvatarSize, MEMBER_CARD_PAD, MemberPanel, MemberPanelModel, MemberRow, MemberRowActions,
    MemberRowModel, OwnerAction, PanelActions, PanelStep, ROW_HEIGHT, Rename, Sharer, Tile,
    WatchLine, picture, voice_box,
};
use crate::scale::{device, u};

/// The sidebar's groups. The signed-in Member is first in whichever group they are in. While
/// not connected the last known groups stay (`known` is false and the sidebar dims them), so
/// nobody seems to leave during a reconnect.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemberGroups {
    pub known: bool,
    pub in_voice: Vec<MemberRowModel>,
    pub online: Vec<MemberRowModel>,
    pub offline: Vec<MemberRowModel>,
}

impl MemberGroups {
    /// The signed-in Member's tile fills while their own share is on screen, which the Lobby
    /// view knows and the state does not.
    pub fn own_shown(mut self, shown: bool) -> Self {
        for row in self.in_voice.iter_mut().filter(|r| r.is_me && shown) {
            if row.tile == Some(Tile::Rest) {
                row.tile = Some(Tile::OnScreen { switching: false });
            }
        }
        self
    }
}

/// The tile of a share this person could show: one's own comes from this computer, a Member's
/// from the Watch the Session runs.
fn tile_of(state: &LobbyState, id: &UserId, is_me: bool, can_watch: bool) -> Tile {
    if !can_watch {
        return Tile::Faint;
    }
    if is_me {
        return Tile::Rest;
    }
    match state.share.watch.as_ref() {
        Some(w) if &w.sharer == id => Tile::OnScreen {
            switching: w.coming.is_some(),
        },
        Some(w) if w.coming.as_ref() == Some(id) => Tile::Coming,
        _ => Tile::Rest,
    }
}

/// The signed-in Member's own Voice state follows this client's Voice session, not the roster, so
/// once it ends they leave the Voice box at once, before the Lobby's word arrives. In a call,
/// who speaks is this client's own word, timed to what it plays.
pub fn shown_voice(state: &LobbyState, m: &lobby_protocol::Member) -> Option<VoiceState> {
    let is_me = state.me.as_ref() == Some(&m.id);
    match state.voice.phase {
        lobby_client::VoicePhase::Idle if is_me => None,
        lobby_client::VoicePhase::InVoice { .. } => m.voice.map(|v| VoiceState {
            speaking: state.voice.speaking.contains(&m.id),
            ..v
        }),
        _ => m.voice,
    }
}

pub fn voice_roster(state: &LobbyState) -> Vec<&lobby_protocol::Member> {
    state
        .in_voice()
        .into_iter()
        .filter(|m| shown_voice(state, m).is_some())
        .collect()
}

pub fn member_groups(state: &LobbyState) -> MemberGroups {
    let me = state.me.as_ref();
    let owner = state.owner();
    let can_watch = voice_active(state);
    let row = |m: &lobby_protocol::Member| {
        let voice = shown_voice(state, m);
        let sharing = voice.is_some() && state.share_of(&m.id).is_some();
        let is_me = Some(&m.id) == me;
        // One's own share is shown from this computer: the Lobby's word is not needed.
        let shows = voice.is_some()
            && if is_me {
                state.share.sharing.is_some()
            } else {
                sharing
            };
        MemberRowModel {
            id: m.id.clone(),
            name: m.username.to_string().into(),
            avatar: m.avatar.clone(),
            is_me,
            presence: m.presence,
            voice,
            sharing,
            tile: shows.then(|| tile_of(state, &m.id, is_me, can_watch)),
            owner: owner == Some(&m.id),
        }
    };
    let mut groups = MemberGroups {
        known: state.phase == Phase::Connected,
        ..MemberGroups::default()
    };
    groups.in_voice = voice_roster(state).into_iter().map(row).collect();
    for m in state
        .sidebar()
        .into_iter()
        .filter(|m| shown_voice(state, m).is_none())
    {
        if m.presence == Presence::Offline {
            groups.offline.push(row(m));
        } else {
            groups.online.push(row(m));
        }
    }
    groups
}

/// Whose share this person can show now, in the order of the member list: their own first, while
/// they share.
pub fn sharers(state: &LobbyState) -> Vec<Sharer> {
    member_groups(state)
        .in_voice
        .into_iter()
        .filter(|r| r.tile.is_some_and(|t| t != Tile::Faint))
        .map(|r| Sharer::of(&r))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenPanel {
    pub user: UserId,
    pub step: PanelStep,
    /// Where the pointer was when the card was asked for: the card's top left corner.
    pub at: Point<Pixels>,
}

/// The card keeps this far from the window's edge.
const CARD_MARGIN: f32 = 8.;

const ROW_FADE: Duration = Duration::from_millis(220);

fn member_skeleton() -> Vec<AnyElement> {
    [0.62_f32, 0.48, 0.7]
        .into_iter()
        .map(|w| {
            div()
                .h(u(ROW_HEIGHT))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .child(Skeleton::new().size(u(AvatarSize::Row.px())).rounded_full())
                .child(Skeleton::new().h(u(12.)).w(relative(w * 0.8)))
                .into_any_element()
        })
        .collect()
}

impl MemberSidebar {
    /// Before READY the list shows skeleton rows of the real size, like the feed's.
    pub(super) fn member_list(
        &mut self,
        groups: &MemberGroups,
        voice_surface: Hsla,
        before_ready: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let sidebar = cx.theme().sidebar;
        let known = groups.known;
        let actions = self.member_row_actions(cx);
        let mut members: Vec<AnyElement> = Vec::new();
        let anyone =
            !groups.in_voice.is_empty() || !groups.online.is_empty() || !groups.offline.is_empty();
        if !anyone && before_ready {
            self.rows_shown = std::mem::take(&mut self.rows_now);
            return member_skeleton();
        }
        if !groups.in_voice.is_empty() {
            let rows = groups
                .in_voice
                .iter()
                .map(|r| self.member_row(r, known, true, voice_surface, &actions, cx))
                .collect();
            members.push(voice_box(rows, voice_surface));
        }
        for r in groups.online.iter().chain(&groups.offline) {
            members.push(self.member_row(r, known, false, sidebar, &actions, cx));
        }
        let now = cx.background_executor().now();
        self.rows_shown = std::mem::take(&mut self.rows_now);
        self.rows_entered
            .retain(|_, at| now.duration_since(*at) < ROW_FADE * 2);
        members
    }

    fn member_row_actions(&self, cx: &mut Context<Self>) -> MemberRowActions {
        let (open, watch, stop) = (cx.weak_entity(), cx.weak_entity(), cx.weak_entity());
        MemberRowActions {
            open: Rc::new(move |(user, at), window, cx| {
                let _ = open.update(cx, |v, cx| v.open_member_panel(&user, at, window, cx));
            }),
            watch: Rc::new(move |sharer, _, cx| {
                let _ = watch.update(cx, |_, cx| cx.emit(SidebarEvent::Watch(sharer)));
            }),
            stop_watching: Rc::new(move |(), _, cx| {
                let _ = stop.update(cx, |_, cx| cx.emit(SidebarEvent::StopWatching));
            }),
        }
    }

    fn member_row(
        &mut self,
        row: &MemberRowModel,
        known: bool,
        in_voice_group: bool,
        surface: Hsla,
        actions: &MemberRowActions,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = MemberRow::key(&row.id, in_voice_group);
        self.rows_now.insert(key.clone());
        let image = picture(&self.images, row.avatar.as_ref(), cx);
        let element = MemberRow::new(row.clone(), known)
            .image(image)
            .in_voice_group(in_voice_group)
            .surface(surface)
            .still(self.rings_still())
            .open(self.member_panel.as_ref().is_some_and(|p| p.user == row.id))
            .actions(actions.clone())
            .into_any_element();
        let now = cx.background_executor().now();
        // The signed-in Member's own row never fades: joining an empty Voice only moves it into the
        // box.
        if !row.is_me && !self.rows_shown.contains(&key) && !self.rows_shown.is_empty() {
            self.rows_entered.insert(key.clone(), now);
        }
        let fading = self
            .rows_entered
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < ROW_FADE * 2);
        if !fading {
            return element;
        }
        div()
            .id(SharedString::from(format!("row-in-{key}")))
            .flex_shrink_0()
            .child(element)
            .with_animation(
                SharedString::from(format!("row-fade-{key}")),
                Animation::new(ROW_FADE).with_easing(ease_out_quint()),
                |d, delta| d.opacity(0.2 + 0.8 * delta),
            )
            .into_any_element()
    }

    /// What the open panel shows for `row`, if it is the row it opened from.
    fn panel_model(&self, row: &MemberRowModel, cx: &App) -> Option<MemberPanelModel> {
        let open = self.member_panel.as_ref().filter(|p| p.user == row.id)?;
        let session = self.session.read(cx);
        let state = session.state()?;
        let own = self
            .own_card
            .as_ref()
            .filter(|_| row.is_me)
            .map(|o| o.read(cx));
        Some(MemberPanelModel {
            name: row.name.clone(),
            is_me: row.is_me,
            owner: row.owner,
            watch: WatchLine::of(row),
            dnd: session.dnd(),
            in_voice: row.voice.is_some_and(|v| !v.joining),
            email: own.and_then(|o| o.email(cx)).map(Into::into),
            has_picture: own.is_some_and(|o| o.has_picture(cx)),
            picture_busy: own.is_some_and(|o| o.picture_busy(cx)),
            rename: own.map_or(Rename::Idle, |o| o.rename(cx)),
            manage: state.is_owner() && !row.is_me,
            step: state
                .owner_acting
                .get(&row.id)
                .map_or(open.step, |action| PanelStep::Busy(*action)),
            since: state
                .members
                .get(&row.id)
                .map(|m| local_date(m.created_at))
                .unwrap_or_default()
                .into(),
        })
    }

    /// The open panel, for the automation model.
    pub fn open_panel_model(&self, cx: &App) -> Option<MemberPanelModel> {
        let state = self.session.read(cx).state()?;
        let groups = self.groups(state);
        let row = groups
            .in_voice
            .iter()
            .chain(&groups.online)
            .chain(&groups.offline)
            .find(|r| self.member_panel.as_ref().is_some_and(|p| p.user == r.id))?;
        self.panel_model(row, cx)
    }

    /// Where the card's slider stands, in percent, for the automation model.
    pub fn open_panel_volume(&self, cx: &App) -> u16 {
        self.volume.read(cx).value().start().round() as u16
    }

    /// What is typed in the own card's name field, for the automation model.
    pub fn open_panel_typed(&self, cx: &App) -> Option<String> {
        self.own_card.as_ref().map(|own| own.read(cx).typed(cx))
    }

    /// The open Member Card, drawn at the pointer and above everything, whether the list is on
    /// screen or not. Escape closes it (the Lobby view catches the key wherever the focus is), and
    /// so does a press outside.
    pub(super) fn card_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let refocus = std::mem::take(&mut self.refocus_card);
        let open = self.member_panel.as_ref()?;
        let (user, at) = (open.user.clone(), open.at);
        if refocus {
            window.focus(&self.card_focus, cx);
        }
        let state = self.session.read(cx).state()?;
        let groups = self.groups(state);
        let row = groups
            .in_voice
            .iter()
            .chain(&groups.online)
            .chain(&groups.offline)
            .find(|r| r.id == user)?;
        let model = self.panel_model(row, cx)?;
        let panel = MemberPanel::new(model);
        let panel = match &self.own_card {
            Some(own) if row.is_me => panel.name_field(own.read(cx).field().clone()),
            _ if !row.is_me => panel.volume(self.volume.clone()),
            _ => panel,
        };
        let panel = panel.actions(self.card_actions(row, cx));
        let card = div()
            .id("member-card")
            .track_focus(&self.card_focus)
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                this.close_member_panel(&user, cx);
            }))
            .popover_style(cx)
            .p(u(MEMBER_CARD_PAD))
            .child(panel)
            .test_support();
        Some(
            deferred(
                Positioner::corner(Anchor::TopLeft, at)
                    .margin(device(CARD_MARGIN, window))
                    .occlude()
                    .child(card),
            )
            .with_priority(POPUP_PRIORITY)
            .into_any_element(),
        )
    }

    fn card_actions(&self, row: &MemberRowModel, cx: &mut Context<Self>) -> PanelActions {
        let sharer = Sharer::of(row);
        PanelActions {
            watch: forward(cx, move |v: &mut Self, (), _, cx| {
                v.member_panel = None;
                cx.emit(SidebarEvent::Watch(sharer.clone()));
                cx.notify();
            }),
            stop_watching: forward(cx, |v: &mut Self, (), _, cx| {
                v.member_panel = None;
                cx.emit(SidebarEvent::StopWatching);
                cx.notify();
            }),
            reset_volume: forward(cx, |v: &mut Self, (), window, cx| {
                v.reset_open_member_volume(window, cx)
            }),
            set_dnd: forward(cx, |v: &mut Self, on, _, cx| {
                v.session
                    .update(cx, |s, cx| s.dispatch(Intent::SetDnd(on), cx));
                cx.notify();
            }),
            voice_stats: forward(cx, |v: &mut Self, (), _, cx| {
                v.member_panel = None;
                cx.emit(SidebarEvent::VoiceStats);
                cx.notify();
            }),
            sign_out: forward(cx, |v: &mut Self, (), _, cx| {
                v.member_panel = None;
                cx.emit(SidebarEvent::SignOut);
                cx.notify();
            }),
            ask: forward(cx, |v: &mut Self, action, _, cx| {
                v.ask_owner_action(action, cx)
            }),
            confirm: forward(cx, |v: &mut Self, action, window, cx| {
                v.confirm_owner_action(action, window, cx)
            }),
            start_rename: forward(cx, |v: &mut Self, (), window, cx| {
                if let Some(own) = v.own_card.clone() {
                    own.update(cx, |o, cx| o.start_rename(window, cx));
                }
            }),
            save_rename: forward(cx, |v: &mut Self, (), window, cx| {
                if let Some(own) = v.own_card.clone() {
                    own.update(cx, |o, cx| o.save(window, cx));
                }
            }),
            change_picture: forward(cx, |v: &mut Self, (), window, cx| {
                if let Some(own) = v.own_card.clone() {
                    own.update(cx, |o, cx| o.change_picture(window, cx));
                }
                v.member_panel = None;
                cx.notify();
            }),
            remove_picture: forward(cx, |v: &mut Self, (), _, cx| {
                if let Some(own) = v.own_card.clone() {
                    own.update(cx, |o, cx| o.remove_picture(cx));
                }
            }),
        }
    }

    /// A right-click on `user`'s row or face: their card opens at the pointer, in place of any
    /// other.
    pub fn open_member_panel(
        &mut self,
        user: &UserId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.member_panel = Some(OpenPanel {
            user: user.clone(),
            step: PanelStep::Actions,
            at,
        });
        let is_me = self
            .session
            .read(cx)
            .state()
            .is_some_and(|s| s.me.as_ref() == Some(user));
        if is_me {
            let own = self.own_card(window, cx);
            own.update(cx, |o, cx| o.reopen(window, cx));
        } else {
            let saved = self.saved_volume(user, cx);
            self.show_volume(saved, window, cx);
        }
        window.focus(&self.card_focus, cx);
        cx.notify();
    }

    fn own_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<OwnCard> {
        if let Some(own) = &self.own_card {
            return own.clone();
        }
        let session = self.session.clone();
        let own = cx.new(|cx| OwnCard::new(session, window, cx));
        cx.observe(&own, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&own, |this, _, _: &Renamed, _| this.refocus_card = true)
            .detach();
        self.own_card = Some(own.clone());
        own
    }

    /// Another card's press outside closes this one in the same click, before that card opens.
    fn close_member_panel(&mut self, user: &UserId, cx: &mut Context<Self>) {
        if self.member_panel.as_ref().is_some_and(|p| &p.user == user) {
            self.member_panel = None;
            cx.notify();
        }
    }

    fn saved_volume(&self, user: &UserId, cx: &App) -> Volume {
        self.session
            .read(cx)
            .origin()
            .map(|origin| self.lobbies.read(cx).settings().member_volume(origin, user))
            .unwrap_or_default()
    }

    /// The slider stands where the open Member's volume is; it sends no change for this.
    fn show_volume(&mut self, volume: Volume, window: &mut Window, cx: &mut Context<Self>) {
        let percent = f32::from(volume.percent());
        self.volume
            .update(cx, |slider, cx| slider.set_value(percent, window, cx));
    }

    /// The open Member plays at `volume` from now on, in this call and the next, on this device.
    pub(super) fn set_open_member_volume(&mut self, volume: Volume, cx: &mut Context<Self>) {
        let Some(user) = self.member_panel.as_ref().map(|p| p.user.clone()) else {
            return;
        };
        let Some(origin) = self.session.read(cx).origin().cloned() else {
            return;
        };
        self.lobbies.update(cx, |lobbies, cx| {
            lobbies.update_settings(|s| s.set_member_volume(&origin, &user, volume), cx)
        });
        self.session.update(cx, |s, cx| {
            s.dispatch(Intent::MemberVolume { user, volume }, cx)
        });
    }

    fn reset_open_member_volume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_volume(Volume::DEFAULT, window, cx);
        self.set_open_member_volume(Volume::DEFAULT, cx);
    }

    fn owner_acting(&self, user: &UserId, cx: &App) -> bool {
        self.session
            .read(cx)
            .state()
            .is_some_and(|s| s.owner_acting.contains_key(user))
    }

    pub fn ask_owner_action(&mut self, action: Option<OwnerAction>, cx: &mut Context<Self>) {
        let acting = self
            .member_panel
            .as_ref()
            .is_some_and(|p| self.owner_acting(&p.user, cx));
        if let Some(panel) = &mut self.member_panel
            && !acting
        {
            panel.step = action.map_or(PanelStep::Actions, PanelStep::Confirm);
            cx.notify();
        }
    }

    /// The panel shows Busy until the Lobby answers, also when closed and opened again; a refusal
    /// offers the actions again.
    pub fn confirm_owner_action(
        &mut self,
        action: OwnerAction,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(user) = self
            .member_panel
            .as_ref()
            .filter(|p| p.step == PanelStep::Confirm(action))
            .map(|p| p.user.clone())
        else {
            return;
        };
        if self.owner_acting(&user, cx) {
            return;
        }
        if let Some(panel) = &mut self.member_panel {
            panel.step = PanelStep::Actions;
        }
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::Owner { user, action }, cx));
        cx.notify();
    }

    /// The Lobby accepted it: the panel it came from closes.
    pub(super) fn owner_acted(&mut self, user: &UserId) {
        if self.member_panel.as_ref().is_some_and(|p| &p.user == user) {
            self.member_panel = None;
        }
    }

    pub(super) fn keep_member_panel(&mut self, cx: &App) {
        let gone = self.member_panel.as_ref().is_some_and(|p| {
            self.session
                .read(cx)
                .state()
                .is_none_or(|s| !s.members.contains_key(&p.user))
        });
        if gone {
            self.member_panel = None;
        }
    }
}

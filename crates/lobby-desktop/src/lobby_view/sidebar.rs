//! The right sidebar: the member list, the Member Card a right-click on a row (or a face in the
//! small window's Voice strip) opens at the pointer, and the Voice controls pinned at its foot.
//! Docked, it is a floating sheet inset from the window's edges, and its leading edge drags its
//! width, which this device keeps in `state.json`. Below [`NARROW_BELOW`] it folds away, and
//! unfolds as a sheet over the feed that holds the focus, so Escape reaches it wherever the
//! person was. The card is drawn here whether or not the list is, since a face opens it too.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, AnyView, App, AppContext as _, Axis, Context, Entity,
    EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription,
    TestSupportExt as _, Window,
    base::{
        HandleEdge, InteractiveElementExt as _, ResizeHandleContext, ResizeHandleRenderer,
        ResizeHandleState, Transition, resize_handle,
        slider::{SliderEvent, SliderState},
        transition,
    },
    deferred, div, ease_out_quint, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _,
    px,
};
use lobby_client::{LobbyState, UiEffect, Volume};

use super::BODY_TOP;
use super::member_list::{MemberGroups, OpenPanel, member_groups};
use super::own_card::OwnCard;
use crate::scale::{u, window_scale};
use crate::{
    components::{
        BORDER, MEMBER_ROW_GAP, MediaImages, SURFACE_PAD, SURFACE_RADIUS, Sharer, VoiceBarActions,
        VoiceFoot, Waits, card_surface, floating_sheet, nested_radius, voice_bar_model,
    },
    kit::ActiveTheme as _,
    lobbies::Lobbies,
    session_entity::{SessionEntity, SessionUiEvent},
};

/// Below this width the member panel folds away behind the title bar's members button.
pub const NARROW_BELOW: f32 = 600.;
/// The right sidebar's width: room for the Voice controls' three buttons in one row.
pub const SIDEBAR_W: f32 = 264.;
pub const SIDEBAR_SLIM_W: f32 = 220.;
/// The sheet unfolded over a narrow window, which it does not share with a chat column.
pub const FOLDED_SHEET_W: f32 = 232.;
const SLIM_SIDEBAR_BELOW: f32 = 900.;
pub const SIDEBAR_MIN: f32 = SIDEBAR_SLIM_W;
pub const SIDEBAR_MAX: f32 = 420.;
/// What a dragged sidebar always leaves the chat column: the feed keeps its measure.
pub const MAIN_MIN: f32 = 340.;
/// The docked sheet's air at the window's right edge and bottom.
pub const SHEET_INSET: f32 = 8.;
pub const SIDEBAR_STEP: f32 = 16.;
/// A dragged edge goes this far under [`SIDEBAR_MIN`] before the sidebar puts itself away, so a
/// drag that overshoots the minimum by a hair does not lose it.
const COLLAPSE_SLACK: f32 = 20.;
const SHEET_IN: Duration = Duration::from_millis(220);

/// How the member list sits in a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SidebarLayout {
    /// Beside the feed, this wide in design pixels.
    Docked(f32),
    /// The person put it away. The title bar's members button brings it back at its last width.
    Collapsed,
    /// The window is too narrow to dock it: the same button opens it as a sheet over the feed.
    Sheet,
}

impl SidebarLayout {
    pub fn docked(self) -> Option<f32> {
        match self {
            SidebarLayout::Docked(width) => Some(width),
            _ => None,
        }
    }

    /// What the chat column gives up to the docked sheet, its inset included, in design pixels.
    pub fn taken(self) -> f32 {
        self.docked().map_or(0., |width| width + SHEET_INSET)
    }
}

/// The member list's layout in a window `window_px` wide drawn at `scale`, for the width the
/// person chose (`preferred`, in design pixels) and whether they put it away. A window too narrow
/// to dock it always gets the sheet, whatever was chosen.
pub fn sidebar_layout(
    window_px: f32,
    scale: f32,
    preferred: Option<f32>,
    collapsed: bool,
) -> SidebarLayout {
    match sidebar_width(window_px, scale, preferred) {
        None => SidebarLayout::Sheet,
        Some(_) if collapsed => SidebarLayout::Collapsed,
        Some(width) => SidebarLayout::Docked(width),
    }
}

/// The docked width, in design pixels, for a window `window_px` wide drawn at `scale`:
/// `preferred`, else the default for the window. `None` when it folds away. The rule is written
/// for 100%, so it sees the window in design pixels: a bigger scale folds the sidebar sooner.
fn sidebar_width(window_px: f32, scale: f32, preferred: Option<f32>) -> Option<f32> {
    let window_width = window_px / scale;
    if window_width < NARROW_BELOW {
        return None;
    }
    let default = if window_width < SLIM_SIDEBAR_BELOW {
        SIDEBAR_SLIM_W
    } else {
        SIDEBAR_W
    };
    let max = SIDEBAR_MAX
        .min(window_width - MAIN_MIN - SHEET_INSET)
        .max(SIDEBAR_MIN);
    Some(preferred.unwrap_or(default).clamp(SIDEBAR_MIN, max))
}

#[derive(Clone)]
pub(super) struct SidebarDrag;

impl Render for SidebarDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui_kit::Empty
    }
}

fn edge_appearance() -> ResizeHandleRenderer {
    Rc::new(|handle, window, cx| Some(edge_line(handle, window, cx)))
}

fn edge_line(handle: &ResizeHandleContext, window: &mut Window, cx: &mut App) -> AnyElement {
    let (length, opacity) = match handle.state() {
        ResizeHandleState::Idle => (0., 0.),
        ResizeHandleState::Hovered => (20., 0.35),
        ResizeHandleState::Pressed => (28., 0.6),
        ResizeHandleState::Dragging => (44., 0.9),
    };
    let motion = cx.theme().motion_tokens();
    let policy = Transition::new(motion.duration_fast).easing(motion.easing_move.clone());
    // Sampled on every frame, at rest too, so a new hover starts from where the last one ended.
    let length = transition("sidebar-edge-length", length, policy.clone(), window, cx);
    let opacity = transition("sidebar-edge-opacity", opacity, policy, window, cx).clamp(0., 1.);
    let theme = cx.theme();
    div()
        .flex_none()
        .flex()
        .items_center()
        .w(px(1.)) // px: hairline
        .h_full()
        .bg(theme.border)
        .when(length > 0.5, |line| {
            // Deferred, so the sheet's clip does not take its outer pixel past the border.
            line.child(deferred(
                div()
                    .flex_none()
                    .w(px(EDGE_PILL_W)) // px: drawn across a hairline
                    .h(px(length)) // px: drawn along a hairline
                    .ml(px((1. - EDGE_PILL_W) / 2.)) // px: centred on the hairline
                    .rounded_full()
                    .bg(theme.muted_foreground)
                    .opacity(opacity),
            ))
        })
        .into_any_element()
}

const EDGE_PILL_W: f32 = 3.;

/// What a row or the Member panel asks of the Lobby view.
pub enum SidebarEvent {
    Watch(Sharer),
    StopWatching,
    VoiceStats,
    SignOut,
    /// The folded sheet went away: the composer takes the focus back.
    Folded,
    /// Escape closed a card, so the keyboard goes back to the composer.
    CardClosed,
}

pub struct MemberSidebar {
    pub(super) session: Entity<SessionEntity>,
    pub(super) lobbies: Entity<Lobbies>,
    /// The Lobby view's pictures, which the rows draw avatars from.
    pub(super) images: Entity<MediaImages>,
    voice: VoiceBarActions,
    /// The folded sheet is out.
    open: bool,
    focus: FocusHandle,
    pub member_panel: Option<OpenPanel>,
    /// The open card holds the focus, so Escape reaches it.
    pub(super) card_focus: FocusHandle,
    /// The signed-in Member's name field and picture picker, made the first time their own card
    /// opens and kept, so a picker outlives the card.
    pub(super) own_card: Option<Entity<OwnCard>>,
    /// The name field went away with a Save, so the next frame puts the focus on the card.
    pub(super) refocus_card: bool,
    /// The one slider of the open Member Card, pointed at that Member when it opens.
    pub(super) volume: Entity<SliderState>,
    pub(super) rows_now: HashSet<SharedString>,
    /// Member rows drawn last frame, by key; a row not among them arrived in its group.
    pub(super) rows_shown: HashSet<SharedString>,
    pub(super) rows_entered: HashMap<SharedString, Instant>,
    /// Where on the edge the pointer took it, so a drag does not jump by that much.
    grab: f32,
    on_screen: OnScreen,
    _subscriptions: Vec<Subscription>,
}

/// The share the Lobby view shows, which the state does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnScreen {
    Nothing,
    /// A Member's, from the running Watch.
    Watch,
    /// This Member's own: their row says Watching.
    Own,
}

impl EventEmitter<SidebarEvent> for MemberSidebar {}

impl MemberSidebar {
    pub fn new(
        session: Entity<SessionEntity>,
        lobbies: Entity<Lobbies>,
        images: Entity<MediaImages>,
        voice: VoiceBarActions,
        cx: &mut Context<Self>,
    ) -> Self {
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(f32::from(Volume::MAX_PERCENT))
                .step(1.)
                .default_value(f32::from(Volume::DEFAULT.percent()))
        });
        let subscriptions = vec![
            cx.subscribe(&volume, |this, _, ev: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = ev {
                    this.set_open_member_volume(Volume::new(value.start().round() as u16), cx);
                }
            }),
            cx.observe(&volume, |_, _, cx| cx.notify()),
            cx.observe(&images, |_, _, cx| cx.notify()),
            cx.observe(&session, |this, _, cx| {
                this.keep_member_panel(cx);
                cx.notify();
            }),
            cx.observe(&lobbies, |_, _, cx| cx.notify()),
            cx.subscribe(&session, |this, _, ev: &SessionUiEvent, cx| {
                for effect in &ev.0.0 {
                    if let UiEffect::OwnerActed { user } = effect {
                        this.owner_acted(user);
                    }
                }
                cx.notify();
            }),
        ];
        Self {
            session,
            lobbies,
            images,
            voice,
            open: false,
            focus: cx.focus_handle(),
            member_panel: None,
            card_focus: cx.focus_handle(),
            own_card: None,
            refocus_card: false,
            volume,
            rows_now: HashSet::new(),
            rows_shown: HashSet::new(),
            rows_entered: HashMap::new(),
            grab: 0.,
            on_screen: OnScreen::Nothing,
            _subscriptions: subscriptions,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_on_screen(&mut self, on_screen: OnScreen, cx: &mut Context<Self>) {
        if self.on_screen != on_screen {
            self.on_screen = on_screen;
            cx.notify();
        }
    }

    pub fn groups(&self, state: &LobbyState) -> MemberGroups {
        member_groups(state).own_shown(self.on_screen == OnScreen::Own)
    }

    /// Speaking rings hold still while a share is on screen (see [`MemberAvatar::still`]).
    ///
    /// [`MemberAvatar::still`]: crate::components::MemberAvatar::still
    pub fn rings_still(&self) -> bool {
        self.on_screen != OnScreen::Nothing
    }

    /// Open, the sheet holds the focus so Escape folds it from anywhere; folded, the composer gets
    /// it back.
    pub fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.open == open {
            return;
        }
        self.open = open;
        if open {
            window.focus(&self.focus, cx);
        } else {
            self.member_panel = None;
            cx.emit(SidebarEvent::Folded);
        }
        cx.notify();
    }

    /// Escape cancels a rename in the open card first, then closes the card, then (in the unfolded
    /// sheet) the sheet. The keyboard goes back to where the card took it from.
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.member_panel.is_none() {
            self.set_open(false, window, cx);
            return;
        }
        let mine = self
            .session
            .read(cx)
            .state()
            .is_some_and(|s| s.me.as_ref() == self.member_panel.as_ref().map(|p| &p.user));
        if let Some(own) = self
            .own_card
            .clone()
            .filter(|o| mine && o.read(cx).is_editing())
        {
            own.update(cx, |o, cx| o.cancel_rename(window, cx));
            window.focus(&self.card_focus, cx);
            return;
        }
        self.member_panel = None;
        if self.open {
            window.focus(&self.focus, cx);
        } else {
            cx.emit(SidebarEvent::CardClosed);
        }
        cx.notify();
    }

    /// A page over the Lobby covers the card, so it goes.
    pub fn close_card(&mut self, cx: &mut Context<Self>) {
        if self.member_panel.take().is_some() {
            cx.notify();
        }
    }

    pub fn layout(&self, window: &Window, cx: &App) -> SidebarLayout {
        let lobbies = self.lobbies.read(cx);
        sidebar_layout(
            f32::from(window.viewport_size().width),
            window_scale(window),
            lobbies.sidebar_width(),
            lobbies.sidebar_collapsed(),
        )
    }

    /// The sidebar in the Lobby body, cached: a keystroke, a frame of a share or a pulse in the
    /// feed does not render it again. The body lays it out at the box its render fills, without
    /// rendering it.
    pub fn cached(sidebar: &Entity<Self>, window: &Window, cx: &App) -> AnyElement {
        let this = sidebar.read(cx);
        let style = StyleRefinement::default();
        let style = match this.layout(window, cx) {
            SidebarLayout::Docked(width) => style.flex_shrink_0().w(u(width)).mr(u(SHEET_INSET)),
            SidebarLayout::Sheet if this.open => style.absolute().inset_0(),
            SidebarLayout::Sheet | SidebarLayout::Collapsed => style,
        };
        AnyView::from(sidebar.clone())
            .cached(style)
            .into_any_element()
    }

    /// Its width in design pixels in this window; `None` while it is folded away or put away.
    pub fn shown(&self, window: &Window, cx: &App) -> Option<f32> {
        self.layout(window, cx).docked()
    }

    /// The title bar's button while the sidebar is put away: it comes back at its last width.
    pub fn expand(&mut self, cx: &mut Context<Self>) {
        self.lobbies
            .update(cx, |l, cx| l.set_sidebar_collapsed(false, cx));
    }

    /// `wanted` is a width in design pixels, or `None` for the default. Below the minimum, past
    /// `slack`, the sidebar is put away (its width stays for when it comes back); otherwise it is
    /// docked at the clamped width.
    fn resize(&mut self, wanted: Option<f32>, slack: f32, window: &Window, cx: &mut Context<Self>) {
        let (window_px, scale) = (
            f32::from(window.viewport_size().width),
            window_scale(window),
        );
        if self.layout(window, cx) == SidebarLayout::Sheet {
            return;
        }
        if wanted.is_some_and(|w| w + slack < SIDEBAR_MIN) {
            self.lobbies
                .update(cx, |l, cx| l.set_sidebar_collapsed(true, cx));
            return;
        }
        let width = wanted.and_then(|w| sidebar_width(window_px, scale, Some(w)));
        self.lobbies.update(cx, |l, cx| {
            l.set_sidebar_width(width, cx);
            l.set_sidebar_collapsed(false, cx);
        });
    }

    /// Keyboard step; the kit's handle takes no focus. Stepping past the minimum puts the
    /// sidebar away, and widening a sidebar that is away brings it back.
    pub fn step(&mut self, by: f32, window: &Window, cx: &mut Context<Self>) {
        match self.layout(window, cx) {
            SidebarLayout::Docked(now) => self.resize(Some(now + by), 0., window, cx),
            SidebarLayout::Collapsed if by > 0. => self.expand(cx),
            SidebarLayout::Collapsed | SidebarLayout::Sheet => {}
        }
    }

    /// The edge dragged to `from_right` window pixels before the window's right edge.
    pub fn drag(&mut self, from_right: f32, window: &Window, cx: &mut Context<Self>) {
        let wanted = (from_right + self.grab) / window_scale(window) - SHEET_INSET;
        self.resize(Some(wanted), COLLAPSE_SLACK, window, cx);
    }

    fn edge(&self, cx: &Context<Self>) -> impl IntoElement {
        let sidebar = cx.weak_entity();
        let handle = resize_handle::<SidebarDrag, SidebarDrag>("sidebar-edge", Axis::Horizontal)
            .with_appearance(edge_appearance())
            .inside(HandleEdge::Leading)
            .on_drag(SidebarDrag, move |drag, offset, _, cx| {
                let _ = sidebar.update(cx, |v, _| v.grab = f32::from(offset.x));
                cx.new(|_| drag.clone())
            })
            .on_double_click(cx.listener(|this, _, window, cx| this.resize(None, 0., window, cx)));
        div()
            .absolute()
            .left_0()
            .top(u(SURFACE_RADIUS))
            .bottom(u(SURFACE_RADIUS))
            .child(handle)
    }

    /// One list of sections, then the Voice controls pinned at its foot. Unfolded over a narrow
    /// window it has no foot: the Voice bar above the composer has the controls.
    fn panel(&mut self, panel_w: f32, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let session = self.session.read(cx);
        let Some(state) = session.state() else {
            return gpui_kit::Empty.into_any_element();
        };
        let groups = self.groups(state);
        let voice = voice_bar_model(state);
        let now = cx.background_executor().now();
        let waits = Waits::of(state, now);
        let before_ready = state.me.is_none();
        let voice_surface = card_surface(cx);
        let known = groups.known;
        let members = self.member_list(&groups, voice_surface, before_ready, cx);
        let sidebar = cx.theme().sidebar;
        let voice_bar = (!narrow).then(|| {
            div()
                .id("voice-bar")
                .debug_selector(|| "voice-bar".into())
                .flex_shrink_0()
                .p(u(SURFACE_PAD))
                .child(VoiceFoot::new(voice, self.voice.clone()).waits(waits))
                .test_support()
        });
        floating_sheet(cx)
            .id("members")
            .relative()
            .w(u(panel_w))
            .when(narrow, |d| d.h_full())
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("member-list")
                            .debug_selector(|| "member-list".into())
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(u(SURFACE_PAD))
                            .pt(u(SURFACE_PAD))
                            .pb_4()
                            .flex()
                            .flex_col()
                            .gap(u(MEMBER_ROW_GAP))
                            .when(!known, |d| d.opacity(0.55))
                            .children(members)
                            .test_support(),
                    )
                    // The list fades where it meets the controls, so a cut-off row reads as "more".
                    // With no foot the fade is the sheet's bottom, so it rounds its own corners:
                    // GPUI clips only to rectangles.
                    .child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(u(16.))
                            .when(narrow, |d| {
                                d.rounded_b(u(nested_radius(SURFACE_RADIUS, BORDER)))
                            })
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(sidebar.opacity(0.), 0.),
                                linear_color_stop(sidebar, 1.),
                            )),
                    ),
            )
            .children(voice_bar)
            .test_support()
            .into_any_element()
    }
}

impl Render for MemberSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.card_layer(window, cx);
        let (shown, narrow) = match self.layout(window, cx) {
            SidebarLayout::Docked(width) => (Some(width), false),
            SidebarLayout::Sheet => (None, true),
            SidebarLayout::Collapsed => return div().children(card).into_any_element(),
        };
        if narrow && !self.open {
            return div().children(card).into_any_element();
        }
        let panel_w = shown.unwrap_or(FOLDED_SHEET_W);
        let panel = self.panel(panel_w, narrow, cx);
        // Each root fills the box `cached` gives it in the Lobby body.
        if !narrow {
            // The Lobby body's padding gives the sheet its air above and below.
            return div()
                .relative()
                .size_full()
                .flex()
                .child(panel)
                .child(self.edge(cx))
                .children(card)
                .into_any_element();
        }
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("members-backdrop")
                    .test_support()
                    .absolute()
                    .inset_0()
                    .bg(cx.theme().overlay)
                    .on_click(cx.listener(|this, _, window, cx| this.set_open(false, window, cx))),
            )
            // The docked sheet's place: below the title bar by the body's air, inset at the right
            // and the bottom.
            .child(
                div()
                    .id("members-sheet")
                    .track_focus(&self.focus)
                    .absolute()
                    .top(u(BODY_TOP))
                    .right(u(SHEET_INSET))
                    .bottom(u(SHEET_INSET))
                    .child(panel)
                    .with_animation(
                        "members-in",
                        Animation::new(SHEET_IN).with_easing(ease_out_quint()),
                        move |d, delta| d.mr(u(-(panel_w + SHEET_INSET) * (1. - delta))),
                    ),
            )
            .children(card)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_follows_the_window_in_design_pixels_and_what_the_person_chose() {
        use SidebarLayout::{Collapsed, Docked, Sheet};
        for (window_px, scale, preferred, collapsed, want) in [
            // The default width, by the window's size in design pixels.
            (680., 1., None, false, Docked(220.)),
            (899., 1., None, false, Docked(220.)),
            (900., 1., None, false, Docked(264.)),
            // Too narrow to dock: the sheet, even if the person put it away.
            (599., 1., None, false, Sheet),
            (599., 1., Some(300.), true, Sheet),
            // A bigger scale sees a smaller window, a smaller one a bigger window.
            (680., 1.25, None, false, Sheet),
            (750., 1.25, None, false, Docked(220.)),
            (900., 1.5, None, false, Docked(220.)),
            (899., 1.5, None, false, Sheet),
            (480., 0.8, None, false, Docked(220.)),
            (1440., 1.5, None, false, Docked(264.)),
            // The chosen width is clamped to the chat's measure (beside the sheet's inset), the
            // maximum and the minimum.
            (1000., 1., Some(400.), false, Docked(400.)),
            (700., 1., Some(400.), false, Docked(352.)),
            (1600., 1., Some(900.), false, Docked(SIDEBAR_MAX)),
            (1000., 1., Some(10.), false, Docked(SIDEBAR_MIN)),
            // Put away, in a window that could dock it.
            (680., 1., None, true, Collapsed),
            (1440., 1.5, Some(300.), true, Collapsed),
        ] {
            assert_eq!(
                sidebar_layout(window_px, scale, preferred, collapsed),
                want,
                "{window_px} px at {scale}, chose {preferred:?}, put away {collapsed}"
            );
        }
    }
}

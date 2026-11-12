//! The Screen Share viewer: a dark video tile, above the feed or filling the Lobby view, with ink
//! over the picture, [`ShareToolbar`] over its top, and the words that say when a share is too
//! much for this computer to show. The share is another Member's, over the network, or the
//! Sharer's own, drawn from what their encoder sends.
//! The Lobby view sizes the tile with [`ShareView::set_place`]; the picture is letterboxed inside
//! it and never cropped, and the ink canvas covers the picture only. Each render tells the feed
//! the size it paints the picture at, in device pixels, and the decoder scales its pictures down
//! to it: a 4K share in a 680 px tile is read back and uploaded at 680 px.
//!
//! The toolbar shows as a video player's controls do (see [`super::share_controls`]).
//!
//! Each `BGRA` frame becomes a `RenderImage` without a pixel copy. Every `RenderImage` is a new GPU
//! atlas tile that nothing frees by itself, so the view keeps exactly two (the one painted now and
//! the one before it, which the GPU may still be drawing) and drops the older one on each new
//! frame. On close or release the rest go too. Without this the atlas grew about 250 MB/s. Cadence
//! comes from the decoder, never from GPUI timers.

use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Context, Div, Entity, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, MouseMoveEvent, ObjectFit, ParentElement as _, Pixels,
    Render, RenderImage, RenderOnce, SharedString, Size, Stateful, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Task, TestSupportExt as _, Window, assets::IconName, div,
    ease_out_quint, img, prelude::FluentBuilder as _, px,
};
use image::{Frame as ImageFrame, RgbaImage};
use lobby_client::{PaintSize, StrainWarning, VideoFeed, VideoFrame, copy};
use lobby_protocol::UserId;
use tokio::sync::mpsc;

use super::ink::{InkLayer, InkStroke, InkTags, ink_canvas};
use super::share_controls::{Controls, FADE_IN, FADE_OUT, Look, Pointer};
use super::{
    BAR_CONTROL_H, BORDER, Callback, ICON_SM, MemberRowModel, SURFACE_RADIUS, live_red,
    picture_backdrop,
};
use crate::kit::ActiveTheme as _;
use crate::scale::u;
use crate::ui;

const FPS_WINDOW: Duration = Duration::from_secs(1);
const SPINNER_LG: f32 = 24.;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewerStats {
    pub shown: u64,
    pub decoded: u64,
    pub size: Option<(u32, u32)>,
    pub fps: u32,
    pub images_dropped: usize,
}

pub struct ShareView {
    name: SharedString,
    /// Whose pictures it draws: a picture of anyone else's share waits in `held` until the
    /// Session says the view shows them. `None` on a view of one share only.
    sharer: Option<UserId>,
    held: Option<VideoFrame>,
    latest: Option<Arc<RenderImage>>,
    shown: Option<Arc<RenderImage>>,
    previous: Option<Arc<RenderImage>>,
    arrivals: VecDeque<Instant>,
    stats: ViewerStats,
    ended: bool,
    /// The Sharer's own share: its first picture comes from this computer, with nothing to
    /// connect to.
    own: bool,
    released: usize,
    place: Option<Place>,
    strain: Option<StrainWarning>,
    /// None on the Sharer's own view: no ink is painted inside what is being captured.
    ink: Option<InkLayer>,
    /// The left button went down on the picture in drawing mode and has not come up.
    pressing: std::rc::Rc<std::cell::Cell<bool>>,
    tags: InkTags,
    /// Where the view tells the decoder the size the picture is painted at.
    paint: PaintSize,
    controls: Controls,
    /// Held by the toolbar: a control with the keyboard focus keeps it shown.
    controls_focus: FocusHandle,
    /// Wakes the view when the pointer's rest on the picture has hidden the controls.
    rest: Option<Task<()>>,
    _pump: Task<()>,
}

/// Where the tile is and how big, in window pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    /// Above the feed, in a rounded frame.
    Panel(Size<Pixels>),
    /// Filling the view, edge to edge.
    Fullscreen(Size<Pixels>),
}

impl Place {
    pub fn size(self) -> Size<Pixels> {
        match self {
            Self::Panel(size) | Self::Fullscreen(size) => size,
        }
    }
}

pub fn contain(tile: Size<Pixels>, picture: (u32, u32)) -> Size<Pixels> {
    let (w, h) = (picture.0.max(1) as f32, picture.1.max(1) as f32);
    let scale = (f32::from(tile.width) / w).min(f32::from(tile.height) / h);
    Size {
        width: px((w * scale).floor()), // px: the picture is fitted in window pixels
        height: px((h * scale).floor()), // px: the picture is fitted in window pixels
    }
}

impl ShareView {
    pub fn new(name: impl Into<SharedString>, feed: VideoFeed, cx: &mut Context<Self>) -> Self {
        Self::build(name.into(), false, feed, cx)
    }

    pub fn own(feed: VideoFeed, cx: &mut Context<Self>) -> Self {
        Self::build(YOU.into(), true, feed, cx)
    }

    fn build(name: SharedString, own: bool, feed: VideoFeed, cx: &mut Context<Self>) -> Self {
        cx.on_release(|view: &mut ShareView, cx| {
            for image in view.take_images() {
                cx.drop_image(image, None);
                view.released += 1;
            }
        })
        .detach();
        let VideoFeed { frames, paint } = feed;
        Self {
            name,
            sharer: None,
            held: None,
            latest: None,
            shown: None,
            previous: None,
            arrivals: VecDeque::new(),
            stats: ViewerStats::default(),
            ended: false,
            own,
            released: 0,
            place: None,
            strain: None,
            ink: None,
            pressing: Default::default(),
            tags: InkTags::default(),
            paint,
            controls: Controls::default(),
            controls_focus: cx.focus_handle(),
            rest: None,
            _pump: Self::pump(frames, cx),
        }
    }

    /// The pointer moved over `at`: the controls show, and hide again when it rests.
    pub fn pointer_at(&mut self, at: Pointer, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        if !self.controls.shown(now, false) {
            cx.notify();
        }
        self.controls.moved(at, now);
        if at == Pointer::Picture && self.rest.is_none() {
            self.rest = Some(Self::wait_for_rest(cx));
        }
    }

    pub fn pointer_left(&mut self, from: Pointer, cx: &mut Context<Self>) {
        self.controls.left(from);
        cx.notify();
    }

    fn wait_for_rest(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let now = cx.background_executor().now();
                let Ok(Some(wait)) = this.update(cx, |view, _| view.controls.hides_in(now)) else {
                    break;
                };
                cx.background_executor().timer(wait).await;
            }
            let _ = this.update(cx, |view, cx| {
                if let Some(rest) = view.rest.take() {
                    rest.detach();
                }
                cx.notify();
            });
        })
    }

    fn controls_held(&self, window: &Window, cx: &App) -> bool {
        self.controls_focus.contains_focused(window, cx) && window.last_input_was_keyboard()
    }

    /// Whether the toolbar shows now.
    pub fn controls_shown(&self, window: &Window, cx: &App) -> bool {
        let now = cx.background_executor().now();
        self.controls.shown(now, self.controls_held(window, cx))
    }

    fn controls_look(&mut self, window: &Window, cx: &App) -> Look {
        let held = self.controls_held(window, cx);
        self.controls.look(cx.background_executor().now(), held)
    }

    pub fn released(&self) -> usize {
        self.released
    }

    pub fn name(&self) -> &SharedString {
        &self.name
    }

    pub fn stats(&self) -> ViewerStats {
        self.stats
    }

    /// The Lobby view computes the place while it renders, which renders this view too, so no
    /// notify is needed.
    pub fn set_place(&mut self, place: Place) {
        self.place = Some(place);
    }

    /// The decoder scales the next pictures to `fitted`, the size this one is painted at, in
    /// device pixels: it reads back and the GPU uploads only the pixels on screen.
    fn tell_paint_size(&self, fitted: Size<Pixels>, scale_factor: f32) {
        let side = |p: Pixels| (f32::from(p) * scale_factor).round() as u32;
        self.paint.set((side(fitted.width), side(fitted.height)));
    }

    pub fn set_strain(&mut self, strain: Option<StrainWarning>) {
        self.strain = strain;
    }

    pub fn strain(&self) -> Option<StrainWarning> {
        self.strain
    }

    /// The Sharer a Watch's view shows now, set with the place. A move to another drops the
    /// picture of the one before: under the new name only their pictures show.
    pub fn set_sharer(&mut self, sharer: UserId, name: SharedString, now: Instant) {
        self.name = name;
        if self.sharer.as_ref() == Some(&sharer) {
            return;
        }
        self.sharer = Some(sharer);
        self.latest = None;
        self.stats.shown = 0;
        self.arrivals.clear();
        if let Some(frame) = self.held.take() {
            self.push(frame, now);
        }
    }

    /// Set with the place, for the same reason.
    pub fn set_ink(&mut self, ink: Option<InkLayer>) {
        if ink.as_ref().is_none_or(|ink| ink.pen.is_none()) {
            self.pressing.set(false);
        }
        self.ink = ink;
    }

    /// The strokes painted over the picture at `now`.
    pub fn ink(&self, now: Instant) -> Vec<InkStroke> {
        self.ink
            .as_ref()
            .map(|ink| ink.strokes(now))
            .unwrap_or_default()
    }

    /// Whether the pointer on the picture draws.
    pub fn pen(&self) -> bool {
        self.ink.as_ref().is_some_and(|ink| ink.pen.is_some())
    }

    /// The canvas over the fitted picture. While a stroke shows, every frame repaints it, so a
    /// fade moves without anything else changing.
    fn ink_canvas(
        &self,
        size: Size<Pixels>,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let ink = self.ink.as_ref()?;
        let strokes = ink.strokes(cx.background_executor().now());
        if !strokes.is_empty() && ink.at.is_none() {
            window.request_animation_frame();
        }
        Some(ink_canvas(
            strokes,
            size,
            ink.pen.clone(),
            self.pressing.clone(),
            ink.spot.clone(),
            &self.tags,
            window,
        ))
    }

    fn pump(mut feed: mpsc::Receiver<VideoFrame>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(mut frame) = feed.recv().await {
                while let Ok(newer) = feed.try_recv() {
                    frame = newer;
                }
                let shown = this.update(cx, |view, cx| {
                    let now = cx.background_executor().now();
                    view.push(frame, now);
                    cx.notify();
                });
                if shown.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |view, cx| {
                view.ended = true;
                cx.notify();
            });
        })
    }

    pub fn push(&mut self, frame: VideoFrame, now: Instant) {
        if frame.sharer != self.sharer {
            self.held = Some(frame);
            return;
        }
        // The feed went back to this Sharer, so a held picture of another is older than this.
        self.held = None;
        let VideoFrame {
            width,
            height,
            bgra,
            source,
            decoded,
            sharer: _,
        } = frame;
        let Some(buffer) = RgbaImage::from_raw(width, height, bgra) else {
            tracing::warn!(width, height, "a video frame of the wrong size");
            return;
        };
        if self.latest.is_none() {
            tracing::debug!(own = self.own, decoded, "first share picture shown");
        }
        self.latest = Some(Arc::new(RenderImage::new([ImageFrame::new(buffer)])));
        self.stats.decoded = decoded;
        self.stats.size = Some(source);
        self.stats.shown += 1;
        self.arrivals.push_back(now);
        while self
            .arrivals
            .front()
            .is_some_and(|t| now.duration_since(*t) > FPS_WINDOW)
        {
            self.arrivals.pop_front();
        }
        self.stats.fps = self.arrivals.len() as u32;
    }

    fn take_images(&mut self) -> Vec<Arc<RenderImage>> {
        let mut images: Vec<Arc<RenderImage>> =
            [self.previous.take(), self.shown.take(), self.latest.take()]
                .into_iter()
                .flatten()
                .collect();
        images.dedup_by(|a, b| a.id == b.id);
        images
    }

    pub fn release(&mut self, window: &mut Window) {
        for image in self.take_images() {
            let _ = window.drop_image(image);
            self.stats.images_dropped += 1;
        }
    }

    fn advance(&mut self, window: &mut Window) -> Option<Arc<RenderImage>> {
        let latest = self.latest.clone()?;
        if self.shown.as_ref().is_some_and(|s| s.id == latest.id) {
            return Some(latest);
        }
        if let Some(old) = self.previous.take() {
            let _ = window.drop_image(old);
            self.stats.images_dropped += 1;
        }
        self.previous = self.shown.replace(latest.clone());
        Some(latest)
    }
}

/// Whose Screen Share is on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Whose {
    /// This Member's own, drawn from what their encoder sends: no network.
    Mine,
    Member(UserId),
}

/// How the Member's own share is named.
pub const YOU: &str = "You";

/// A share that can be shown now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharer {
    pub whose: Whose,
    pub name: SharedString,
}

impl Sharer {
    pub fn mine() -> Sharer {
        Sharer {
            whose: Whose::Mine,
            name: YOU.into(),
        }
    }

    /// The share of the Member a row stands for: one's own row is "You".
    pub fn of(row: &MemberRowModel) -> Sharer {
        if row.is_me {
            Sharer::mine()
        } else {
            Sharer {
                whose: Whose::Member(row.id.clone()),
                name: row.name.clone(),
            }
        }
    }
}

/// The toolbar's distance from the picture's top and right edges, in design pixels.
const TOOLBAR_INSET: f32 = 8.;
/// The air between the toolbar's pill and its buttons.
const TOOLBAR_PAD: f32 = 2.;
/// The pill's height: a bar control, its air and its edge.
const TOOLBAR_H: f32 = BAR_CONTROL_H + 2. * (TOOLBAR_PAD + BORDER);
/// How much of the sheet's brown the pill keeps: dark enough to read over any picture, with the
/// picture still behind it.
const TOOLBAR_OPACITY: f32 = 0.85;

/// What a share's toolbar shows: whose share it is, and where drawing stands on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarState {
    /// Your own share: "Let others draw" and its switch, on while friends may draw on it. With
    /// `blocked` the switch is disabled, and its tooltip says why.
    Mine {
        others_draw: bool,
        blocked: Option<&'static str>,
    },
    /// A friend's share: "Draw" while it lets others draw, with `pen` saying whether the pointer
    /// on the picture draws; nothing while it does not.
    Friend { pen: Option<bool> },
}

/// Where the toolbar lies.
pub enum ToolbarAt {
    /// Over the top of the rounded picture box above the feed.
    Panel,
    /// Over the top of the view it fills.
    Fullscreen,
}

/// The share's toolbar: one dark pill over the picture's top right with the draw control,
/// fullscreen and close, close at the far right. [`panel_frame`] and [`fullscreen_frame`] tie it
/// to the pointer over their view, so it shows and fades as [`Controls`] says; a toolbar on its
/// own stays shown. Hidden, a cover on it takes the pointer, so a touch that arrives with its
/// first move over the picture only shows the controls.
#[derive(IntoElement)]
pub struct ShareToolbar {
    state: ToolbarState,
    at: ToolbarAt,
    draw: Callback<()>,
    fullscreen: Callback<()>,
    close: Callback<()>,
    over: Option<Entity<ShareView>>,
}

impl ShareToolbar {
    /// `draw` is "Let others draw" on your own share, your pen on a friend's.
    pub fn new(
        state: ToolbarState,
        at: ToolbarAt,
        draw: Callback<()>,
        fullscreen: Callback<()>,
        close: Callback<()>,
    ) -> Self {
        Self {
            state,
            at,
            draw,
            fullscreen,
            close,
            over: None,
        }
    }

    /// The toolbar of `view`: it shows and hides with the pointer over that view.
    fn over(mut self, view: &Entity<ShareView>) -> Self {
        self.over = Some(view.clone());
        self
    }
}

/// The pointer moved on a part of a share, as seen from `view`.
fn pointer_moves(
    view: &Entity<ShareView>,
    at: Pointer,
) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, _, cx| view.update(cx, |v, cx| v.pointer_at(at, cx))
}

/// The pointer left `from` (or went under something that takes it).
fn pointer_leaves(
    view: &Entity<ShareView>,
    from: Pointer,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |hovered, _, cx| {
        if !*hovered {
            view.update(cx, |v, cx| v.pointer_left(from, cx));
        }
    }
}

/// "Let others draw" with its switch. The word turns the switch too, so it is a bigger target.
fn let_others_draw(on: bool, blocked: Option<&'static str>, draw: Callback<()>, cx: &App) -> Div {
    let theme = cx.theme();
    let label = div()
        .id("share-let-draw-label")
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(if blocked.is_some() {
            theme.muted_foreground
        } else {
            theme.foreground
        })
        .child("Let others draw")
        .when(blocked.is_none(), |d| {
            let draw = draw.clone();
            d.cursor_pointer()
                .on_click(move |_, window, cx| draw((), window, cx))
        });
    let switch = ui::Switch::new("share-let-draw-switch")
        .small()
        .checked(on)
        .disabled(blocked.is_some())
        .accessibility_label("Let others draw")
        .on_change(move |_, window, cx| draw((), window, cx));
    div()
        .h(u(BAR_CONTROL_H))
        .pl(u(10.))
        .pr(u(6.))
        .flex()
        .items_center()
        .gap(u(8.))
        .child(label)
        .child(switch)
}

impl RenderOnce for ShareToolbar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let look = match &self.over {
            Some(view) => view.update(cx, |v, cx| v.controls_look(window, cx)),
            None => Look::Shown,
        };
        let focus = self
            .over
            .as_ref()
            .map(|v| v.read(cx).controls_focus.clone());
        let theme = cx.theme();
        let (surface, edge, text) = (
            theme.popover.opacity(TOOLBAR_OPACITY),
            theme.border,
            theme.foreground,
        );
        let fullscreen = matches!(self.at, ToolbarAt::Fullscreen);
        let draw = self.draw;
        let draw: Option<AnyElement> = match self.state {
            ToolbarState::Mine {
                others_draw,
                blocked,
            } => {
                let control = let_others_draw(others_draw, blocked, draw, cx)
                    .id("share-let-draw")
                    .debug_selector(|| "share-let-draw".into())
                    .when_some(blocked, |c, why| {
                        c.tooltip(move |_, cx| ui::tooltip(why, cx))
                    })
                    .test_support();
                Some(control.into_any_element())
            }
            ToolbarState::Friend { pen: Some(on) } => Some(
                ui::Button::new("share-pen")
                    .debug_selector(|| "share-pen".into())
                    .size(ui::Size::Bar)
                    .rounded_full()
                    .selected(on)
                    .toggled(on)
                    .icon(IconName::Pencil)
                    .label("Draw")
                    .font_weight(FontWeight::MEDIUM)
                    .on_click(move |_, window, cx| draw((), window, cx))
                    .into_any_element(),
            ),
            ToolbarState::Friend { pen: None } => None,
        };
        let mine = matches!(self.state, ToolbarState::Mine { .. });
        let close = self.close;
        let close = ui::Button::new("share-close")
            .size(ui::Size::Bar)
            .icon(IconName::X)
            .debug_selector(|| "share-close".into())
            .accessibility_label(if mine { "Close" } else { "Stop watching" })
            .rounded_full()
            .tooltip(if mine {
                "Close. Your share goes on."
            } else {
                "Stop watching"
            })
            .on_click(move |_, window, cx| close((), window, cx));
        let (id, icon, label) = if fullscreen {
            (
                "share-fullscreen-exit",
                IconName::Minimize,
                "Leave fullscreen",
            )
        } else {
            ("share-fullscreen", IconName::Maximize, "Fullscreen")
        };
        let toggle = self.fullscreen;
        let fullscreen_button = ui::Button::new(id)
            .size(ui::Size::Bar)
            .icon(icon)
            .debug_selector(move || id.into())
            .accessibility_label(label)
            .rounded_full()
            .tooltip(if fullscreen {
                "Leave fullscreen (Esc twice)"
            } else {
                label
            })
            .on_click(move |_, window, cx| toggle((), window, cx));
        let over = self.over.as_ref();
        let cover = over.filter(|_| look == Look::Hidden);
        let pill = div()
            .id("share-controls")
            .h(u(TOOLBAR_H))
            .px(u(TOOLBAR_PAD))
            .relative()
            .flex()
            .items_center()
            .gap(u(TOOLBAR_PAD))
            .rounded_full()
            .bg(surface)
            .border_1()
            .border_color(edge)
            .text_color(text)
            .occlude()
            .when_some(focus, |d, focus| d.track_focus(&focus))
            .when_some(over, |d, view| {
                d.on_mouse_move(pointer_moves(view, Pointer::Controls))
                    .on_hover(pointer_leaves(view, Pointer::Controls))
            })
            .children(draw)
            .child(fullscreen_button)
            .child(close)
            .when_some(cover, |d, view| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_move(pointer_moves(view, Pointer::Controls)),
                )
            });
        let pill = match look {
            Look::Shown => pill.into_any_element(),
            Look::Hidden => pill.opacity(0.).into_any_element(),
            Look::Fading { shown, turn } => pill
                .with_animation(
                    SharedString::from(format!("share-controls-fade-{turn}")),
                    Animation::new(if shown { FADE_IN } else { FADE_OUT })
                        .with_easing(ease_out_quint()),
                    move |d, delta| d.opacity(if shown { delta } else { 1. - delta }),
                )
                .into_any_element(),
        };
        div()
            .id("share-toolbar")
            .debug_selector(|| "share-toolbar".into())
            .absolute()
            .top(u(TOOLBAR_INSET))
            .right(u(TOOLBAR_INSET))
            .child(pill)
            .test_support()
    }
}

/// Enough that a 16:9 share fills the column's width at the default 680×480, with no slivers of
/// backdrop at its sides.
const VIEWER_MAX_SHARE: f32 = 0.62;
/// Folded, the Voice strip also lives in the column: the tile takes less, so two lines of the
/// feed still show under it at the 480×400 minimum, as in the mock.
const VIEWER_MAX_SHARE_FOLDED: f32 = 0.34;
/// Under the panel's picture, in design pixels: its air above the feed.
const VIEWER_PANEL_PAD: f32 = 4.;

/// The picture's box in the panel above the feed, in design pixels: the column's width between
/// its gutters, and the picture's height at that width. The panel is capped at a share of the
/// column's height, so the feed below keeps room and stays anchored at the bottom; a picture the
/// cap makes narrower sits in the middle of the box, on the backdrop.
pub fn panel_fit(
    main_w: f32,
    main_h: f32,
    folded: bool,
    picture: Option<(u32, u32)>,
) -> (f32, f32) {
    let (pw, ph) = picture.unwrap_or((16, 9));
    let tile_w = (main_w - 32.).max(1.);
    let natural = tile_w * ph.max(1) as f32 / pw.max(1) as f32;
    let share = if folded {
        VIEWER_MAX_SHARE_FOLDED
    } else {
        VIEWER_MAX_SHARE
    };
    let cap = main_h * share + 16. - VIEWER_PANEL_PAD;
    (tile_w, natural.min(cap).floor().max(1.))
}

/// The box the toolbar shows and hides with: the pointer on it is the pointer on the picture.
fn stage(view: &Entity<ShareView>) -> Stateful<Div> {
    div()
        .id("share-stage")
        .relative()
        .on_mouse_move(pointer_moves(view, Pointer::Picture))
        .on_hover(pointer_leaves(view, Pointer::Picture))
}

/// The share above the feed: `view` in the box [`panel_fit`] gave, `toolbar` over its top.
pub fn panel_frame(
    toolbar: ShareToolbar,
    view: Entity<ShareView>,
    (tile_w, tile_h): (f32, f32),
) -> Div {
    let stage = stage(&view).w(u(tile_w)).h(u(tile_h));
    let toolbar = toolbar.over(&view);
    div()
        .flex_shrink_0()
        .h(u(tile_h + VIEWER_PANEL_PAD))
        .px_4()
        .pb(u(VIEWER_PANEL_PAD))
        .child(stage.child(view).child(toolbar))
}

/// The share over the whole area it is placed in: `view` filling it, `toolbar` over its top.
pub fn fullscreen_frame(toolbar: ShareToolbar, view: Entity<ShareView>, cx: &App) -> Div {
    let stage = stage(&view).size_full();
    let toolbar = toolbar.over(&view);
    div()
        .absolute()
        .inset_0()
        .bg(cx.theme().background)
        .occlude()
        .child(stage.child(view).child(toolbar))
}

/// What fullscreen says after the first Esc, laid over the foot of the frame.
pub fn esc_hint(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .absolute()
        .bottom_6()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .px_3()
                .py_1p5()
                .rounded_full()
                .bg(picture_backdrop(theme).opacity(0.78))
                .text_sm()
                .text_color(theme.foreground)
                .child("Press Esc again to leave fullscreen"),
        )
}

pub fn strain_notice(warning: &StrainWarning, framed: bool, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let title = copy::strain_title(warning);
    let detail = copy::strain_detail(warning);
    div()
        .id("share-strain")
        .debug_selector(|| "share-strain".into())
        .absolute()
        .bottom(u(if framed { 8. } else { 24. }))
        .left_0()
        .right_0()
        .px(u(12.))
        .flex()
        .justify_center()
        .child(
            div()
                .max_w_full()
                .px(u(12.))
                .py(u(6.))
                .rounded_xl()
                .flex()
                .items_center()
                .gap(u(8.))
                .bg(picture_backdrop(theme).opacity(0.85))
                .child(
                    ui::Icon::new(IconName::CircleAlert)
                        .size(u(ICON_SM))
                        .flex_shrink_0()
                        .text_color(theme.warning),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(title),
                        )
                        .children(
                            detail.map(|d| {
                                div().text_xs().text_color(theme.muted_foreground).child(d)
                            }),
                        ),
                ),
        )
        .test_support()
}

impl Render for ShareView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.advance(window);
        let stats = self.stats;
        let name = self.name.clone();
        let theme = cx.theme();
        let (soft, backdrop) = (theme.muted_foreground, picture_backdrop(theme));
        let outline = live_red(theme).opacity(0.35);
        let fitted = self
            .place
            .zip(stats.size)
            .map(|(place, picture)| contain(place.size(), picture));
        if let Some(fitted) = fitted {
            self.tell_paint_size(fitted, window.scale_factor());
        }
        let framed = !matches!(self.place, Some(Place::Fullscreen(_)));
        // GPUI clips children to a rectangle only, so the picture rounds its own corners where
        // they are the frame's. Behind bars as wide as the radius (a tall share) its corners are
        // clear of the frame's, and stay square against the bars.
        let radius = u(SURFACE_RADIUS).to_pixels(window.rem_size());
        let in_corners = self.place.zip(fitted).is_none_or(|(place, fitted)| {
            let bars = place.size() - fitted;
            bars.width.max(bars.height) < radius * 2.
        });
        let body = match frame {
            // Explicit pixels: the kit's image takes its intrinsic aspect ratio, so a percentage
            // size would grow past the tile and crop the picture.
            Some(image) => {
                let picture = img(image)
                    .object_fit(ObjectFit::Contain)
                    .when(framed && in_corners, |p| p.rounded(u(SURFACE_RADIUS)));
                let picture = match fitted {
                    Some(size) => {
                        let fitted = div()
                            .relative()
                            .w(size.width)
                            .h(size.height)
                            .child(picture.w(size.width).h(size.height));
                        let fitted = fitted.children(self.ink_canvas(size, window, cx));
                        fitted.into_any_element()
                    }
                    None => picture.size_full().into_any_element(),
                };
                div()
                    .id("viewer-video")
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(picture)
                    .into_any_element()
            }
            None => div()
                .id("viewer-waiting")
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(soft)
                .when(!self.ended && self.own, |d| {
                    d.child(ui::Spinner::new().color(soft))
                })
                .when(!self.ended && !self.own, |d| {
                    d.child(ui::Spinner::new().size(u(SPINNER_LG)).color(soft))
                        .child(format!("Connecting to {name}'s screen…"))
                })
                .when(self.ended, |d| d.child("The Screen Share ended."))
                .into_any_element(),
        };
        let notice = self.strain.map(|w| strain_notice(&w, framed, cx));
        div()
            .id("share-view")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(backdrop)
            .when(framed, |d| d.rounded(u(SURFACE_RADIUS)))
            .child(body)
            .children(notice)
            // The outline lies over the picture: a border on the tile would inset the picture
            // by its width, and the picture's corners would no longer meet the outline's.
            .when(framed, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(u(SURFACE_RADIUS))
                        .border_1()
                        .border_color(outline),
                )
            })
            .test_support()
    }
}

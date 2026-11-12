//! The Sharer's ink overlay: a window of its own over the shared monitor, or just above the
//! shared window, that paints the ink Lobby keeps off its own windows there, with the canvas's
//! look. The host makes it click-through, never focused and left out of captures
//! ([`NativeWindow::make_overlay`]); over a window it follows the window, and hides while the
//! window cannot be seen.

use std::{cell::Cell, rc::Rc, time::Duration};

use futures_lite::future;
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Point, Render, Styled as _, Task, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point,
    px, size,
};

use super::ink::{InkLayer, InkTags, ink_canvas};
use crate::host::{NativeWindow, OverlayFault, OverlayTarget, WindowWatch, per_monitor_v2};

/// Windows says nothing when another app's window is raised without being activated, nor when
/// a restore lifts it above the overlay after its last event: a shown overlay over a window
/// checks its place this often too.
const FOLLOW_CHECK_EVERY: Duration = Duration::from_millis(250);

/// Where a canvas was last painted in its window, so the overlay can paint over it.
pub type InkSpot = Rc<Cell<Option<Bounds<Pixels>>>>;

/// One board the overlay paints, and where.
#[derive(Clone)]
pub struct OverlayPane {
    pub layer: InkLayer,
    pub at: PaneAt,
}

#[derive(Clone)]
pub enum PaneAt {
    /// The whole overlay: the Member's own share of the monitor or window under it.
    Whole,
    /// Over the picture of a share the Member watches, in a Lobby window on the shared monitor.
    Picture { window: NativeWindow, spot: InkSpot },
}

pub struct InkOverlay {
    target: OverlayTarget,
    panes: Vec<OverlayPane>,
    stage: Stage,
    whole_tags: InkTags,
    picture_tags: InkTags,
}

enum Stage {
    /// Before the host made the window the overlay: it must never show.
    Opening,
    Made {
        native: NativeWindow,
        /// Whether the host was last asked to show it: only while it has a pane to paint.
        shown: bool,
        /// Over a window: places the overlay again whenever the window moves.
        _follow: Option<Task<()>>,
    },
}

impl InkOverlay {
    /// Opens the overlay's window, hidden and not yet click-through: [`InkOverlay::make`] does
    /// the rest, outside any GPUI update.
    pub fn open(target: OverlayTarget, cx: &mut App) -> gpui_kit::Result<WindowHandle<InkOverlay>> {
        let options = WindowOptions {
            // The host covers the target in physical pixels once the window exists.
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                Point::default(),
                size(px(1.), px(1.)), // px: a placeholder until the host sizes the window
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Lobby ink".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };
        per_monitor_v2(|| {
            cx.open_window(options, |_, cx| {
                cx.new(|_| InkOverlay {
                    target,
                    panes: Vec::new(),
                    stage: Stage::Opening,
                    whole_tags: InkTags::default(),
                    picture_tags: InkTags::default(),
                })
            })
        })
    }

    /// Makes the window the overlay over its target, hidden, and over a window starts following
    /// it. The host's calls send window messages, so this runs in a task, never in an update.
    pub async fn make(
        overlay: WindowHandle<InkOverlay>,
        cx: &mut AsyncApp,
    ) -> Result<NativeWindow, OverlayFault> {
        let (native, target) = overlay
            .update(cx, |o, window, _| (NativeWindow::of(window), o.target))
            .map_err(|_| OverlayFault::NoWindow)?;
        let native = native.ok_or(OverlayFault::NoWindow)?;
        native.make_overlay(target)?;
        let follow = match target {
            OverlayTarget::Monitor(_) => None,
            OverlayTarget::Window(hwnd) => {
                let (moved, mut heard) = tokio::sync::mpsc::unbounded_channel();
                let watch = WindowWatch::start(hwnd, moved)?;
                Some(cx.spawn(async move |cx| {
                    let _watch = watch;
                    loop {
                        let tick = cx.background_executor().timer(FOLLOW_CHECK_EVERY);
                        let heard_one = future::or(async { heard.recv().await.is_some() }, async {
                            tick.await;
                            true
                        });
                        if !heard_one.await {
                            return;
                        }
                        // A drag sends a burst: one placing covers all of it.
                        while heard.try_recv().is_ok() {}
                        let Ok(shown) = overlay.update(cx, |o, _, _| o.shown()) else {
                            return;
                        };
                        if shown && !native.show_overlay(target, true) {
                            tracing::warn!("the shared window is gone: no ink overlay");
                            return;
                        }
                    }
                }))
            }
        };
        overlay
            .update(cx, |o, _, _| {
                o.stage = Stage::Made {
                    native,
                    shown: false,
                    _follow: follow,
                };
            })
            .map_err(|_| OverlayFault::NoWindow)?;
        Ok(native)
    }

    pub fn native(&self) -> Option<NativeWindow> {
        match self.stage {
            Stage::Opening => None,
            Stage::Made { native, .. } => Some(native),
        }
    }

    pub fn shown(&self) -> bool {
        matches!(self.stage, Stage::Made { shown: true, .. })
    }

    /// What the overlay paints. It shows while there is a pane, once it is made.
    pub fn set_panes(&mut self, panes: Vec<OverlayPane>, cx: &mut Context<Self>) {
        let wanted = !panes.is_empty();
        self.panes = panes;
        if let Stage::Made { native, shown, .. } = &mut self.stage
            && wanted != *shown
        {
            *shown = wanted;
            let (native, target) = (*native, self.target);
            // Showing sends the window messages: never inside this update.
            cx.spawn(async move |_, _| {
                if !native.show_overlay(target, wanted) {
                    tracing::warn!("the shared monitor or window is gone: no ink overlay");
                }
            })
            .detach();
        }
        cx.notify();
    }

    pub fn panes(&self) -> &[OverlayPane] {
        &self.panes
    }

    /// Where `pane` lies in this window. A picture is placed from both windows' places on the
    /// screen: they share the monitor, so one scale holds for both.
    fn bounds(&self, pane: &PaneAt, window: &Window) -> Option<Bounds<Pixels>> {
        match pane {
            PaneAt::Whole => Some(Bounds::new(Point::default(), window.viewport_size())),
            PaneAt::Picture {
                window: lobby,
                spot,
            } => {
                let spot = spot.get()?;
                let (lx, ly) = lobby.client_origin()?;
                let (ox, oy) = self.native()?.client_origin()?;
                let scale = window.scale_factor();
                let offset = point(
                    px((lx - ox) as f32 / scale), // px: screen pixels to this window's
                    px((ly - oy) as f32 / scale), // px: screen pixels to this window's
                );
                Some(Bounds::new(spot.origin + offset, spot.size))
            }
        }
    }
}

impl Render for InkOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = cx.background_executor().now();
        let mut painting = false;
        let mut panes = Vec::new();
        for pane in &self.panes {
            let Some(bounds) = self.bounds(&pane.at, window) else {
                continue;
            };
            let strokes = pane.layer.strokes(now);
            painting |= !strokes.is_empty();
            let tags = match pane.at {
                PaneAt::Whole => &self.whole_tags,
                PaneAt::Picture { .. } => &self.picture_tags,
            };
            let canvas = ink_canvas(
                strokes,
                bounds.size,
                None,
                Rc::default(),
                None,
                tags,
                window,
            );
            panes.push(
                div()
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .child(canvas),
            );
        }
        // Every frame while ink shows: the fade moves, and a watched picture may move with its
        // window, which this window hears nothing of.
        if painting {
            window.request_animation_frame();
        }
        div()
            .id("ink-overlay")
            .size_full()
            .relative()
            .children(panes)
    }
}

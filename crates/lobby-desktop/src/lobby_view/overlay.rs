//! The overlay over the monitor or window this Member shares: opened when the share starts, made
//! click-through and left out of captures before it ever shows, shown only while it has ink to
//! paint, and closed with the share.

use gpui_kit::{App, Context, Entity, Task, WindowHandle};
use lobby_client::SourceId;

use super::{
    LobbyView,
    ink::{InkHome, ink_home, share_of},
};
use crate::{
    components::{
        Whose,
        ink::{InkLayer, ink_authors},
        ink_overlay::{InkOverlay, InkSpot, OverlayPane, PaneAt},
    },
    host::OverlayTarget,
};

pub(super) struct Overlay {
    /// The shared monitor or window, as the capture has it.
    pub target: OverlayTarget,
    /// Where the canvas of the share watched meanwhile was last painted.
    pub spot: InkSpot,
    pub state: OverlayState,
}

pub(super) enum OverlayState {
    /// The window exists; the host is making it click-through and left out of captures.
    Opening {
        window: WindowHandle<InkOverlay>,
        _setup: Task<()>,
    },
    Ready {
        window: WindowHandle<InkOverlay>,
        view: Entity<InkOverlay>,
    },
    /// Drawing is unavailable on this share; the log says why.
    Failed,
}

impl Overlay {
    pub fn ready(&self) -> bool {
        matches!(self.state, OverlayState::Ready { .. })
    }

    /// Closes the window, after the update this runs in.
    fn close(self, cx: &mut App) {
        let window = match self.state {
            OverlayState::Opening { window, .. } | OverlayState::Ready { window, .. } => window,
            OverlayState::Failed => return,
        };
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        });
    }
}

fn target_of(source: &SourceId) -> Option<OverlayTarget> {
    match *source {
        SourceId::Monitor { hmonitor } => Some(OverlayTarget::Monitor(hmonitor)),
        SourceId::Window { hwnd } => Some(OverlayTarget::Window(hwnd)),
        SourceId::TestPattern { .. } | SourceId::TestBlocked => None,
    }
}

impl LobbyView {
    /// An overlay while this Member shares a monitor or a window, over it; none otherwise.
    pub(super) fn follow_overlay(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .session
            .read(cx)
            .state()
            .and_then(|s| target_of(&s.share.sharing.as_ref()?.id));
        if self.overlay.as_ref().map(|o| o.target) == wanted {
            return;
        }
        if let Some(old) = self.overlay.take() {
            old.close(cx);
        }
        self.overlay = wanted.map(|target| Self::open_overlay(target, cx));
    }

    fn open_overlay(target: OverlayTarget, cx: &mut Context<Self>) -> Overlay {
        let state = match InkOverlay::open(target, cx) {
            Ok(window) => OverlayState::Opening {
                window,
                _setup: cx.spawn(async move |this, cx| {
                    let made = InkOverlay::make(window, cx).await;
                    let _ = this.update(cx, |v, cx| {
                        if let Err(fault) = made {
                            tracing::warn!(%fault, "drawing unavailable on this share");
                        }
                        v.overlay_made(target, made.is_ok(), cx);
                    });
                }),
            },
            Err(e) => {
                tracing::warn!(error = %e, "no ink overlay window: drawing unavailable");
                OverlayState::Failed
            }
        };
        Overlay {
            target,
            spot: InkSpot::default(),
            state,
        }
    }

    fn overlay_made(&mut self, target: OverlayTarget, made: bool, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay.as_mut().filter(|o| o.target == target) else {
            return;
        };
        let OverlayState::Opening { window, .. } = &overlay.state else {
            return;
        };
        let window = *window;
        overlay.state = match (made, window.entity(cx)) {
            (true, Ok(view)) => {
                tracing::info!("ink overlay ready");
                OverlayState::Ready { window, view }
            }
            _ => {
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                });
                OverlayState::Failed
            }
        };
        cx.notify();
    }

    /// Hands the overlay the ink it paints: the Member's own share over all of what they share,
    /// and the share they watch over its picture when that is on the shared monitor. It shows
    /// only while some share it paints has drawing on.
    pub(super) fn paint_overlay(&mut self, cx: &mut Context<Self>) {
        let panes = self.overlay_panes(cx);
        if let Some(Overlay {
            state: OverlayState::Ready { view, .. },
            ..
        }) = &self.overlay
        {
            view.update(cx, |o, cx| o.set_panes(panes, cx));
        }
    }

    fn overlay_panes(&self, cx: &App) -> Vec<OverlayPane> {
        let Some(overlay) = self.overlay.as_ref().filter(|o| o.ready()) else {
            return Vec::new();
        };
        let Some(state) = self.session.read(cx).state() else {
            return Vec::new();
        };
        let capture = self.capture(state);
        let layer = |share: lobby_protocol::ScreenShare| InkLayer {
            board: state.ink.get(&share.share_id).cloned(),
            authors: ink_authors(
                state
                    .members
                    .iter()
                    .map(|(id, m)| (id, m.username.as_str())),
            ),
            me: state.me.clone(),
            pen: None,
            at: None,
            spot: None,
        };
        let mut panes = Vec::new();
        if let Some(own) = share_of(state, &Whose::Mine).filter(|s| state.may_draw(s.share_id))
            && ink_home(capture, &Whose::Mine) == InkHome::Overlay
        {
            panes.push(OverlayPane {
                layer: layer(own),
                at: PaneAt::Whole,
            });
        }
        if let Some(whose) = self.viewing.whose(state)
            && let Some(window) = self.host_window
            && let Some(share) = share_of(state, &whose).filter(|s| state.may_draw(s.share_id))
            && whose != Whose::Mine
            && ink_home(capture, &whose) == InkHome::Overlay
        {
            panes.push(OverlayPane {
                layer: layer(share),
                at: PaneAt::Picture {
                    window,
                    spot: overlay.spot.clone(),
                },
            });
        }
        panes
    }

    /// The overlay's state for the scenario dump: `None` while nothing on screen is shared.
    #[cfg(feature = "automation")]
    pub fn overlay_dump(
        &self,
        cx: &App,
    ) -> Option<(&'static str, &'static str, bool, Vec<&'static str>)> {
        let overlay = self.overlay.as_ref()?;
        let over = match overlay.target {
            OverlayTarget::Monitor(_) => "monitor",
            OverlayTarget::Window(_) => "window",
        };
        let (state, shown, panes) = match &overlay.state {
            OverlayState::Opening { .. } => ("opening", false, Vec::new()),
            OverlayState::Failed => ("failed", false, Vec::new()),
            OverlayState::Ready { view, .. } => {
                let view = view.read(cx);
                let panes = view
                    .panes()
                    .iter()
                    .map(|p| match p.at {
                        PaneAt::Whole => "whole",
                        PaneAt::Picture { .. } => "picture",
                    })
                    .collect();
                ("ready", view.shown(), panes)
            }
        };
        Some((over, state, shown, panes))
    }

    /// Closes the overlay with the view.
    pub(super) fn close_overlay(&mut self, cx: &mut App) {
        if let Some(overlay) = self.overlay.take() {
            overlay.close(cx);
        }
    }

    /// Why the Sharer cannot let viewers draw on their share, while that is so.
    pub(super) fn drawing_blocked(&self) -> Option<Blocked> {
        match self.overlay.as_ref()?.state {
            OverlayState::Opening { .. } => Some(Blocked::Opening),
            OverlayState::Ready { .. } => None,
            OverlayState::Failed => Some(Blocked::Unavailable),
        }
    }
}

/// Why "Let others draw" is off on a share of a monitor or window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Blocked {
    /// For the moment the overlay takes to be ready.
    Opening,
    /// The overlay cannot keep ink out of the capture.
    Unavailable,
}

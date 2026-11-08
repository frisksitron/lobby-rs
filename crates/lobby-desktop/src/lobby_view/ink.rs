//! Drawing on the share on screen: the ink its view paints, the draw control in its toolbar, and
//! the pointer's strokes into the Session. While this Member shares a monitor, no Lobby window on
//! it paints ink: the overlay paints it instead ([`super::overlay`]), as it paints their own ink
//! over a window they share.

use gpui_kit::{App, Context};
use lobby_client::{Intent, LobbyState, SourceId};
use lobby_protocol::ScreenShare;

use super::{LobbyView, forward, overlay::Blocked, share::Shown};
use crate::{
    components::{
        Callback, ToolbarState, Whose,
        ink::{InkLayer, PenInput, ink_authors},
    },
    host::OverlayTarget,
};

/// The share `whose` stands for, while it streams.
pub(super) fn share_of(state: &LobbyState, whose: &Whose) -> Option<ScreenShare> {
    match whose {
        Whose::Mine => state.share_of(state.me.as_ref()?),
        Whose::Member(id) => state.share_of(id),
    }
}

/// What this Member's own share captures of their screen, as far as ink cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Capture {
    /// Nothing on this computer's screen: no share, or a test pattern.
    Nothing,
    /// Another app's window, which no Lobby window is part of. `overlay`: the overlay over it is
    /// ready.
    Window { overlay: bool },
    /// A whole screen. `overlay`: the overlay over it is ready. `here`: the Lobby window is on
    /// it.
    Screen { overlay: bool, here: bool },
}

/// Where the ink over a share is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InkHome {
    /// The canvas over its picture in the Lobby window.
    Canvas,
    /// The overlay over the shared monitor.
    Overlay,
    /// Nowhere on this computer: anything painted would be in the capture.
    Nowhere,
}

/// The routing rule. The Member's own share is never painted in the Lobby window, which may be
/// inside what it captures; the overlay paints it over a shared monitor or window. A share they
/// watch paints on its canvas unless the Lobby window is on the shared screen, where the overlay
/// paints it if there is one.
pub(super) fn ink_home(capture: Capture, whose: &Whose) -> InkHome {
    match (whose, capture) {
        (
            Whose::Mine,
            Capture::Screen { overlay: true, .. } | Capture::Window { overlay: true },
        ) => InkHome::Overlay,
        (Whose::Mine, _) => InkHome::Nowhere,
        (
            Whose::Member(_),
            Capture::Nothing | Capture::Window { .. } | Capture::Screen { here: false, .. },
        ) => InkHome::Canvas,
        (Whose::Member(_), Capture::Screen { overlay: true, .. }) => InkHome::Overlay,
        (Whose::Member(_), Capture::Screen { overlay: false, .. }) => InkHome::Nowhere,
    }
}

const PEN_OPENING: &str = "Getting your screen ready for drawing";
const PEN_UNAVAILABLE: &str = "Drawing is unavailable: Windows cannot keep ink out of this share";

impl LobbyView {
    pub(super) fn capture(&self, state: &LobbyState) -> Capture {
        let Some(source) = &state.share.sharing else {
            return Capture::Nothing;
        };
        let overlay = |target| {
            self.overlay
                .as_ref()
                .is_some_and(|o| o.target == target && o.ready())
        };
        match source.id {
            SourceId::Monitor { hmonitor } => Capture::Screen {
                overlay: overlay(OverlayTarget::Monitor(hmonitor)),
                here: self.host_window.is_none_or(|w| w.on_monitor(hmonitor)),
            },
            SourceId::Window { hwnd } => Capture::Window {
                overlay: overlay(OverlayTarget::Window(hwnd)),
            },
            SourceId::TestPattern { .. } | SourceId::TestBlocked => Capture::Nothing,
        }
    }

    /// What the view of `shown` paints, and where the overlay should find its canvas.
    pub(super) fn show_ink(&self, shown: &Shown, cx: &mut Context<Self>) {
        let layer = self.ink_layer(shown, cx);
        shown.view.update(cx, |v, _| v.set_ink(layer));
    }

    fn ink_layer(&self, shown: &Shown, cx: &Context<Self>) -> Option<InkLayer> {
        let state = self.session.read(cx).state()?;
        let whose = shown.whose(state)?;
        if whose == Whose::Mine {
            return None;
        }
        let share = share_of(state, &whose)?;
        let home = ink_home(self.capture(state), &whose);
        let pen = (shown.pen && state.may_draw(share.share_id))
            .then(|| forward(cx, |v: &mut Self, input: PenInput, _, cx| v.pen(input, cx)));
        Some(InkLayer {
            board: (home == InkHome::Canvas)
                .then(|| state.ink.get(&share.share_id).cloned())
                .flatten(),
            authors: ink_authors(
                state
                    .members
                    .iter()
                    .map(|(id, m)| (id, m.username.as_str())),
            ),
            me: state.me.clone(),
            pen,
            at: None,
            spot: (home == InkHome::Overlay)
                .then(|| self.overlay.as_ref().map(|o| o.spot.clone()))
                .flatten(),
        })
    }

    /// What the toolbar of `shown` says about drawing, and what its draw control does: the
    /// Sharer's "Let others draw" on their own share, a friend's pen while the share they watch
    /// has drawing on. On a share of a monitor the Sharer's switch waits for the overlay, and
    /// stays off where it cannot keep ink out of the capture.
    pub(super) fn toolbar_state(
        &self,
        shown: &Shown,
        cx: &Context<Self>,
    ) -> (ToolbarState, Callback<()>) {
        let drawing = self.session.read(cx).state().is_some_and(|state| {
            shown
                .whose(state)
                .and_then(|whose| share_of(state, &whose))
                .is_some_and(|share| state.may_draw(share.share_id))
        });
        if !shown.own {
            let state = ToolbarState::Friend {
                pen: drawing.then_some(shown.pen),
            };
            return (
                state,
                forward(cx, |v: &mut Self, (), _, cx| v.toggle_pen(cx)),
            );
        }
        let state = ToolbarState::Mine {
            others_draw: drawing,
            blocked: self.drawing_blocked().map(|blocked| match blocked {
                Blocked::Opening => PEN_OPENING,
                Blocked::Unavailable => PEN_UNAVAILABLE,
            }),
        };
        let toggle = forward(cx, move |v: &mut Self, (), _, cx| {
            v.set_drawing(!drawing, cx)
        });
        (state, toggle)
    }

    fn set_drawing(&mut self, on: bool, cx: &mut Context<Self>) {
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::SetDrawing(on), cx));
        cx.notify();
    }

    fn toggle_pen(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.viewing.shown_mut() else {
            return;
        };
        shown.pen = !shown.pen;
        if !shown.pen {
            self.session
                .update(cx, |s, cx| s.dispatch(Intent::PenUp(None), cx));
        }
        cx.notify();
    }

    fn pen(&mut self, input: PenInput, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let intent = match input {
            PenInput::Down(at) => {
                let share = self.viewing.shown().and_then(|shown| {
                    let state = session.read(cx).state()?;
                    share_of(state, &shown.whose(state)?)
                });
                let Some(share) = share else {
                    return;
                };
                Intent::PenDown {
                    share: share.share_id,
                    at,
                }
            }
            PenInput::Move(at) => Intent::PenTo(at),
            PenInput::Up(at) => Intent::PenUp(at),
        };
        session.update(cx, |s, cx| s.dispatch(intent, cx));
    }

    /// Esc while the pen is down ends the stroke, and only that. Returns whether it did.
    pub(super) fn end_stroke(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.session.read(cx).pen_is_down() {
            return false;
        }
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::PenUp(None), cx));
        true
    }

    /// Drawing mode ends with the share's drawing, so it is off again when drawing comes back.
    pub(super) fn follow_drawing(&mut self, cx: &App) {
        let drawing = self
            .viewing
            .shown()
            .and_then(|shown| {
                let state = self.session.read(cx).state()?;
                Some(state.may_draw(share_of(state, &shown.whose(state)?)?.share_id))
            })
            .unwrap_or(false);
        if let Some(shown) = self.viewing.shown_mut()
            && !drawing
        {
            shown.pen = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bob() -> Whose {
        Whose::Member("usr_000000000000000000000002".parse().expect("a user id"))
    }

    const MONITOR_HERE: Capture = Capture::Screen {
        overlay: true,
        here: true,
    };

    #[test]
    fn sharing_a_monitor_sends_all_ink_on_it_to_the_overlay() {
        assert_eq!(ink_home(MONITOR_HERE, &Whose::Mine), InkHome::Overlay);
        assert_eq!(ink_home(MONITOR_HERE, &bob()), InkHome::Overlay);
    }

    #[test]
    fn a_watched_share_on_another_monitor_keeps_its_canvas() {
        let elsewhere = Capture::Screen {
            overlay: true,
            here: false,
        };
        assert_eq!(ink_home(elsewhere, &bob()), InkHome::Canvas);
        assert_eq!(ink_home(elsewhere, &Whose::Mine), InkHome::Overlay);
    }

    #[test]
    fn without_a_working_overlay_no_ink_is_painted_inside_the_capture() {
        let no_overlay = Capture::Screen {
            overlay: false,
            here: true,
        };
        assert_eq!(ink_home(no_overlay, &Whose::Mine), InkHome::Nowhere);
        assert_eq!(ink_home(no_overlay, &bob()), InkHome::Nowhere);
    }

    #[test]
    fn without_a_share_on_screen_watched_ink_is_on_the_canvas_and_own_ink_nowhere() {
        assert_eq!(ink_home(Capture::Nothing, &bob()), InkHome::Canvas);
        assert_eq!(ink_home(Capture::Nothing, &Whose::Mine), InkHome::Nowhere);
    }

    #[test]
    fn sharing_a_window_paints_own_ink_on_its_overlay_and_watched_ink_on_the_canvas() {
        let window = Capture::Window { overlay: true };
        assert_eq!(ink_home(window, &Whose::Mine), InkHome::Overlay);
        assert_eq!(ink_home(window, &bob()), InkHome::Canvas);
        let no_overlay = Capture::Window { overlay: false };
        assert_eq!(ink_home(no_overlay, &Whose::Mine), InkHome::Nowhere);
        assert_eq!(ink_home(no_overlay, &bob()), InkHome::Canvas);
    }
}

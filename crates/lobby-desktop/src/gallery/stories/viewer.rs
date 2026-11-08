//! Stories of a Screen Share on screen, another Member's or one's own: the viewer in its panel and
//! filling the view, with its toolbar over the picture's top, and Viewers' ink over the picture.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::{
    AnyElement, Context, Entity, IntoElement, ParentElement as _, Styled as _, Window,
    component::ActiveTheme as _, div, prelude::FluentBuilder as _, size,
};
use lobby_client::{
    Shortfall, StrainCause, StrainRole, StrainWarning,
    ink::{InkBoard, point_at},
};
use lobby_protocol::{DrawingEpoch, ShareId, StrokeId};

use crate::components::{
    Place, Pointer, SURFACE_RADIUS, ShareToolbar, ShareView, ToolbarAt, ToolbarState, esc_hint,
    fullscreen_frame,
    ink::{InkLayer, ink_authors},
    panel_fit, panel_frame, picture_backdrop,
};
use crate::gallery::{Gallery, Section, fixtures::user_id};
use crate::scale::{device_in, u};

use super::{caption, wrap};

const FRIENDS: ToolbarState = ToolbarState::Friend { pen: None };
const MINE: ToolbarState = ToolbarState::Mine {
    others_draw: false,
    blocked: None,
};

/// The toolbar showing `state`.
fn toolbar(state: ToolbarState, fullscreen: bool) -> ShareToolbar {
    let at = if fullscreen {
        ToolbarAt::Fullscreen
    } else {
        ToolbarAt::Panel
    };
    ShareToolbar::new(
        state,
        at,
        Rc::new(|_, _, _| {}),
        Rc::new(|_, _, _| {}),
        Rc::new(|_, _, _| {}),
    )
}

/// The small window's view under the title bar (480 × 360), which the viewer fills or sits in.
const VIEW: (f32, f32) = (480., 360.);

/// The panel above the feed in the small view, sized by the same rule as the Lobby view's.
fn panel(view: &Entity<ShareView>, bar: ShareToolbar, cx: &mut Context<Gallery>) -> AnyElement {
    panel_in(VIEW, view, bar, cx)
}

fn panel_in(
    column: (f32, f32),
    view: &Entity<ShareView>,
    bar: ShareToolbar,
    cx: &mut Context<Gallery>,
) -> AnyElement {
    let (tile_w, tile_h) = panel_fit(column.0, column.1, false, view.read(cx).stats().size);
    let at = size(device_in(tile_w, cx), device_in(tile_h, cx));
    view.update(cx, |v, _| v.set_place(Place::Panel(at)));
    panel_frame(bar, view.clone(), (tile_w, tile_h))
        .w(u(column.0))
        .into_any_element()
}

const DEFAULT_COLUMN: (f32, f32) = (468., 400.);

fn strained(role: StrainRole, cause: StrainCause) -> Option<StrainWarning> {
    Some(StrainWarning { role, cause })
}

const SLOW: StrainCause = StrainCause::Network(Some(Shortfall {
    reaching: 8_000_000,
    needs: 20_000_000,
}));

/// The viewer filling the small view, with the Esc hint over it while `hint`.
fn fullscreen(
    view: &Entity<ShareView>,
    bar: ShareToolbar,
    hint: bool,
    cx: &mut Context<Gallery>,
) -> AnyElement {
    let at = size(device_in(VIEW.0, cx), device_in(VIEW.1, cx));
    view.update(cx, |v, _| v.set_place(Place::Fullscreen(at)));
    div()
        .relative()
        .w(u(VIEW.0))
        .h(u(VIEW.1))
        .flex_shrink_0()
        .child(fullscreen_frame(bar, view.clone(), cx).when(hint, |d| d.child(esc_hint(cx))))
        .into_any_element()
}

/// The toolbar alone, over a strip of the picture's backdrop as wide as the small panel.
fn toolbar_alone(state: ToolbarState, cx: &mut Context<Gallery>) -> AnyElement {
    div()
        .relative()
        .w(u(VIEW.0 - 32.))
        .h(u(64.))
        .rounded(u(SURFACE_RADIUS))
        .bg(picture_backdrop(cx.theme()))
        .child(toolbar(state, false))
        .into_any_element()
}

/// A stroke for the board: who drew it, its points over the picture, and how long ago the pen
/// lifted (none: still drawing).
struct Drawn {
    author: &'static str,
    points: Vec<(f32, f32)>,
    lifted: Option<Duration>,
}

/// An ellipse around (`cx`, `cy`), as a hand rings something on the screen: a little more than
/// once round.
fn ring(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<(f32, f32)> {
    (0..=44)
        .map(|i| {
            let t = i as f32 / 40. * std::f32::consts::TAU;
            (cx + rx * t.cos(), cy + ry * t.sin())
        })
        .collect()
}

/// A wavy line from `x0` to `x1` at height `y`, as a hand underlines.
fn underline(x0: f32, x1: f32, y: f32) -> Vec<(f32, f32)> {
    (0..=24)
        .map(|i| {
            let t = i as f32 / 24.;
            (x0 + (x1 - x0) * t, y + 0.012 * (t * 9.).sin())
        })
        .collect()
}

/// An arrow from (`x0`, `y0`) to its head at (`x1`, `y1`), in one stroke.
fn arrow(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(f32, f32)> {
    let mut points: Vec<(f32, f32)> = (0..=12)
        .map(|i| {
            let t = i as f32 / 12.;
            (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)
        })
        .collect();
    points.extend([(x1 - 0.05, y1 + 0.01), (x1, y1), (x1 - 0.01, y1 + 0.08)]);
    points
}

/// The view whose pointer is away, so its toolbar is hidden.
const AWAY: usize = 2;

/// You are eve, watching bob's share; everyone else draws.
const ME: &str = "eve";

/// The board as it stands at `at`, held there, with `pen` as drawing mode.
fn ink(drawn: Vec<Drawn>, pen: bool, at: Instant) -> InkLayer {
    let share = ShareId::new(1).expect("not zero");
    let mut board = InkBoard::new(share, DrawingEpoch(1));
    for (n, stroke) in drawn.iter().enumerate() {
        let author = user_id(stroke.author);
        let id = StrokeId(n as u32);
        for (x, y) in &stroke.points {
            board.echo(&author, id, point_at(*x, *y), at);
        }
        if let Some(ago) = stroke.lifted {
            board.lift(&author, id, at.checked_sub(ago).unwrap_or(at));
        }
    }
    let names = ["bob", "carol", "dave", ME].map(|n| (user_id(n), n));
    InkLayer {
        board: Some(board),
        authors: ink_authors(names.iter().map(|(id, n)| (id, *n))),
        me: Some(user_id(ME)),
        pen: pen.then(|| Rc::new(|_, _: &mut Window, _: &mut gpui_kit::App| {}) as _),
        at: Some(at),
        spot: None,
    }
}

pub(super) fn viewer(g: &mut Gallery, _: &mut Window, cx: &mut Context<Gallery>) -> Vec<Section> {
    let v = g.held.viewers.clone();
    let now = Instant::now();
    let fading = Duration::from_millis(2500);
    let full = Duration::from_millis(500);
    // The pointer rests on the toolbar of every view but one, so its stories keep it shown.
    for (n, view) in v.iter().enumerate() {
        if n != AWAY {
            view.update(cx, |v, cx| v.pointer_at(Pointer::Controls, cx));
        }
    }
    v[5].update(cx, |v, _| {
        v.set_ink(Some(ink(
            vec![
                Drawn {
                    author: "dave",
                    points: underline(0.16, 0.6, 0.62),
                    lifted: Some(fading),
                },
                Drawn {
                    author: ME,
                    points: arrow(0.08, 0.2, 0.2, 0.36),
                    lifted: Some(full),
                },
                Drawn {
                    author: "carol",
                    points: ring(0.5, 0.42, 0.16, 0.2),
                    lifted: None,
                },
            ],
            false,
            now,
        )))
    });
    v[6].update(cx, |v, _| {
        v.set_ink(Some(ink(
            vec![
                Drawn {
                    author: "bob",
                    points: underline(0.55, 0.9, 0.86),
                    lifted: None,
                },
                Drawn {
                    author: ME,
                    points: ring(0.32, 0.3, 0.1, 0.12)[..30].to_vec(),
                    lifted: None,
                },
                Drawn {
                    author: "carol",
                    points: arrow(0.95, 0.1, 0.78, 0.28),
                    lifted: None,
                },
            ],
            true,
            now,
        )))
    });
    for (view, warning) in [
        (&v[7], strained(StrainRole::Viewer, SLOW)),
        (&v[8], strained(StrainRole::Viewer, StrainCause::Computer)),
        (&v[9], strained(StrainRole::Sharer, SLOW)),
        (&v[10], strained(StrainRole::Sharer, StrainCause::Computer)),
    ] {
        view.update(cx, |v, _| v.set_strain(warning));
    }
    vec![
        Section::new(
            "strain",
            "A share that cannot keep up says why, once, over the foot of the picture, after the trouble has lasted three seconds and until it has been gone for three. A Viewer's link or computer, then the Sharer's upload or computer. The numbers come with the link's words when they are known: what gets through against what the share needs",
            wrap([
                caption(
                    "Viewer, link, in the small window",
                    panel(&v[7], toolbar(FRIENDS, false), cx),
                    cx,
                ),
                caption(
                    "Viewer, computer, in the small window",
                    panel(&v[8], toolbar(FRIENDS, false), cx),
                    cx,
                ),
                caption(
                    "Sharer, upload, in the default window",
                    panel_in(DEFAULT_COLUMN, &v[9], toolbar(MINE, false), cx),
                    cx,
                ),
                caption(
                    "Sharer, computer, in the default window",
                    panel_in(DEFAULT_COLUMN, &v[10], toolbar(MINE, false), cx),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "panel",
            "In the panel above the feed, the toolbar is one dark pill at the picture's top right: the draw control, fullscreen and close, with close at the far right. It shows while the pointer moves over the picture or rests on the pill, and fades out two seconds after the pointer last moved; with the pointer away it is hidden. The watch tiles in the member list choose whose share is shown",
            wrap([
                caption("bob's share", panel(&v[0], toolbar(FRIENDS, false), cx), cx),
                caption("Your own", panel(&v[1], toolbar(MINE, false), cx), cx),
                caption(
                    "bob's share, the pointer away",
                    panel(&v[AWAY], toolbar(FRIENDS, false), cx),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "fullscreen",
            "Fullscreen fills the view under the title bar of the small window (480 × 360) with the picture, fitted and never cropped, and the same toolbar over its top",
            wrap([
                caption(
                    "bob's share",
                    fullscreen(&v[3], toolbar(FRIENDS, true), false, cx),
                    cx,
                ),
                caption(
                    "Your own, after the first Esc",
                    fullscreen(&v[4], toolbar(MINE, true), true, cx),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "ink",
            "Viewers' ink over the picture, never on the letterbox, each Member in their own colour: their avatar's hue where no one else has it. A stroke stays full for two seconds after the pen lifts, then fades over one. The name of a Member still drawing rides at the tip of their stroke, beside its last step on the side that faces the middle of the picture; your own stroke needs none. Your own share shows no ink",
            wrap([
                caption(
                    "carol drawing, your arrow, dave's line fading",
                    panel(
                        &v[5],
                        toolbar(ToolbarState::Friend { pen: Some(false) }, false),
                        cx,
                    ),
                    cx,
                ),
                caption(
                    "Drawing, with bob and carol",
                    fullscreen(
                        &v[6],
                        toolbar(ToolbarState::Friend { pen: Some(true) }, true),
                        false,
                        cx,
                    ),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "draw",
            "The draw control says what it does. On your own share, \"Let others draw\" and its switch; the word turns it too. On a friend's share that lets others draw, \"Draw\" puts you in drawing mode, where the pointer draws; on one that does not, the toolbar has only fullscreen and close",
            wrap([
                caption("Your own", toolbar_alone(MINE, cx), cx),
                caption(
                    "Your own: others can draw",
                    toolbar_alone(
                        ToolbarState::Mine {
                            others_draw: true,
                            blocked: None,
                        },
                        cx,
                    ),
                    cx,
                ),
                caption(
                    "Your own: the overlay is not ready",
                    toolbar_alone(
                        ToolbarState::Mine {
                            others_draw: false,
                            blocked: Some("Getting your screen ready for drawing"),
                        },
                        cx,
                    ),
                    cx,
                ),
                caption(
                    "Drawing on, pen up",
                    toolbar_alone(ToolbarState::Friend { pen: Some(false) }, cx),
                    cx,
                ),
                caption(
                    "Drawing mode",
                    toolbar_alone(ToolbarState::Friend { pen: Some(true) }, cx),
                    cx,
                ),
                caption("Drawing off", toolbar_alone(FRIENDS, cx), cx),
            ]),
        ),
    ]
}

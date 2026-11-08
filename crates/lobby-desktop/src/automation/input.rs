//! Synthetic GPUI input for one window (clicks, keys, text, actions, host events), shared by
//! the UI tests and the automation channel. Nothing touches the OS: no SendInput, focus or cursor.

use std::{path::PathBuf, time::Duration};

use gpui_kit::{
    App, Bounds, ClipboardItem, ElementId, Entity, ExternalPaths, FileDropEvent, MouseButton,
    MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput,
    ScrollDelta, ScrollEvent, ScrollSource, Size, TouchPhase, Window,
    base::test_support::{ElementSnapshot, snapshots},
    point,
    test::TestWindowExt as _,
};

use super::scenario::PenStep;
use crate::shell::{IDLE_AFTER, Shell};

#[derive(Debug, Clone, PartialEq)]
pub enum WindowStep {
    Click(String),
    MoveTo(String),
    RightClick(String),
    Type {
        text: String,
        into: Option<String>,
    },
    Press(String),
    Scroll {
        list: String,
        to: String,
    },
    Wheel {
        id: String,
        notches: i32,
    },
    Action(String),
    Drag {
        id: String,
        by: (f32, f32),
    },
    Pen(PenStep),
    Drop {
        id: String,
        paths: Vec<PathBuf>,
    },
    /// Uses the test platform's clipboard only, so never offer it on a real app.
    Paste {
        id: String,
        item: ClipboardItem,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepError {
    #[error("{0:?} is not drawn")]
    NotDrawn(String),
    #[error("{0:?} is drawn but not visible")]
    Hidden(String),
    #[error("{0:?} names {1} elements")]
    Ambiguous(String, usize),
    #[error("{0:?} is not a keystroke: {1}")]
    BadKey(String, String),
    #[error("unknown action {0:?}")]
    UnknownAction(String),
    #[error("{0} is not possible here (the real app only: cargo xtask drive)")]
    Unsupported(String),
    #[error("{to:?} did not come into view in {list:?}")]
    OutOfReach { list: String, to: String },
    #[error("{0:?} is under a dialog")]
    Covered(String),
}

/// `id` is the last segment of an element's path, as text.
fn matching(window: &Window, id: &str) -> Vec<ElementSnapshot> {
    snapshots(window)
        .into_iter()
        .filter(|s| s.path().last().is_some_and(|last| last.to_string() == id))
        .collect()
}

pub fn clickable(window: &Window, id: &str) -> Result<ElementSnapshot, StepError> {
    let mut found = matching(window, id);
    let visible = found.iter().filter(|s| s.visible()).count();
    match (found.len(), visible) {
        (0, _) => Err(StepError::NotDrawn(id.into())),
        (_, 0) => Err(StepError::Hidden(id.into())),
        (_, 1) => Ok(found.swap_remove(found.iter().position(|s| s.visible()).unwrap())),
        (_, n) => Err(StepError::Ambiguous(id.into(), n)),
    }
}

pub fn element_bounds(window: &Window, id: &str) -> Option<Bounds<Pixels>> {
    let found = matching(window, id);
    found
        .iter()
        .find(|s| s.visible())
        .or(found.first())
        .map(|s| s.bounds())
}

/// Where an element is drawn among gpui-component's layers, lowest first: the app, each dialog in
/// the order it opened, then the notifications.
fn layer(path: &[ElementId]) -> u64 {
    path.iter()
        .find_map(|id| match id {
            ElementId::NamedInteger(name, n) if name.as_ref() == "dialog-instance" => Some(n + 1),
            ElementId::NamedInteger(name, _) if name.as_ref() == "notification-list" => {
                Some(u64::MAX)
            }
            _ => None,
        })
        .unwrap_or(0)
}

/// Whether a dialog drawn above `target` is mounted. Under an open dialog a click lands on its
/// backdrop, and one still leaving after it closed (gpui-kit's 250 ms exit) takes the click from
/// the field beneath, so the text typed next goes nowhere.
fn covered(window: &Window, target: &ElementSnapshot) -> bool {
    let under = layer(target.path());
    snapshots(window).iter().any(|s| {
        s.path()
            .last()
            .is_some_and(|last| last.to_string() == "dialog")
            && layer(s.path()) > under
    })
}

/// The pointer moved to the middle of the element.
fn move_to(window: &mut Window, id: &str, cx: &mut App) -> Result<(), StepError> {
    window.render_frame(cx);
    let at = clickable(window, id)?.bounds().center();
    let modifiers = window.modifiers();
    let event = PlatformInput::MouseMove(MouseMoveEvent {
        position: at,
        pressed_button: None,
        modifiers,
    });
    window.dispatch_event(event, cx);
    window.render_frame(cx);
    Ok(())
}

fn click(window: &mut Window, id: &str, cx: &mut App) -> Result<(), StepError> {
    press(window, id, MouseButton::Left, cx)
}

/// `button` pressed and released in the middle of the element.
fn press(
    window: &mut Window,
    id: &str,
    button: MouseButton,
    cx: &mut App,
) -> Result<(), StepError> {
    window.render_frame(cx);
    let target = clickable(window, id)?;
    if covered(window, &target) {
        return Err(StepError::Covered(id.into()));
    }
    let at = target.bounds().center();
    let modifiers = window.modifiers();
    let events = [
        PlatformInput::MouseMove(MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers,
        }),
        PlatformInput::MouseDown(MouseDownEvent {
            button,
            position: at,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        PlatformInput::MouseUp(MouseUpEvent {
            button,
            position: at,
            modifiers,
            click_count: 1,
        }),
    ];
    for event in events {
        window.dispatch_event(event, cx);
        window.render_frame(cx);
    }
    Ok(())
}

/// A hand's drag arrives in several moves, not one jump.
const DRAG_MOVES: usize = 8;
/// Past GPUI's drag threshold (2 px), so the drag starts with the first move.
const DRAG_START: f32 = 3.;

/// GPUI starts a drag only after the pointer moves a little, and measures from there. That first
/// move goes across the drag's direction, so the element follows `by` exactly.
fn drag(window: &mut Window, id: &str, by: (f32, f32), cx: &mut App) -> Result<(), StepError> {
    window.render_frame(cx);
    let from = clickable(window, id)?.bounds().center();
    let modifiers = window.modifiers();
    let at = |i: usize| {
        let t = i as f32 / DRAG_MOVES as f32;
        point(
            from.x + gpui_kit::px(by.0 * t),
            from.y + gpui_kit::px(by.1 * t),
        )
    };
    let across = if by.0.abs() >= by.1.abs() {
        point(from.x, from.y + gpui_kit::px(DRAG_START))
    } else {
        point(from.x + gpui_kit::px(DRAG_START), from.y)
    };
    let mut events = vec![
        PlatformInput::MouseMove(MouseMoveEvent {
            position: from,
            pressed_button: None,
            modifiers,
        }),
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        PlatformInput::MouseMove(MouseMoveEvent {
            position: across,
            pressed_button: Some(MouseButton::Left),
            modifiers,
        }),
    ];
    events.extend((1..=DRAG_MOVES).map(|i| {
        PlatformInput::MouseMove(MouseMoveEvent {
            position: at(i),
            pressed_button: Some(MouseButton::Left),
            modifiers,
        })
    }));
    events.push(PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position: at(DRAG_MOVES),
        modifiers,
        click_count: 1,
    }));
    for event in events {
        window.dispatch_event(event, cx);
        window.render_frame(cx);
    }
    Ok(())
}

/// The left button on the ink canvas, at fractions of the picture it covers: down there, moved
/// there while held, or up where the pointer is.
fn pen(window: &mut Window, step: PenStep, cx: &mut App) -> Result<(), StepError> {
    window.render_frame(cx);
    let modifiers = window.modifiers();
    let on_canvas = |window: &Window, x: f32, y: f32| -> Result<_, StepError> {
        let b = clickable(window, INK_CANVAS)?.bounds();
        Ok(point(
            b.origin.x + b.size.width * x,
            b.origin.y + b.size.height * y,
        ))
    };
    let events = match step {
        PenStep::Down(x, y) => {
            let at = on_canvas(window, x, y)?;
            vec![
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: at,
                    pressed_button: None,
                    modifiers,
                }),
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
            ]
        }
        PenStep::Move(x, y) => vec![PlatformInput::MouseMove(MouseMoveEvent {
            position: on_canvas(window, x, y)?,
            pressed_button: Some(MouseButton::Left),
            modifiers,
        })],
        PenStep::Up => vec![PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: window.mouse_position(),
            modifiers,
            click_count: 1,
        })],
    };
    for event in events {
        window.dispatch_event(event, cx);
        window.render_frame(cx);
    }
    Ok(())
}

const INK_CANVAS: &str = "ink-canvas";

fn drop_files(
    window: &mut Window,
    id: &str,
    paths: &[PathBuf],
    cx: &mut App,
) -> Result<(), StepError> {
    window.render_frame(cx);
    let at = clickable(window, id)?.bounds().center();
    let mut dragged = ExternalPaths::default();
    dragged.0.extend(paths.iter().cloned());
    let events = [
        FileDropEvent::Entered {
            position: at,
            paths: dragged,
        },
        FileDropEvent::Submit { position: at },
    ];
    for event in events {
        window.dispatch_event(PlatformInput::FileDrop(event), cx);
        window.render_frame(cx);
    }
    Ok(())
}

/// Hover styles and tooltips go, so a shot shows the resting state.
pub fn rest_pointer(window: &mut Window, cx: &mut App) {
    let away = point(gpui_kit::px(-10_000.), gpui_kit::px(-10_000.));
    let modifiers = window.modifiers();
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position: away,
            pressed_button: None,
            modifiers,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseExited(MouseExitEvent {
            position: away,
            pressed_button: None,
            modifiers,
        }),
        cx,
    );
}

pub fn resize_target(action: &str) -> Option<Size<Pixels>> {
    let (w, h) = action.strip_prefix("resize:")?.split_once('x')?;
    Some(gpui_kit::size(
        gpui_kit::px(w.parse().ok()?),
        gpui_kit::px(h.parse().ok()?),
    ))
}

const SCROLL_NOTCHES: usize = 60;

/// A half-hidden row's middle can sit under the list's clip, so wheel until `to` is wholly inside.
fn scroll(window: &mut Window, list: &str, to: &str, cx: &mut App) -> Result<(), StepError> {
    let mut last = None;
    for _ in 0..SCROLL_NOTCHES {
        window.render_frame(cx);
        let area = clickable(window, list)?.bounds();
        let target = matching(window, to);
        let Some(target) = target.first() else {
            // Not drawn: the feed is virtualized and draws only what is near the view. Wheel up
            // until it is.
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: area.center(),
                    delta: ScrollDelta::Lines(point(0., 3.)),
                    modifiers: window.modifiers(),
                    touch_phase: TouchPhase::Moved,
                    source: ScrollSource::Pointer,
                }),
                cx,
            );
            continue;
        };
        let b = target.bounds();
        let inside = b.origin.y >= area.origin.y
            && b.origin.y + b.size.height <= area.origin.y + area.size.height;
        if target.visible() && inside {
            return Ok(());
        }
        if last == Some(b.origin.y) {
            return if target.visible() {
                Ok(())
            } else {
                Err(StepError::OutOfReach {
                    list: list.into(),
                    to: to.into(),
                })
            };
        }
        last = Some(b.origin.y);
        // Towards the target: up while it is (partly) above the area, else down (GPUI's sign:
        // negative is a wheel turned towards the person). Always down would push a target cut
        // by the top edge out of view and back, and stop with it half drawn.
        let above = b.origin.y < area.origin.y;
        window.dispatch_event(
            PlatformInput::Scroll(ScrollEvent {
                position: area.center(),
                delta: ScrollDelta::Lines(point(0., if above { 1. } else { -3. })),
                modifiers: window.modifiers(),
                touch_phase: TouchPhase::Moved,
                source: ScrollSource::Pointer,
            }),
            cx,
        );
    }
    window.render_frame(cx);
    if matching(window, to).is_empty() {
        return Err(StepError::NotDrawn(to.into()));
    }
    Err(StepError::OutOfReach {
        list: list.into(),
        to: to.into(),
    })
}

/// Windows' default: one wheel notch scrolls three lines.
const LINES_PER_NOTCH: f32 = 3.;

/// `notches` turns of the wheel over the middle of `id`, a frame drawn after each, as a person
/// turns it. Positive is away from the person (up), negative towards (down): GPUI's sign.
fn wheel(window: &mut Window, id: &str, notches: i32, cx: &mut App) -> Result<(), StepError> {
    let step = LINES_PER_NOTCH * notches.signum() as f32;
    for _ in 0..notches.unsigned_abs() {
        window.render_frame(cx);
        let at = clickable(window, id)?.bounds().center();
        window.dispatch_event(
            PlatformInput::Scroll(ScrollEvent {
                position: at,
                delta: ScrollDelta::Lines(point(0., step)),
                modifiers: window.modifiers(),
                touch_phase: TouchPhase::Moved,
                source: ScrollSource::Pointer,
            }),
            cx,
        );
    }
    window.render_frame(cx);
    Ok(())
}

pub fn perform(
    shell: &Entity<Shell>,
    step: &WindowStep,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), StepError> {
    match step {
        WindowStep::Click(id) => click(window, id, cx)?,
        WindowStep::MoveTo(id) => move_to(window, id, cx)?,
        WindowStep::RightClick(id) => press(window, id, MouseButton::Right, cx)?,
        WindowStep::Type { text, into } => {
            if let Some(id) = into {
                click(window, id, cx)?;
            }
            window.input(text, cx);
        }
        WindowStep::Press(key) => {
            gpui_kit::Keystroke::parse_with_os(key, cx.platform_info().os)
                .map_err(|e| StepError::BadKey(key.clone(), e.to_string()))?;
            window.press(key, cx);
        }
        WindowStep::Scroll { list, to } => scroll(window, list, to, cx)?,
        WindowStep::Wheel { id, notches } => wheel(window, id, *notches, cx)?,
        WindowStep::Action(name) => action(shell, name, window, cx)?,
        WindowStep::Drag { id, by } => drag(window, id, *by, cx)?,
        WindowStep::Pen(step) => pen(window, *step, cx)?,
        WindowStep::Drop { id, paths } => drop_files(window, id, paths, cx)?,
        WindowStep::Paste { id, item } => {
            click(window, id, cx)?;
            cx.write_to_clipboard(item.clone());
            window.press("ctrl-v", cx);
        }
    }
    Ok(())
}

/// A GPUI action by name (`lobby::Quit`), or a host event: `idle`, `active`, `wake`,
/// `tray:show`, `tray:quit`, `window:close`, or `resize:<w>x<h>` (content size in logical pixels,
/// real app only, done by `pipe`).
fn action(
    shell: &Entity<Shell>,
    name: &str,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), StepError> {
    match name {
        // The injected idle source stays put, so the periodic check agrees with the step.
        "idle" => shell.update(cx, |s, cx| {
            s.set_idle_source(|| Some(IDLE_AFTER));
            s.check_idle(cx);
        }),
        "active" => shell.update(cx, |s, cx| {
            s.set_idle_source(|| Some(Duration::ZERO));
            s.check_idle(cx);
        }),
        "wake" => shell.update(cx, |s, cx| s.on_system_wake(cx)),
        // The tray's own menu and click reach the Shell as host events; the OS window is hidden
        // and shown through the same host as in the person's session. `window:close` is the close
        // button.
        "tray:show" => shell.update(cx, |s, cx| s.show_window(cx)),
        "tray:quit" => shell.update(cx, |s, cx| s.quit(cx)),
        "window:close" => shell.update(cx, |s, cx| s.on_close_requested(cx)),
        // The OS window is resized by the automation channel itself, outside any update
        // (`pipe`); in process there is no OS window to resize.
        _ if resize_target(name).is_some() => return Err(StepError::Unsupported(name.into())),
        _ if name.contains("::") => {
            let action = cx
                .build_action(name, None)
                .map_err(|_| StepError::UnknownAction(name.into()))?;
            window.dispatch_action(action, cx);
        }
        _ => return Err(StepError::UnknownAction(name.into())),
    }
    Ok(())
}

//! Linux: Close to tray minimizes the window and keeps it, so the Shell always has one. Show
//! replaces a window that Hide minimized with a new one for the same Shell. A draft in the
//! composer, the route and Voice are the Shell's and the Session's, not the window's, so they are
//! all still there; and the new window takes input.
#![cfg(target_os = "linux")]

mod common;

use common::scripted::{in_lobby, input, join_voice, settle_until};
use std::time::Duration;

use gpui_kit::TestAppContext;
use lobby_client::testing::frames;
use lobby_desktop::QuitState;
use lobby_desktop::automation::WindowStep;
use lobby_protocol::Presence;

#[gpui_kit::test]
fn show_after_hide_replaces_the_window_and_keeps_the_shell(cx: &mut TestAppContext) {
    let mut w = in_lobby(cx, &[("bob", Presence::Online)]);
    join_voice(cx, &mut w);
    input(
        cx,
        &w.o,
        WindowStep::Type {
            text: "a draft".into(),
            into: Some("composer-text".into()),
        },
    );
    let shell = w.o.shell.clone();
    let before = w.o.window;
    shell.update(cx, |s, _| {
        s.keep_in_tray(None);
        s.set_minimizer(|_| {});
    });

    shell.update(cx, |s, cx| s.on_close_requested(cx));
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| shell.read(cx).window()), before);
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        1,
        "Hide keeps the window"
    );

    shell.update(cx, |s, cx| s.show_window(cx));
    cx.run_until_parked();
    let now = cx.read(|cx| shell.read(cx).window());
    assert_ne!(now, before, "a new window");
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        1,
        "and the old one is gone"
    );
    w.o.window = now;

    // A window that Hide did not minimize is asked to come forward and replaced only if the
    // desktop does not (the test platform never activates one). A Hide in between cancels that.
    let wait = Duration::from_secs(1);
    shell.update(cx, |s, cx| s.show_window(cx));
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| shell.read(cx).window()), now, "still waiting");
    shell.update(cx, |s, cx| s.hide_to_tray(cx));
    cx.executor().advance_clock(wait);
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| shell.read(cx).window()),
        now,
        "Hide cancelled it"
    );

    // Hide, Show, Hide in quick succession ends hidden with one window.
    shell.update(cx, |s, cx| s.show_window(cx));
    shell.update(cx, |s, cx| s.hide_to_tray(cx));
    cx.run_until_parked();
    assert!(cx.read(|cx| shell.read(cx).hidden_to_tray()));
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);

    // Show, Show: the second takes over from the first, and one window is left.
    shell.update(cx, |s, cx| s.show_window(cx));
    shell.update(cx, |s, cx| s.show_window(cx));
    cx.executor().advance_clock(wait);
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 1, "one window");
    w.o.window = cx.read(|cx| shell.read(cx).window());

    input(
        cx,
        &w.o,
        WindowStep::Type {
            text: " and more".into(),
            into: Some("composer-text".into()),
        },
    );
    settle_until(cx, &w.o, "the draft and Voice as they were", |m| {
        m["route"] == "Lobby"
            && m["lobby"]["composer"] == "a draft and more"
            && m["lobby"]["voice"]["controls"] == "in_voice"
    });
}

/// The Lobby view's watch on its window moves with the window: a Message that came while the
/// replacement was not the active window is seen once it is.
#[gpui_kit::test]
fn the_replacement_marks_the_feed_seen_when_it_is_activated(cx: &mut TestAppContext) {
    let mut w = in_lobby(cx, &[("bob", Presence::Online)]);
    let shell = w.o.shell.clone();
    shell.update(cx, |s, _| {
        s.keep_in_tray(None);
        s.set_minimizer(|_| {});
    });
    shell.update(cx, |s, cx| s.on_close_requested(cx));
    cx.run_until_parked();
    shell.update(cx, |s, cx| s.show_window(cx));
    cx.run_until_parked();
    w.o.window = cx.read(|cx| shell.read(cx).window());

    let seen = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            let shell = shell.read(cx);
            let origin = shell.current_origin().expect("in a Lobby");
            shell.lobbies.read(cx).seen(origin)
        })
    };
    w.lobby.emit(frames::message(7, "bob", "hello", "n-7"));
    settle_until(cx, &w.o, "the Message in the feed", |m| {
        m["lobby"]["feed"].to_string().contains("hello")
    });
    assert_eq!(seen(cx), None, "the window is not the active one");

    w.o.window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    assert!(seen(cx).is_some(), "activating the window marks it seen");
}

/// Quit while a Show still waits for the desktop: no window opens during the teardown, and a link
/// that came with the Show is not opened.
#[gpui_kit::test]
fn quit_cancels_a_show_that_is_still_waiting(cx: &mut TestAppContext) {
    let w = in_lobby(cx, &[("bob", Presence::Online)]);
    let shell = w.o.shell.clone();
    let before = w.o.window;
    shell.update(cx, |s, _| {
        s.keep_in_tray(None);
        s.set_minimizer(|_| {});
    });

    shell.update(cx, |s, cx| s.show_window(cx));
    cx.run_until_parked();
    // The test platform makes a window active when it is asked to be; another window that is
    // activated takes that from it, so the desktop "does not bring it forward".
    let other = cx.add_empty_window();
    other.update(|window, _| window.activate_window());
    cx.run_until_parked();
    shell.update(cx, |s, cx| s.quit(cx));
    // The wait looks every 25 ms, a new timer each time: the clock moves in the same steps.
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(25));
        cx.run_until_parked();
    }

    assert_eq!(
        cx.read(|cx| shell.read(cx).window()),
        before,
        "no new window"
    );
    assert_eq!(cx.update(|cx| cx.windows().len()), 2, "and the other one");
    assert!(cx.read(|cx| shell.read(cx).quit != QuitState::Running));
}

/// The Member brings the minimized window back without Show (the dock, Alt+Tab): Lobby no longer
/// counts it as hidden, so the close button hides it again and a Show from the tray leaves it be.
#[gpui_kit::test]
fn a_window_brought_back_by_the_member_can_be_hidden_again(cx: &mut TestAppContext) {
    use std::cell::Cell;
    thread_local! {
        static MINIMIZED: Cell<u32> = const { Cell::new(0) };
    }
    let w = in_lobby(cx, &[("bob", Presence::Online)]);
    let shell = w.o.shell.clone();
    let window = w.o.window;
    shell.update(cx, |s, _| {
        s.keep_in_tray(None);
        s.set_minimizer(|_| MINIMIZED.with(|n| n.set(n.get() + 1)));
    });
    let minimized = || MINIMIZED.with(Cell::get);
    let hidden = |cx: &mut TestAppContext| cx.read(|cx| shell.read(cx).hidden_to_tray());

    shell.update(cx, |s, cx| s.on_close_requested(cx));
    cx.run_until_parked();
    assert_eq!((minimized(), hidden(cx)), (1, true));

    // Another window takes the activation; the Member then brings Lobby's back themselves.
    let other = cx.add_empty_window();
    other.update(|window, _| window.activate_window());
    cx.run_until_parked();
    window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    assert!(!hidden(cx), "the window is in plain sight");

    shell.update(cx, |s, cx| s.on_close_requested(cx));
    cx.run_until_parked();
    assert_eq!(minimized(), 2, "the close button hides it again");

    // Brought back once more: a Show from the tray does not replace a window in plain sight.
    let other = cx.add_empty_window();
    other.update(|window, _| window.activate_window());
    cx.run_until_parked();
    window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    shell.update(cx, |s, cx| s.show_window(cx));
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| shell.read(cx).window()),
        window,
        "no new window"
    );
}

/// A desktop that cannot minimize a window (sway) would leave a hidden window in plain sight, and
/// the Shell cannot be without one: the close button quits, as it does with no tray icon.
#[gpui_kit::test]
fn a_desktop_that_cannot_minimize_has_the_close_button_quit(cx: &mut TestAppContext) {
    use std::cell::Cell;
    thread_local! {
        static MINIMIZED: Cell<u32> = const { Cell::new(0) };
    }
    let w = in_lobby(cx, &[("bob", Presence::Online)]);
    let shell = w.o.shell.clone();
    let window = w.o.window;
    shell.update(cx, |s, _| {
        s.keep_in_tray(None);
        s.set_minimizer(|_| MINIMIZED.with(|n| n.set(n.get() + 1)));
        s.set_desktop_minimizes(|_| false);
    });
    assert!(cx.read(|cx| shell.read(cx).quit == QuitState::Running));

    // The desktop says what it can do when the window is activated.
    let other = cx.add_empty_window();
    other.update(|window, _| window.activate_window());
    cx.run_until_parked();
    window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();

    shell.update(cx, |s, cx| s.on_close_requested(cx));
    cx.run_until_parked();
    assert_eq!(MINIMIZED.with(Cell::get), 0, "nothing was minimized");
    assert!(cx.read(|cx| shell.read(cx).quit != QuitState::Running));
}

//! The "nothing moves" rules: the Voice controls keep their place through Join, Connecting and In
//! Voice, every Member row has one height and its name stays put while Voice connects, the feed,
//! composer, member list and Voice card keep their bounds through a lost connection and a failed
//! send, the sidebar and the scale. The Lobby is a `FakeLobby` the test speaks for.

mod common;

use common::{
    Opened,
    scripted::{
        Scripted, admit, click, dump, in_lobby, input, join_voice as join, leave_voice as leave,
        sent_until, settle_until, start_join, wait_for,
    },
    tokio_rt,
};
use gpui_kit::{
    AppContext as _, Bounds, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, PlatformInput,
    TestAppContext, VisualTestContext, px, size, test::TestWindowExt as _,
};
use lobby_client::testing::{FakeBehavior, FakeLobby, frames};
use lobby_desktop::{
    automation::{self, WindowStep},
    components::{BAR_CONTROL_H, FootSize, ROW_HEIGHT, TITLE_BAR_HEIGHT},
    kit::{Theme, WindowExt as _},
    lobby_view::{self, NARROW_BELOW},
};
use lobby_protocol::{ClientMsg, ErrorBody, ErrorCode, PROTOCOL_VERSION, Presence, ServerMsg};

/// Alice, with bob and dave online and Carol offline.
fn in_layout_lobby(cx: &mut TestAppContext) -> Scripted {
    in_lobby(
        cx,
        &[
            ("bob", Presence::Online),
            ("Carol", Presence::Offline),
            ("dave", Presence::Online),
        ],
    )
}

fn bounds(cx: &mut TestAppContext, o: &Opened, names: &[&'static str]) -> Vec<Bounds<Pixels>> {
    cx.update_window(o.window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let mut vcx = VisualTestContext::from_window(o.window, cx);
    names
        .iter()
        .map(|n| {
            vcx.debug_bounds(n)
                .unwrap_or_else(|| panic!("{n} is not drawn"))
        })
        .collect()
}

fn one(cx: &mut TestAppContext, o: &Opened, name: &'static str) -> Bounds<Pixels> {
    bounds(cx, o, &[name])[0]
}

/// A source's picture box is reserved from the first frame: the thumbnail that arrives later fills
/// it, and no tile, row or the list around them moves or changes size.
#[gpui_kit::test]
fn picker_boxes_are_reserved_so_thumbnails_move_nothing(cx: &mut TestAppContext) {
    use lobby_client::{ScreenSource, SourceId, synthetic_thumbnail};
    use lobby_desktop::components::{PickerModel, PickerView};

    cx.update(gpui_kit::init);
    let source = |id, name: &str| ScreenSource {
        id,
        name: name.to_owned(),
        detail: String::new(),
        blocked: false,
    };
    let sources = vec![
        source(SourceId::Monitor { hmonitor: 1 }, "Display 1"),
        source(SourceId::Monitor { hmonitor: 2 }, "Display 2"),
        source(SourceId::Window { hwnd: 1 }, "Editor"),
        source(SourceId::Window { hwnd: 2 }, "Terminal"),
    ];
    let window = cx.add_window(|window, cx| {
        PickerView::new(
            std::rc::Rc::new(|_, _, _, _| {}),
            lobby_protocol::Quality::UpTo2160,
            window,
            cx,
        )
    });
    let view = window.root(cx).unwrap();
    view.update(cx, |v, cx| {
        v.set_model(PickerModel::Sources(sources.clone()), cx)
    });
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    let layout = [
        "picker-screen-0",
        "picker-screen-1",
        "picker-window-0",
        "picker-window-1",
    ];
    let find = |cx: &mut VisualTestContext, id: &str| {
        cx.update(|window, _| automation::element_bounds(window, id))
    };
    let measure = |cx: &mut VisualTestContext| -> Vec<Bounds<Pixels>> {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        layout
            .iter()
            .map(|n| find(cx, n).unwrap_or_else(|| panic!("{n} is not drawn")))
            .collect()
    };
    let before = measure(cx);
    assert!(find(cx, "picker-screen-thumbnail-0").is_none());
    for id in [&sources[0].id, &sources[2].id] {
        let thumbnail = synthetic_thumbnail(id);
        view.update(cx, |v, cx| v.set_thumbnail(id, thumbnail, cx));
    }
    let after = measure(cx);
    assert_eq!(before, after, "no tile or row moved");
    for (thumb, host) in [
        ("picker-screen-thumbnail-0", "picker-screen-0"),
        ("picker-window-thumbnail-0", "picker-window-0"),
    ] {
        let thumb = find(cx, thumb).unwrap();
        let host = find(cx, host).unwrap();
        assert!(
            host.contains(&thumb.origin),
            "the picture sits inside its source"
        );
    }
    assert!(
        find(cx, "picker-screen-thumbnail-1").is_none(),
        "a source with no thumbnail keeps its icon"
    );
    let stale = SourceId::Window { hwnd: 99 };
    view.update(cx, |v, cx| {
        v.set_thumbnail(&stale, synthetic_thumbnail(&stale), cx)
    });
    assert!(!view.read_with(cx, |v, _| v.has_thumbnail(&stale)));
}

fn drawn(cx: &mut TestAppContext, o: &Opened, name: &'static str) -> bool {
    cx.update_window(o.window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    VisualTestContext::from_window(o.window, cx)
        .debug_bounds(name)
        .is_some()
}

fn controls(cx: &mut TestAppContext, o: &Opened) -> String {
    dump(cx, o)["lobby"]["voice"]["controls"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn resize(cx: &mut TestAppContext, o: &Opened, w: f32, h: f32) {
    cx.simulate_window_resize(o.window, size(px(w), px(h)));
    cx.run_until_parked();
}

fn inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

fn bob_is_in_voice(cx: &mut TestAppContext, w: &Scripted) {
    w.lobby.emit(frames::in_voice("bob", false, false, false));
    settle_until(cx, &w.o, "bob In Voice", |m| {
        m["lobby"]["members"]["bob"]["in_voice"] == true
    });
}

#[gpui_kit::test]
fn the_voice_controls_keep_their_place_in_every_state(cx: &mut TestAppContext) {
    let mut w = in_layout_lobby(cx);
    let o = &w.o;
    let idle = one(cx, o, "voice-controls");
    assert_eq!(idle.size.height, px(FootSize::Sheet.height()));
    let list_top = one(cx, o, "member-list").origin;

    bob_is_in_voice(cx, &w);
    assert_eq!(one(cx, &w.o, "voice-controls"), idle, "bob joined");

    let rid = start_join(cx, &mut w);
    assert_eq!(controls(cx, &w.o), "connecting");
    assert_eq!(one(cx, &w.o, "voice-controls"), idle, "connecting");
    admit(cx, &w, rid);
    assert_eq!(
        controls(cx, &w.o),
        "in_voice",
        "the row below is the In Voice one"
    );
    assert_eq!(one(cx, &w.o, "voice-controls"), idle, "In Voice");

    w.lobby.emit(frames::in_voice("dave", false, true, true));
    settle_until(cx, &w.o, "dave In Voice", |m| {
        m["lobby"]["members"]["dave"]["in_voice"] == true
    });
    assert_eq!(one(cx, &w.o, "voice-controls"), idle, "dave joined");
    assert_eq!(one(cx, &w.o, "member-list").origin, list_top);

    leave(cx, &mut w);
    assert_eq!(one(cx, &w.o, "voice-controls"), idle, "left");
}

#[gpui_kit::test]
fn every_member_row_has_one_height_and_its_name_stays_put_through_joining_voice(
    cx: &mut TestAppContext,
) {
    let w = in_layout_lobby(cx);
    let row = px(ROW_HEIGHT);
    let [me, bob, carol] = bounds(
        cx,
        &w.o,
        &["member-row-alice", "member-row-bob", "member-row-Carol"],
    )[..] else {
        unreachable!()
    };
    assert_eq!(
        (me.size.height, bob.size.height, carol.size.height),
        (row, row, row)
    );
    w.lobby.emit(frames::in_voice("dave", true, false, false));
    settle_until(cx, &w.o, "dave joining", |m| {
        m["lobby"]["members"]["dave"]["joining"] == true
    });
    assert_eq!(one(cx, &w.o, "member-row-dave").size.height, row);
    let joining = one(cx, &w.o, "member-name-dave");
    w.lobby.emit(frames::in_voice("dave", false, false, false));
    settle_until(cx, &w.o, "dave connected", |m| {
        m["lobby"]["members"]["dave"]["in_voice"] == true
            && m["lobby"]["members"]["dave"]["joining"] == false
    });
    assert_eq!(one(cx, &w.o, "member-name-dave"), joining, "connected");
    w.lobby.emit(frames::in_voice("dave", false, true, false));
    settle_until(cx, &w.o, "dave muted", |m| {
        m["lobby"]["members"]["dave"]["muted"] == true
    });
    assert_eq!(one(cx, &w.o, "member-row-dave").size.height, row, "muted");
    assert_eq!(one(cx, &w.o, "member-name-dave"), joining, "muted");
}

#[gpui_kit::test]
fn nothing_moves_through_a_lost_connection_a_failed_send_and_the_return(cx: &mut TestAppContext) {
    let mut w = in_layout_lobby(cx);
    const SURFACES: [&str; 4] = ["feed", "composer", "member-list", "voice-bar"];
    let before = bounds(cx, &w.o, &SURFACES);

    w.lobby.refuse_ws(true);
    w.lobby.drop_socket();
    settle_until(cx, &w.o, "the connection given up on", |m| {
        m["lobby"]["phase"] == "unavailable"
    });
    assert_eq!(bounds(cx, &w.o, &SURFACES), before, "lost");

    w.lobby.refuse_ws(false);
    click(cx, &w.o, "notification-bell");
    click(cx, &w.o, "connection-try-now");
    settle_until(cx, &w.o, "back online", |m| {
        m["lobby"]["phase"] == "connected"
    });
    click(cx, &w.o, "notification-bell");
    assert_eq!(bounds(cx, &w.o, &SURFACES), before, "back online");
    cx.update_window(w.o.window, |_, window, cx| {
        let type_it = WindowStep::Type {
            text: "refused".into(),
            into: Some("composer".into()),
        };
        automation::perform(&w.o.shell, &type_it, window, cx).unwrap();
        window.press("enter", cx);
    })
    .unwrap();
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(250));
    let rid = sent_until(cx, &mut w.lobby, "MESSAGE_SEND", |f| {
        matches!(f.msg, ClientMsg::MessageSend(_))
    })
    .rid
    .expect("MESSAGE_SEND has a rid");
    w.lobby.emit(ServerMsg::Error(ErrorBody::new(
        ErrorCode::RateLimited,
        "You are sending Messages too quickly",
        Some(rid),
    )));
    settle_until(cx, &w.o, "the refusal said", |m| {
        m["lobby"]["notifications"].as_array().unwrap().len() == 1
    });
    let toasts = cx
        .update_window(w.o.window, |_, window, cx| window.notifications(cx).len())
        .unwrap();
    assert_eq!(toasts, 1, "said as a toast");
    assert_eq!(bounds(cx, &w.o, &SURFACES), before, "failed send");
    cx.executor().advance_clock(lobby_view::RESTORE_AFTER_PAUSE);
    cx.run_until_parked();
    assert_eq!(bounds(cx, &w.o, &SURFACES), before, "the draft back");
}

/// Every auth screen fits the small window under the title bar, its rail, question and field on
/// one left edge.
#[gpui_kit::test]
fn every_auth_screen_fits_the_small_window_on_one_left_edge(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let rt = tokio_rt();
    let lobby = rt.block_on(FakeLobby::start(FakeBehavior::default()));
    let o = common::open_shell(cx, &rt);
    let check = |cx: &mut TestAppContext, screen: &str, field: &'static str| {
        for (w, h) in [(680., 480.), (480., 400.)] {
            resize(cx, &o, w, h);
            let names = ["auth", "auth-column", "auth-rail", "auth-title", field];
            let [area, column, rail, title, field] = bounds(cx, &o, &names)[..] else {
                unreachable!()
            };
            assert!(
                area.origin.y >= px(TITLE_BAR_HEIGHT),
                "{screen} {w}×{h}: the area starts under the title bar"
            );
            assert!(
                inside(column, area),
                "{screen} {w}×{h}: the whole column shows, {column:?} in {area:?}"
            );
            for (what, b) in [("question", title), ("field", field)] {
                assert_eq!(
                    b.origin.x, rail.origin.x,
                    "{screen} {w}×{h}: the {what} starts at the rail's left edge"
                );
            }
        }
    };
    check(cx, "Connect", "lobby-url");
    input(
        cx,
        &o,
        WindowStep::Type {
            text: lobby.origin().to_string(),
            into: Some("lobby-url".into()),
        },
    );
    click(cx, &o, "connect");
    settle_until(cx, &o, "the Email step", |m| m["auth"]["step"] == "Email");
    check(cx, "Email", "email");
}

#[gpui_kit::test]
fn the_default_window_shows_both_columns_and_a_narrow_one_docks_voice(cx: &mut TestAppContext) {
    let mut w = in_layout_lobby(cx);
    resize(cx, &w.o, 680., 480.);
    assert!(
        drawn(cx, &w.o, "member-list"),
        "the member list at the default size"
    );
    let [controls_at, feed] = bounds(cx, &w.o, &["voice-controls", "feed"])[..] else {
        unreachable!()
    };
    assert_eq!(controls_at.size.height, px(FootSize::Sheet.height()));
    assert!(
        controls_at.origin.x > feed.origin.x + feed.size.width,
        "in the sidebar"
    );
    assert!(!drawn(cx, &w.o, "voice-strip"));

    resize(cx, &w.o, NARROW_BELOW - 120., 400.);
    assert!(!drawn(cx, &w.o, "member-list"), "folded");
    let [strip, docked, composer] =
        bounds(cx, &w.o, &["voice-strip", "voice-controls", "composer"])[..]
    else {
        unreachable!()
    };
    assert!(inside(docked, strip), "the controls are in the strip");
    assert_eq!(docked.size.height, px(FootSize::Bar.height()));
    assert!(strip.origin.y < composer.origin.y, "above the composer");
    join(cx, &mut w);
    assert_eq!(controls(cx, &w.o), "in_voice");
    assert_eq!(one(cx, &w.o, "voice-strip"), strip);
    assert_eq!(one(cx, &w.o, "composer"), composer);
}

const ROWS: [&str; 4] = [
    "member-row-alice",
    "member-row-bob",
    "member-row-dave",
    "member-row-Carol",
];

fn bottom(b: Bounds<Pixels>) -> Pixels {
    b.origin.y + b.size.height
}

#[gpui_kit::test]
fn joining_and_leaving_voice_never_move_or_cover_a_row_outside_the_box(cx: &mut TestAppContext) {
    let mut w = in_layout_lobby(cx);
    assert!(
        !drawn(cx, &w.o, "voice-section"),
        "no box while nobody is in Voice"
    );
    let rows = bounds(cx, &w.o, &ROWS);
    let controls_at = one(cx, &w.o, "voice-controls");
    join(cx, &mut w);
    let box_in = one(cx, &w.o, "voice-section");
    let joined = bounds(cx, &w.o, &ROWS);
    assert_eq!(joined, rows, "alice was first already: no row moved");
    assert!(inside(joined[0], box_in), "my row is in the box");
    assert!(
        bottom(box_in) <= joined[1].origin.y,
        "the box stops above bob's row"
    );
    assert_eq!(one(cx, &w.o, "voice-controls"), controls_at);

    bob_is_in_voice(cx, &w);
    let box_two = one(cx, &w.o, "voice-section");
    assert_eq!(bounds(cx, &w.o, &ROWS), rows, "bob joins in his own place");
    assert!(
        inside(rows[0], box_two) && inside(rows[1], box_two),
        "both are in the box"
    );
    assert!(box_two.size.height > box_in.size.height, "the box grew");
    assert!(
        bottom(box_two) <= rows[2].origin.y,
        "it stops above dave's row"
    );

    leave(cx, &mut w);
    let [mine, bob, dave, carol] = bounds(cx, &w.o, &ROWS)[..] else {
        unreachable!()
    };
    let box_one = one(cx, &w.o, "voice-section");
    assert!(inside(bob, box_one), "bob is alone in the box");
    assert_eq!(bob, rows[0], "and first");
    assert_eq!(mine, rows[1], "I am the first of the rest");
    assert!(
        bottom(box_one) <= mine.origin.y,
        "the box does not reach my row below it"
    );
    assert_eq!([dave, carol], [rows[2], rows[3]], "the rows below stay");
    assert_eq!(one(cx, &w.o, "voice-controls"), controls_at);
}

#[gpui_kit::test]
fn going_idle_and_becoming_owner_move_no_row(cx: &mut TestAppContext) {
    let w = in_layout_lobby(cx);
    let rows = bounds(cx, &w.o, &ROWS);
    for (name, presence, word) in [
        ("bob", Presence::Idle, "idle"),
        ("dave", Presence::Dnd, "dnd"),
        ("alice", Presence::Idle, "idle"),
    ] {
        w.lobby.emit(ServerMsg::MemberUpsert {
            member: frames::member(name, presence),
        });
        settle_until(cx, &w.o, name, |m| {
            m["lobby"]["members"][name]["presence"] == word
        });
        assert_eq!(bounds(cx, &w.o, &ROWS), rows, "{name} {presence:?}");
    }
    for owner in ["alice", "bob"] {
        let mut lobby = FakeLobby::lobby_info(PROTOCOL_VERSION);
        lobby.owner = Some(frames::member(owner, Presence::Online).id);
        w.lobby.emit(ServerMsg::LobbyUpdate { lobby });
        settle_until(cx, &w.o, owner, |m| {
            m["lobby"]["members"][owner]["owner"] == true
        });
        assert_eq!(bounds(cx, &w.o, &ROWS), rows, "{owner} owns the Lobby");
    }
}

#[gpui_kit::test]
fn the_title_bar_controls_line_up(cx: &mut TestAppContext) {
    let w = in_layout_lobby(cx);
    let o = &w.o;
    for (width, h) in [(680., 480.), (480., 400.)] {
        resize(cx, o, width, h);
        cx.update_window(o.window, |_, window, cx| {
            window.render_frame(cx);
            let mut ids = vec!["lobby-switcher", "notification-bell", "settings-toggle"];
            if width < NARROW_BELOW {
                ids.push("members-toggle");
            }
            for id in ids {
                let b = window.find(id).bounds();
                assert_eq!(b.size.height, px(BAR_CONTROL_H), "{width}: {id} {b:?}");
                let middle = b.origin.y + b.size.height / 2.;
                assert!(
                    (middle - px(TITLE_BAR_HEIGHT / 2.)).abs() <= px(0.5),
                    "{width}: {id} sits off the bar's middle: {b:?}"
                );
            }
            let bell = window.find("notification-bell").bounds();
            assert_eq!(bell.size.width, px(BAR_CONTROL_H), "the bell is square");
            let cog = window.find("settings-toggle").bounds();
            assert_eq!(cog.size.width, px(BAR_CONTROL_H), "the cog is square");
            assert!(
                cog.origin.x >= bell.origin.x + bell.size.width,
                "{width}: the cog is right of the bell"
            );
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn a_long_lobby_name_never_pushes_the_bell_or_the_cog(cx: &mut TestAppContext) {
    let w = in_layout_lobby(cx);
    for (width, h) in [(680., 480.), (480., 400.)] {
        resize(cx, &w.o, width, h);
        let controls_at = |cx: &mut TestAppContext| {
            cx.update_window(w.o.window, |_, window, cx| {
                window.render_frame(cx);
                ["notification-bell", "settings-toggle"].map(|id| window.find(id).bounds())
            })
            .unwrap()
        };
        let before = controls_at(cx);
        let mut lobby = FakeLobby::lobby_info(PROTOCOL_VERSION);
        lobby.name = "A Lobby with a name far too long for any title bar".repeat(5);
        let name = lobby.name.clone();
        w.lobby.emit(ServerMsg::LobbyUpdate { lobby });
        settle_until(cx, &w.o, "the long name", |m| {
            m["lobby"]["name"] == name.as_str()
        });
        assert_eq!(controls_at(cx), before, "{width}: the controls stay put");
        for b in before {
            assert!(
                b.origin.x + b.size.width <= px(width),
                "{width}: {b:?} is inside the window"
            );
        }
    }
}

fn sidebar_step(cx: &mut TestAppContext, o: &Opened, step: WindowStep) {
    cx.update_window(o.window, |_, window, cx| {
        automation::perform(&o.shell, &step, window, cx).unwrap();
    })
    .unwrap();
    cx.run_until_parked();
}

fn drag_edge(cx: &mut TestAppContext, o: &Opened, dx: f32) {
    let id = "sidebar-edge".to_owned();
    sidebar_step(cx, o, WindowStep::Drag { id, by: (dx, 0.) });
}

fn double_click_edge(cx: &mut TestAppContext, o: &Opened) {
    cx.update_window(o.window, |_, window, cx| {
        window.render_frame(cx);
        let at = automation::element_bounds(window, "sidebar-edge")
            .expect("the edge is drawn")
            .center();
        let modifiers = window.modifiers();
        for click_count in [1, 2] {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers,
                    click_count,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers,
                    click_count,
                }),
                cx,
            );
            window.render_frame(cx);
        }
    })
    .unwrap();
    cx.run_until_parked();
}

fn press(cx: &mut TestAppContext, o: &Opened, key: &str) {
    cx.update_window(o.window, |_, window, cx| window.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}

fn element(cx: &mut TestAppContext, o: &Opened, id: &str) -> Option<Bounds<Pixels>> {
    cx.update_window(o.window, |_, window, cx| {
        window.render_frame(cx);
        automation::element_bounds(window, id)
    })
    .unwrap()
}

fn sidebar_w(cx: &mut TestAppContext, o: &Opened) -> f32 {
    f32::from(element(cx, o, "members").expect("docked").size.width)
}

fn saved_width(cx: &mut TestAppContext, o: &Opened) -> Option<f32> {
    cx.read(|cx| o.shell.read(cx).lobbies.read(cx).sidebar_width())
}

#[gpui_kit::test]
fn the_sidebar_is_dragged_within_the_window(cx: &mut TestAppContext) {
    let w = in_layout_lobby(cx);
    let o = &w.o;
    resize(cx, o, 680., 480.);
    let members = element(cx, o, "members").unwrap();
    let edge = element(cx, o, "sidebar-edge").unwrap();
    let feed = one(cx, o, "feed");
    assert_eq!(members.size.width, px(lobby_view::SIDEBAR_SLIM_W));
    assert!(inside(edge, members), "the band lies in the sidebar");
    assert!(
        feed.origin.x + feed.size.width <= edge.origin.x,
        "and none of it over the feed"
    );
    assert_eq!(saved_width(cx, o), None);

    drag_edge(cx, o, -100.);
    assert_eq!(sidebar_w(cx, o), 320.);
    let wider = one(cx, o, "feed");
    assert_eq!(
        wider.size.width,
        feed.size.width - px(100.),
        "the feed gives way"
    );
    assert_eq!(saved_width(cx, o), Some(320.));
    let toasts = cx.read(|cx| Theme::global(cx).notification.margins.right);
    assert_eq!(
        toasts,
        px(320. + lobby_view::SHEET_INSET + 16.),
        "toasts sit beside the wider sidebar"
    );

    drag_edge(cx, o, -200.);
    assert_eq!(
        sidebar_w(cx, o),
        680. - lobby_view::MAIN_MIN - lobby_view::SHEET_INSET,
        "the chat keeps its measure"
    );
    resize(cx, o, 1100., 700.);
    drag_edge(cx, o, -200.);
    assert_eq!(sidebar_w(cx, o), lobby_view::SIDEBAR_MAX);
    drag_edge(cx, o, 200.);
    assert_eq!(sidebar_w(cx, o), lobby_view::SIDEBAR_MIN);
    drag_edge(cx, o, -180.);
    assert_eq!(sidebar_w(cx, o), 400.);

    resize(cx, o, 650., 480.);
    assert_eq!(
        sidebar_w(cx, o),
        650. - lobby_view::MAIN_MIN - lobby_view::SHEET_INSET,
        "clamped to the window"
    );
    resize(cx, o, NARROW_BELOW - 10., 480.);
    assert!(
        !drawn(cx, o, "member-list") && element(cx, o, "sidebar-edge").is_none(),
        "folded"
    );
    resize(cx, o, 1100., 700.);
    assert_eq!(sidebar_w(cx, o), 400., "the chosen width comes back");

    press(cx, o, "ctrl-alt-right");
    assert_eq!(sidebar_w(cx, o), 400. - lobby_view::SIDEBAR_STEP);
    press(cx, o, "ctrl-alt-left");
    press(cx, o, "ctrl-alt-left");
    assert_eq!(sidebar_w(cx, o), 400. + lobby_view::SIDEBAR_STEP);
    assert_eq!(saved_width(cx, o), Some(416.));

    double_click_edge(cx, o);
    assert_eq!(sidebar_w(cx, o), lobby_view::SIDEBAR_W, "the default again");
    assert_eq!(saved_width(cx, o), None);
}

fn set_scale(cx: &mut TestAppContext, o: &Opened, percent: u16) {
    let lobbies = cx.read(|cx| o.shell.read(cx).lobbies.clone());
    cx.update(|cx| lobbies.update(cx, |l, cx| l.update_settings(|s| s.ui_scale = percent, cx)));
    cx.run_until_parked();
}

fn ui_scale(cx: &mut TestAppContext, o: &Opened) -> u16 {
    cx.read(|cx| o.shell.read(cx).lobbies.read(cx).settings().ui_scale)
}

#[gpui_kit::test]
fn the_size_keys_step_the_scale_to_its_ends_and_back_to_100(cx: &mut TestAppContext) {
    let w = in_layout_lobby(cx);
    let o = &w.o;
    assert_eq!(ui_scale(cx, o), 100);
    for (key, percent) in [
        ("ctrl-=", 110),
        ("ctrl-=", 120),
        ("ctrl--", 110),
        ("ctrl-0", 100),
        ("ctrl-+", 110),
        ("ctrl-shift-=", 120),
        ("ctrl-0", 100),
    ] {
        press(cx, o, key);
        assert_eq!(ui_scale(cx, o), percent, "{key}");
    }
    for _ in 0..9 {
        press(cx, o, "ctrl-=");
    }
    assert_eq!(ui_scale(cx, o), 150, "the top stops");
    for _ in 0..9 {
        press(cx, o, "ctrl--");
    }
    assert_eq!(ui_scale(cx, o), 80, "and so does the bottom");
    let font = cx.read(|cx| Theme::global(cx).font_size);
    assert_eq!(
        font,
        px(12.8),
        "the theme's font size is the rem size every length follows"
    );
}

#[gpui_kit::test]
fn a_bigger_scale_folds_the_sidebar_sooner(cx: &mut TestAppContext) {
    let w = in_layout_lobby(cx);
    let o = &w.o;
    // The rule is in design pixels: the window's width over the scale.
    for (window_w, percent, sidebar) in [
        (680., 100, Some(220.)),
        (680., 125, None),
        (680., 80, Some(220. * 0.8)),
        (900., 150, Some(220. * 1.5)),
        (899., 150, None),
        (1100., 150, Some(220. * 1.5)),
        (1400., 150, Some(264. * 1.5)),
        (1400., 100, Some(264.)),
        (480., 80, Some(220. * 0.8)),
        (480., 100, None),
    ] {
        set_scale(cx, o, percent);
        resize(cx, o, window_w, 480.);
        let shown = drawn(cx, o, "member-list").then(|| sidebar_w(cx, o));
        match (shown, sidebar) {
            (Some(got), Some(want)) => {
                assert!(
                    (got - want).abs() < 0.5,
                    "{window_w} at {percent}%: {got} not {want}"
                )
            }
            (got, want) => assert_eq!(got, want, "{window_w} at {percent}%"),
        }
        let feed = one(cx, o, "feed");
        let window = f32::from(feed.origin.x + feed.size.width);
        assert!(
            window <= window_w,
            "{window_w} at {percent}%: the feed leaves the window"
        );
    }
}

#[gpui_kit::test]
fn the_lobby_surfaces_stay_inside_the_window_at_both_heights(cx: &mut TestAppContext) {
    let w = in_layout_lobby(cx);
    for (seq, who, text, nonce) in [
        (1, "bob", "First from bob", "n1"),
        (2, "bob", "Second from bob", "n2"),
        (3, "alice", "Alice answers", "n3"),
    ] {
        w.lobby.emit(frames::message(seq, who, text, nonce));
    }
    wait_for(cx, "the Messages", |cx| {
        dump(cx, &w.o)["lobby"]["feed"].as_array().unwrap().len() == 3
    });
    for window_h in [480., 400.] {
        resize(cx, &w.o, 680., window_h);
        for id in ["feed", "composer", "member-list", "voice-bar"] {
            assert!(
                inside(
                    one(cx, &w.o, id),
                    Bounds::new(
                        gpui_kit::point(px(0.), px(0.)),
                        size(px(680.), px(window_h))
                    )
                ),
                "{id} stays inside at {window_h}"
            );
        }
    }
}

fn collapsed(cx: &mut TestAppContext, o: &Opened) -> bool {
    cx.read(|cx| o.shell.read(cx).lobbies.read(cx).sidebar_collapsed())
}

#[gpui_kit::test]
fn the_sidebar_is_put_away_past_its_minimum_and_comes_back_at_its_last_width(
    cx: &mut TestAppContext,
) {
    let w = in_layout_lobby(cx);
    let o = &w.o;
    let click_toggle = |cx: &mut TestAppContext| {
        cx.update_window(o.window, |_, window, cx| {
            let step = WindowStep::Click("members-toggle".into());
            automation::perform(&o.shell, &step, window, cx).unwrap();
        })
        .unwrap();
        cx.run_until_parked();
    };
    resize(cx, o, 1100., 700.);
    drag_edge(cx, o, -60.);
    assert_eq!(sidebar_w(cx, o), 324.);
    assert!(
        element(cx, o, "members-toggle").is_none(),
        "docked: the list already says who is here"
    );
    let feed = one(cx, o, "feed");

    // A drag that overshoots the minimum by a hair keeps the sidebar.
    drag_edge(cx, o, 104.);
    assert_eq!(sidebar_w(cx, o), lobby_view::SIDEBAR_MIN);
    assert!(!collapsed(cx, o));
    drag_edge(cx, o, 10.);
    assert_eq!(
        sidebar_w(cx, o),
        lobby_view::SIDEBAR_MIN,
        "within the slack"
    );
    drag_edge(cx, o, -104.);
    assert_eq!(sidebar_w(cx, o), 324.);

    // Well past it, the sidebar goes: no icon rail, the feed takes the room, Voice moves to the
    // strip, and the title bar's button is how it comes back.
    drag_edge(cx, o, 150.);
    assert!(collapsed(cx, o));
    assert_eq!(
        saved_width(cx, o),
        Some(lobby_view::SIDEBAR_MIN),
        "the width it had at the edge of the minimum is kept"
    );
    assert!(
        !drawn(cx, o, "members") && !drawn(cx, o, "voice-bar"),
        "nothing is left of it"
    );
    assert!(drawn(cx, o, "voice-strip") && element(cx, o, "members-toggle").is_some());
    assert!(
        one(cx, o, "feed").size.width > feed.size.width,
        "the feed has the room"
    );
    resize(cx, o, 900., 700.);
    assert!(collapsed(cx, o), "narrower, still put away");
    resize(cx, o, 1100., 700.);

    click_toggle(cx);
    assert!(!collapsed(cx, o));
    assert_eq!(sidebar_w(cx, o), lobby_view::SIDEBAR_MIN, "its last width");
    assert!(
        element(cx, o, "members-toggle").is_none(),
        "docked again: the button goes"
    );

    // The keyboard: stepping past the minimum puts it away, widening brings it back.
    press(cx, o, "ctrl-alt-right");
    assert!(collapsed(cx, o), "one step past the minimum");
    press(cx, o, "ctrl-alt-right");
    assert!(collapsed(cx, o));
    press(cx, o, "ctrl-alt-left");
    assert!(!collapsed(cx, o));
    assert_eq!(
        sidebar_w(cx, o),
        lobby_view::SIDEBAR_MIN,
        "at the width it left"
    );

    // A window too narrow to dock folds it into the sheet, put away or not.
    press(cx, o, "ctrl-alt-right");
    resize(cx, o, NARROW_BELOW - 10., 480.);
    assert!(element(cx, o, "members-toggle").is_some() && !drawn(cx, o, "member-list"));
    click_toggle(cx);
    assert!(
        drawn(cx, o, "member-list"),
        "narrow: the button opens the sheet"
    );
    assert!(collapsed(cx, o), "and what was put away stays put away");
}

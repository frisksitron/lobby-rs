//! A signed-in Shell whose Lobby is a `FakeLobby` the test speaks for: a real WebSocket and real
//! HTTP over loopback, the real `Client` and Session. The test pushes the frames the Lobby would
//! send (`lobby.emit`) and reads the frames the app sent (`lobby.sent`). For what a real server
//! cannot be made to say: a refusal with a chosen code, a lost connection at a chosen moment.
//!
//! Real sockets wake the test scheduler, so these tests allow parking and wait on a condition
//! (`settle_until`) where a fake connection could just run the executor.

use std::{rc::Rc, time::Duration};

use gpui_kit::{AppContext as _, TestAppContext};
use lobby_client::{
    Client, HttpPolicy, MemoryTokenStore, Persisted, TokenStore as _,
    testing::{FakeBehavior, FakeLobby, fast_reconnects, tokens_for_tests},
};
use lobby_desktop::automation;
use lobby_protocol::Presence;

use super::{Opened, open_shell_with_client, state_file, tokio_rt};

pub struct Scripted {
    pub o: Opened,
    pub lobby: FakeLobby,
    _rt: tokio::runtime::Runtime,
}

/// Alice, signed in at a `FakeLobby` where `others` are, Connected.
pub fn in_lobby(cx: &mut TestAppContext, others: &[(&str, Presence)]) -> Scripted {
    in_lobby_with(
        cx,
        FakeBehavior {
            members: others.iter().map(|(n, p)| ((*n).to_owned(), *p)).collect(),
            me: "alice".into(),
            ..FakeBehavior::default()
        },
    )
}

pub fn in_lobby_with(cx: &mut TestAppContext, behavior: FakeBehavior) -> Scripted {
    cx.executor().allow_parking();
    let rt = tokio_rt();
    let lobby = rt.block_on(FakeLobby::start(behavior));
    let origin = lobby.origin();
    let dir = Rc::new(tempfile::tempdir().unwrap());
    let mut saved = Persisted::default();
    saved.upsert(
        &origin,
        &FakeLobby::lobby_info(lobby_protocol::PROTOCOL_VERSION),
    );
    saved.last_active = Some(origin.clone());
    state_file(dir.path()).write(1, &saved).unwrap();
    let store = MemoryTokenStore::new();
    store.save(&origin, &tokens_for_tests("alice")).unwrap();
    let client = Client::new(
        rt.handle().clone(),
        std::sync::Arc::new(store.clone()),
        HttpPolicy::LoopbackOnly,
    )
    .unwrap()
    .with_reconnect_policy(fast_reconnects());
    let o = open_shell_with_client(cx, &rt, client, dir, store);
    let world = Scripted { o, lobby, _rt: rt };
    settle_until(cx, &world.o, "the Lobby Connected", |m| {
        m["lobby"]["phase"] == "connected"
    });
    world
}

/// The window's model (`automation::dump`) after a frame.
pub fn dump(cx: &mut TestAppContext, o: &Opened) -> serde_json::Value {
    let shell = o.shell.clone();
    cx.update_window(o.window, |_, window, cx| {
        gpui_kit::test::TestWindowExt::render_frame(window, cx);
        automation::dump(&shell, window, cx)
    })
    .unwrap()
}

/// Runs the executor and the real clock until `cond` holds of the model, read once the window
/// shows every Intent it dispatched. Never advances the app's clock: a test does that where it
/// means to.
pub fn settle_until(
    cx: &mut TestAppContext,
    o: &Opened,
    what: &str,
    cond: impl Fn(&serde_json::Value) -> bool,
) {
    for _ in 0..1000 {
        settle(cx, o);
        if cond(&dump(cx, o)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let model = dump(cx, o);
    panic!("timed out waiting for {what}: {}", model["lobby"]);
}

/// Runs the executor until the window shows the snapshot that took every Intent it dispatched:
/// the Session's driver takes them on a Tokio thread.
pub fn settle(cx: &mut TestAppContext, o: &Opened) {
    for _ in 0..5000 {
        cx.run_until_parked();
        if cx.read(|cx| super::caught_up(&o.shell, cx)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the window never showed the Intents it dispatched");
}

/// Click `id`, as a person would.
pub fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.window, |_, window, cx| {
        gpui_kit::test::TestWindowExt::render_frame(window, cx);
        gpui_kit::test::TestWindowExt::click(window, gpui_kit::SharedString::from(id), cx);
    })
    .unwrap();
    settle(cx, o);
}

/// One event of the person's, in an update of its own like a platform event: nothing the
/// Session publishes is taken in between, as when the window falls behind.
pub fn input(cx: &mut TestAppContext, o: &Opened, step: automation::WindowStep) {
    let shell = o.shell.clone();
    cx.update_window(o.window, |_, window, cx| {
        gpui_kit::test::TestWindowExt::render_frame(window, cx);
        automation::perform(&shell, &step, window, cx).unwrap();
    })
    .unwrap();
}

/// Join Voice: the Lobby is asked, and until it answers the controls say Connecting.
pub fn start_join(cx: &mut TestAppContext, w: &mut Scripted) -> lobby_protocol::Rid {
    click(cx, &w.o, "voice-join");
    let join = sent_until(cx, &mut w.lobby, "VOICE_JOIN", |f| {
        matches!(f.msg, lobby_protocol::ClientMsg::VoiceJoin(_))
    });
    join.rid.expect("VOICE_JOIN has a rid")
}

/// The Lobby takes alice into Voice.
pub fn admit(cx: &mut TestAppContext, w: &Scripted, rid: lobby_protocol::Rid) {
    use lobby_client::testing::frames;
    w.lobby.emit(frames::voice_joined(rid.as_str(), 3));
    w.lobby.emit(frames::in_voice("alice", false, false, false));
    settle_until(cx, &w.o, "In Voice", |m| {
        m["lobby"]["voice"]["controls"] == "in_voice"
    });
}

pub fn join_voice(cx: &mut TestAppContext, w: &mut Scripted) {
    let rid = start_join(cx, w);
    admit(cx, w, rid);
}

pub fn leave_voice(cx: &mut TestAppContext, w: &mut Scripted) {
    use lobby_client::testing::frames;
    click(cx, &w.o, "voice-leave");
    sent_until(cx, &mut w.lobby, "VOICE_LEAVE", |f| {
        matches!(f.msg, lobby_protocol::ClientMsg::VoiceLeave)
    });
    w.lobby.emit(frames::not_in_voice("alice"));
    settle_until(cx, &w.o, "out of Voice", |m| {
        m["lobby"]["voice"]["controls"] == "join"
    });
}

/// Every frame the app sends, up to a frame `want` accepts, waiting for that one. Frames on a
/// socket arrive in order, so the frames before it are all the app sent before it: a test that
/// claims a frame was not sent waits for the one that must come after. Frames that arrive in the
/// same read as the match come too.
pub fn sent_through(
    cx: &mut TestAppContext,
    lobby: &mut FakeLobby,
    what: &str,
    want: impl Fn(&lobby_protocol::ClientFrame) -> bool,
) -> Vec<lobby_protocol::ClientFrame> {
    let mut frames = Vec::new();
    for _ in 0..1000 {
        cx.run_until_parked();
        frames.extend(lobby.sent());
        if frames.iter().any(&want) {
            return frames;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the app never sent {what}: {frames:?}");
}

/// The first frame the app sends that `want` accepts, waiting for it. Frames before it are
/// dropped from the lobby's record.
pub fn sent_until(
    cx: &mut TestAppContext,
    lobby: &mut FakeLobby,
    what: &str,
    want: impl Fn(&lobby_protocol::ClientFrame) -> bool,
) -> lobby_protocol::ClientFrame {
    sent_through(cx, lobby, what, &want)
        .into_iter()
        .find(|f| want(f))
        .expect("sent_through returns once a frame matches")
}

/// As [`settle_until`] for a condition on something other than the model.
pub fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut cond: impl FnMut(&mut TestAppContext) -> bool,
) {
    for _ in 0..1000 {
        cx.run_until_parked();
        if cond(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

//! The Session's driver at its handle: Intents in, the newest snapshot and the news out, while
//! the connection never stops bringing events and the window may not keep up. The WebSocket is a
//! fake one, REST a `FakeLobby`, and media the real engine on fake devices.

use std::{sync::Arc, time::Duration};

use lobby_client::{
    AudioSource, COMMAND_DEADLINE, ChangeSync, ConnEvent, History, Intent, MediaConfig, MediaError,
    Phase, SessionEvent, SessionHandle, Snapshot, UiEffect, Updates, VoicePhase,
    ink::PenPoint,
    testing::{FakeBehavior, FakeConn, FakeLobby, frames, inspect, scripted_handle},
};
use lobby_protocol::{
    ClientMsg, ErrorBody, ErrorCode, Message, MessageChanges, MessageId, MessagePage, Presence,
    ReadyHistory, ServerMsg, ShareId, UnixMs,
};
use tokio::{sync::Notify, task::JoinHandle};

/// Real waits that only a broken driver runs into.
const WAIT: Duration = Duration::from_secs(5);

/// mallory's Session run by its driver, Connected, with what it published so far.
struct Driven {
    handle: SessionHandle,
    updates: Updates,
    snapshot: Arc<Snapshot>,
    news: Vec<UiEffect>,
    fake: FakeConn,
    _lobby: FakeLobby,
}

impl Driven {
    /// Her own share 5 is on the roster with drawing on, so her pen draws on it.
    async fn connected() -> Driven {
        let lobby = FakeLobby::start(FakeBehavior::default()).await;
        let (handle, updates, fake) = scripted_handle(&lobby);
        let ready = frames::ready("mallory", &[("bob", Presence::Online)]);
        fake.emit(ConnEvent::Msg(ready));
        fake.emit(ConnEvent::Phase(Phase::Connected));
        fake.emit(ConnEvent::Msg(frames::sharing_ink("mallory", 5, Some(1))));
        let mut d = Driven {
            handle,
            updates,
            snapshot: Arc::default(),
            news: Vec::new(),
            fake,
            _lobby: lobby,
        };
        d.until("Connected with history and the share", |s| {
            s.state.history == History::Loaded && s.state.pen_share().is_some()
        })
        .await;
        d
    }

    fn take(&mut self) {
        let update = self.updates.take();
        self.snapshot = update.snapshot;
        self.news.extend(update.news.0);
    }

    async fn until(&mut self, what: &str, cond: impl Fn(&Snapshot) -> bool) {
        let waited = tokio::time::timeout(WAIT, async {
            while !cond(&self.snapshot) {
                assert!(self.updates.changed().await, "the Session ended");
                self.take();
            }
        })
        .await;
        assert!(waited.is_ok(), "timed out waiting for {what}");
    }

    async fn act(&mut self, intent: Intent) {
        let n = self.handle.dispatch(intent);
        self.until("the Intent taken", |s| s.intents >= n).await;
    }

    /// The connection drops, and the next one brings `ready`.
    async fn back_with(&mut self, ready: ServerMsg) {
        self.fake.emit(ConnEvent::Disconnected { code: None });
        self.fake
            .emit(ConnEvent::Phase(Phase::Connecting { attempt: 0 }));
        self.until("away", |s| s.state.phase != Phase::Connected)
            .await;
        self.fake.emit(ConnEvent::Msg(ready));
        self.fake.emit(ConnEvent::Phase(Phase::Connected));
    }

    /// In Voice as session 7: the Lobby took the Join.
    async fn in_voice(&mut self) {
        self.act(Intent::JoinVoice {
            media: Box::new(MediaConfig::fake()),
            volumes: Default::default(),
        })
        .await;
        let join = tokio::time::timeout(WAIT, async {
            loop {
                let sent = self.fake.sent();
                if let Some(rid) = sent
                    .into_iter()
                    .find(|f| matches!(f.msg, ClientMsg::VoiceJoin(_)))
                    .and_then(|f| f.rid)
                {
                    return rid;
                }
                assert!(self.updates.changed().await, "the Session ended");
                self.take();
            }
        })
        .await
        .expect("VOICE_JOIN went out");
        self.fake
            .emit(ConnEvent::Msg(frames::voice_joined(join.as_str(), 7)));
        self.fake.emit(ConnEvent::Msg(frames::in_voice(
            "mallory", false, false, false,
        )));
        self.until("In Voice", |s| {
            matches!(s.state.voice.phase, VoicePhase::InVoice { .. })
        })
        .await;
    }
}

/// Bob going online and offline as fast as the Session's channel takes it, until aborted: the
/// queue is never empty when the driver looks, and the state does not grow.
fn flood(fake: &FakeConn) -> JoinHandle<()> {
    let events = fake.sender();
    tokio::spawn(async move {
        for i in 0u64.. {
            let presence = if i % 2 == 0 {
                Presence::Offline
            } else {
                Presence::Online
            };
            let member = frames::member("bob", presence);
            let ev = SessionEvent::Conn(ConnEvent::Msg(ServerMsg::MemberUpsert { member }));
            if events.send(ev).await.is_err() {
                return;
            }
        }
    })
}

fn share() -> ShareId {
    ShareId::new(5).unwrap()
}

/// A READY whose history carries no Message: `after` for a client that holds the newest, else
/// the feed starts over.
fn ready(last_message_seq: u64, last_change_rev: u64, after: bool) -> ServerMsg {
    let ServerMsg::Ready(mut ready) = frames::ready("mallory", &[("bob", Presence::Online)]) else {
        unreachable!("frames::ready is a READY")
    };
    ready.last_message_seq = last_message_seq;
    ready.last_change_rev = last_change_rev;
    let empty = MessagePage {
        messages: vec![],
        has_more: false,
    };
    ready.history = if after {
        ReadyHistory::After(empty)
    } else {
        ReadyHistory::Latest(empty)
    };
    ServerMsg::Ready(ready)
}

fn bobs(id: i64, content: &str) -> Message {
    let nonce = format!("bob-nonce-{id:08}");
    let ServerMsg::MessageCreate { message } = frames::message(id, "bob", content, &nonce) else {
        unreachable!("frames::message is a MESSAGE_CREATE")
    };
    message
}

fn feed(s: &Snapshot) -> Vec<(i64, String)> {
    let chat = s.state.chat.values();
    chat.map(|m| (m.id.get(), m.content.clone())).collect()
}

/// mallory is away while bob edits one of his Messages and deletes another. Back, her feed shows
/// the edit and not the deleted one, and the Lobby's newest page, read before the changes, does
/// not undo them when she starts the feed over.
#[tokio::test]
async fn back_from_away_the_feed_shows_what_was_edited_and_deleted_meanwhile() {
    let cat = bobs(2, "teh cat");
    let fixed = Message {
        content: "the cat".into(),
        edited_at: Some(UnixMs(cat.created_at.0 + 5_000)),
        rev: Some(1),
        ..cat.clone()
    };
    let lobby = FakeLobby::start(FakeBehavior {
        history: vec![bobs(1, "hi"), cat, bobs(3, "oops, wrong Lobby")],
        changes: MessageChanges {
            updated: vec![fixed],
            removed: vec![MessageId::new(3).unwrap()],
        },
        ..FakeBehavior::default()
    })
    .await;
    let (handle, updates, fake) = scripted_handle(&lobby);
    let mut d = Driven {
        handle,
        updates,
        snapshot: Arc::default(),
        news: Vec::new(),
        fake,
        _lobby: lobby,
    };
    d.fake.emit(ConnEvent::Msg(ready(3, 0, false)));
    d.fake.emit(ConnEvent::Phase(Phase::Connected));
    d.until("the newest page", |s| {
        s.state.history == History::Loaded && s.state.chat.len() == 3
    })
    .await;
    let away = || ConnEvent::Phase(Phase::Connecting { attempt: 0 });
    let gone = |s: &Snapshot| s.state.phase != Phase::Connected;

    d.fake.emit(ConnEvent::Disconnected { code: None });
    d.fake.emit(away());
    d.until("away", gone).await;
    d.fake.emit(ConnEvent::Msg(ready(3, 2, true)));
    d.fake.emit(ConnEvent::Phase(Phase::Connected));
    d.until("the edit and the delete caught up", |s| {
        s.state.changes.sync() == ChangeSync::Synced(2)
    })
    .await;
    let right = [(1, "hi".to_owned()), (2, "the cat".to_owned())];
    assert_eq!(feed(&d.snapshot), right);

    d.fake.emit(ConnEvent::Disconnected { code: None });
    d.fake.emit(away());
    d.until("away again", gone).await;
    // Away long enough that the Lobby starts the feed over.
    d.fake.emit(ConnEvent::Msg(ready(3, 2, false)));
    d.fake.emit(ConnEvent::Phase(Phase::Connected));
    d.until("the newest page again", |s| {
        s.state.phase == Phase::Connected && s.state.history == History::Loaded
    })
    .await;
    assert_eq!(feed(&d.snapshot), right, "the older page did not undo them");
}

/// mallory's catch-up on bob's fix fails, and he edits another Message while she is here. Her
/// next READY still asks for the fix, which the live edit came after.
#[tokio::test]
async fn a_failed_catch_up_is_asked_again_after_a_live_edit() {
    let (hi, cat) = (bobs(1, "hi"), bobs(2, "teh cat"));
    let fixed = Message {
        content: "the cat".into(),
        edited_at: Some(UnixMs(cat.created_at.0 + 5_000)),
        rev: Some(1),
        ..cat.clone()
    };
    let gate = Arc::new(Notify::new());
    let lobby = FakeLobby::start(FakeBehavior {
        history: vec![hi.clone(), cat],
        changes: MessageChanges {
            updated: vec![fixed],
            removed: vec![],
        },
        changes_failures: 1,
        changes_gate: Some(gate.clone()),
        ..FakeBehavior::default()
    })
    .await;
    let (handle, updates, fake) = scripted_handle(&lobby);
    let mut d = Driven {
        handle,
        updates,
        snapshot: Arc::default(),
        news: Vec::new(),
        fake,
        _lobby: lobby,
    };
    d.fake.emit(ConnEvent::Msg(ready(2, 0, false)));
    d.fake.emit(ConnEvent::Phase(Phase::Connected));
    d.until("the newest page", |s| {
        s.state.history == History::Loaded && s.state.chat.len() == 2
    })
    .await;
    let catching_up =
        |s: &Snapshot| matches!(s.state.changes.sync(), ChangeSync::CatchingUp { .. });

    d.back_with(ready(2, 1, true)).await;
    d.until("catching up", catching_up).await;
    gate.notify_one();
    d.until("the catch-up failed", |s| !catching_up(s)).await;
    let live = Message {
        content: "hi!".into(),
        edited_at: Some(UnixMs(hi.created_at.0 + 9_000)),
        rev: Some(2),
        ..hi
    };
    d.fake
        .emit(ConnEvent::Msg(ServerMsg::MessageUpdate { message: live }));
    d.until("the live edit", |s| feed(s)[0].1 == "hi!").await;

    d.back_with(ready(2, 2, true)).await;
    gate.notify_one();
    let both = [(1, "hi!".to_owned()), (2, "the cat".to_owned())];
    d.until("the fix caught up", |s| feed(s) == both).await;
}

#[tokio::test]
async fn a_send_times_out_on_time_while_events_and_the_pen_never_stop() {
    let mut d = Driven::connected().await;
    // Only the test moves the clock from here on, a millisecond a pen move.
    tokio::time::pause();
    let t0 = tokio::time::Instant::now();
    d.act(Intent::Send("hello".into())).await;
    assert_eq!(d.snapshot.state.pending.len(), 1, "on its way");
    let at = PenPoint::new(0.1, 0.1);
    d.act(Intent::PenDown { share: share(), at }).await;
    assert!(d.snapshot.pen_down);
    let flood = flood(&d.fake);
    let failed_after = loop {
        tokio::time::advance(Duration::from_millis(1)).await;
        let moved = (t0.elapsed().as_millis() % 1000) as f32 / 1000.;
        d.handle.dispatch(Intent::PenTo(PenPoint::new(moved, 0.5)));
        d.take();
        if d.news
            .iter()
            .any(|e| matches!(e, UiEffect::SendFailed { .. }))
        {
            break t0.elapsed();
        }
        d.news.clear();
        assert!(
            t0.elapsed() < COMMAND_DEADLINE * 2,
            "the send never timed out"
        );
    };
    flood.abort();
    assert!(
        failed_after >= COMMAND_DEADLINE
            && failed_after <= COMMAND_DEADLINE + Duration::from_millis(5),
        "failed {failed_after:?} after the send, its deadline is {COMMAND_DEADLINE:?}"
    );
    assert!(d.snapshot.state.pending.is_empty());
    assert!(d.snapshot.pen_down, "the stroke went on all along");
}

/// `VOICE_JOIN` goes out while the devices open, so a microphone that does not open leaves Voice
/// again, and the Issue says why.
#[tokio::test]
async fn a_microphone_that_does_not_open_leaves_voice_again() {
    let mut d = Driven::connected().await;
    let media = MediaConfig {
        source: AudioSource::Broken(MediaError::DeviceNotFound),
        ..MediaConfig::fake()
    };
    d.act(Intent::JoinVoice {
        media: Box::new(media),
        volumes: Default::default(),
    })
    .await;
    d.until("Voice ended", |s| s.state.voice.phase == VoicePhase::Idle)
        .await;
    let sent: Vec<ClientMsg> = d.fake.sent().into_iter().map(|f| f.msg).collect();
    let join = sent
        .iter()
        .position(|m| matches!(m, ClientMsg::VoiceJoin(_)));
    let leave = sent.iter().position(|m| matches!(m, ClientMsg::VoiceLeave));
    assert!(
        matches!((join, leave), (Some(j), Some(l)) if j < l),
        "VOICE_JOIN, then VOICE_LEAVE: {sent:?}"
    );
    assert!(
        d.news.iter().any(|e| matches!(e, UiEffect::Issue(_))),
        "an Issue says why"
    );
}

#[tokio::test]
async fn leave_and_quit_end_the_call_at_once_under_a_flood_with_the_window_behind() {
    let mut d = Driven::connected().await;
    d.in_voice().await;
    d.fake.sent();
    let flood = flood(&d.fake);
    // The window takes no update from here on.
    d.handle.dispatch(Intent::LeaveVoice);
    let (running, phase) = inspect(&d.handle, |s| {
        (s.voice_media_running(), s.state.voice.phase)
    })
    .await;
    assert!(!running, "the media stopped with no answer from the Lobby");
    assert_eq!(phase, VoicePhase::Idle);
    assert!(
        d.fake
            .sent()
            .iter()
            .any(|f| matches!(f.msg, ClientMsg::VoiceLeave)),
        "VOICE_LEAVE went out"
    );
    let shutdown = tokio::time::timeout(WAIT, d.handle.shutdown()).await;
    assert!(
        matches!(shutdown, Ok(Ok(()))),
        "the shutdown finished: {shutdown:?}"
    );
    assert!(d.fake.is_cancelled(), "the connection closed");
    flood.abort();
}

/// Well under the 1.5 s a media thread that never stops is given.
const AT_ONCE: Duration = Duration::from_millis(1000);

/// A quit while the event queue is full: the teardown takes the events that still come, so the
/// device thread waiting for room to say the devices opened gets to its stop.
#[tokio::test]
async fn a_quit_behind_a_full_event_queue_ends_at_once() {
    let d = Driven::connected().await;
    let Driven {
        mut handle, fake, ..
    } = d;
    let events = fake.sender();
    let bob = || {
        let member = frames::member("bob", Presence::Online);
        SessionEvent::Conn(ConnEvent::Msg(ServerMsg::MemberUpsert { member }))
    };
    // One runtime thread: the driver takes nothing until the test awaits.
    while events.try_send(bob()).is_ok() {}
    handle.dispatch(Intent::JoinVoice {
        media: Box::new(MediaConfig::fake()),
        volumes: Default::default(),
    });
    let shutdown = tokio::time::timeout(AT_ONCE, handle.shutdown()).await;
    assert!(
        matches!(shutdown, Ok(Ok(()))),
        "the shutdown finished at once: {shutdown:?}"
    );
    assert!(fake.is_cancelled(), "the connection closed");
}

/// A send the Lobby's throttle refused waits for its retry, and the connection drops meanwhile:
/// nothing is due while disconnected, so the driver sleeps instead of turning on a deadline it
/// cannot act on once it passes.
#[tokio::test]
async fn a_throttled_send_waits_without_a_deadline_while_disconnected() {
    let mut d = Driven::connected().await;
    d.act(Intent::Send("hi".into())).await;
    let rid = d
        .fake
        .sent()
        .into_iter()
        .find(|f| matches!(f.msg, ClientMsg::MessageSend(_)))
        .and_then(|f| f.rid)
        .expect("the send went out");
    d.fake.emit(ConnEvent::Msg(ServerMsg::Error(ErrorBody {
        code: ErrorCode::RateLimited,
        message: "slow down".into(),
        detail: None,
        rid: Some(rid),
        retry_after_ms: Some(1_000),
    })));
    d.fake.emit(ConnEvent::Phase(Phase::Reconnecting {
        attempt: 1,
        max: 5,
        retry_in: Duration::from_secs(1),
    }));
    d.until("disconnected with the send queued", |s| {
        matches!(s.state.phase, Phase::Reconnecting { .. })
            && s.state.pending.iter().any(|p| p.queued)
    })
    .await;
    let due = inspect(&d.handle, |s| s.next_deadline()).await;
    assert_eq!(due, None, "nothing is due while disconnected");
}

#[tokio::test]
async fn dropping_the_handle_ends_the_session_and_its_connection() {
    let mut d = Driven::connected().await;
    d.in_voice().await;
    let flood = flood(&d.fake);
    let Driven {
        handle,
        mut updates,
        fake,
        ..
    } = d;
    drop(handle);
    let ended = tokio::time::timeout(WAIT, async { while updates.changed().await {} }).await;
    assert!(ended.is_ok(), "the driver ended");
    assert!(fake.is_cancelled(), "the connection closed");
    flood.abort();
}

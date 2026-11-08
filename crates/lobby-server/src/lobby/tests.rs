use futures_util::FutureExt as _;
use lobby_protocol::{
    AuthorRef, InkBatch, InkPoint, MessageId, Quality, StrokeId, UnixMs, Username, VoiceSessionId,
};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::sfu::{SfuEventKind, SfuInbox};

fn profile(name: &str) -> Profile {
    Profile {
        id: UserId::random(),
        username: Username::parse(name).unwrap(),
        email: format!("{name}@lobby.test").parse().unwrap(),
        avatar: None,
        created_at: UnixMs(1),
    }
}

const INK_TEST_QUEUE: usize = 16;

struct TestConn {
    id: ConnId,
    rx: mpsc::Receiver<Utf8Bytes>,
    ink: InkQueue,
    kick: Kick,
}

impl TestConn {
    fn frames(&mut self) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        while let Ok(f) = self.rx.try_recv() {
            out.push(ServerMsg::decode(f.as_str()).unwrap());
        }
        out
    }
}

fn no_history() -> HistoryRead {
    HistoryRead {
        history: ReadyHistory::Latest(lobby_protocol::MessagePage {
            messages: Vec::new(),
            has_more: false,
        }),
        rev: 0,
    }
}

fn connect(lobby: &Lobby, user: &UserId, id: u64, queue: usize) -> TestConn {
    connect_as(lobby, user, id, queue, Presence::Online)
}

fn connect_as(lobby: &Lobby, user: &UserId, id: u64, queue: usize, presence: Presence) -> TestConn {
    let (tx, rx) = mpsc::channel(queue);
    let kick = Kick::child_of(&CancellationToken::new());
    let ink = InkQueue::new(INK_TEST_QUEUE);
    lobby
        .register(
            user,
            Conn {
                id: ConnId(id),
                session: SessionId(id as i64),
                tx,
                ink: ink.clone(),
                kick: kick.clone(),
                auth_epoch: lobby.auth_epoch(),
            },
            presence,
            no_history(),
        )
        .unwrap();
    TestConn {
        id: ConnId(id),
        rx,
        ink,
        kick,
    }
}

fn join_voice(lobby: &Lobby, user: &UserId, conn: &TestConn, sfu: &mut SfuInbox) -> VoiceSessionId {
    let rid: Rid = "j1".parse().unwrap();
    let join = VoiceCommand::Join {
        muted: false,
        deafened: false,
    };
    lobby.voice(user, conn.id, Some(rid), join, Instant::now());
    let Some(SfuCmd {
        vsid,
        op: SfuOp::Join { .. },
        ..
    }) = sfu.try_recv()
    else {
        panic!("the SFU was asked to join");
    };
    let connected = SfuEvent {
        user: user.clone(),
        vsid,
        kind: SfuEventKind::Connected,
    };
    lobby.on_sfu_event(connected, Instant::now());
    vsid
}

fn voice_of(frames: &[ServerMsg], name: &str) -> Vec<Option<lobby_protocol::VoiceState>> {
    frames
        .iter()
        .filter_map(|f| match f {
            ServerMsg::MemberUpsert { member } if member.username.as_str() == name => {
                Some(member.voice)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_replaced_connection_gets_voice_ended_before_4009_and_the_new_one_stays() {
    let (alice, bob) = (profile("alice"), profile("bob"));
    let (lobby, mut sfu) = test_lobby(&[alice.clone(), bob.clone()]);
    let mut b = connect(&lobby, &bob.id, 1, 64);
    b.frames();
    let vsid = join_voice(&lobby, &bob.id, &b, &mut sfu);
    b.frames();
    let mut b2 = connect(&lobby, &bob.id, 2, 64);
    let old = b.frames();
    assert!(
        matches!(old.last(), Some(ServerMsg::VoiceEnded(e)) if e.reason == VoiceEndReason::Replaced && e.voice_session_id == vsid),
        "{old:?}"
    );
    assert_eq!(b.kick.reason(), CloseReason::Replaced);
    assert_eq!(
        sfu.try_recv(),
        Some(SfuCmd {
            user: bob.id.clone(),
            vsid,
            op: SfuOp::Leave
        })
    );
    let ServerMsg::Ready(ready) = &b2.frames()[0] else {
        panic!()
    };
    assert_eq!(
        ready.members.iter().find(|m| m.id == bob.id).unwrap().voice,
        None
    );
    lobby.unregister(&bob.id, b.id);
    assert_eq!(lobby.counts().connections, 1);
    let _ = join_voice(&lobby, &bob.id, &b2, &mut sfu);
    lobby.unregister(&bob.id, b.id);
    assert_eq!(lobby.counts().in_voice, 1);
}

fn register_with(lobby: &Lobby, user: &UserId, id: u64, epoch: u64) -> Result<(), RegisterError> {
    let (tx, _rx) = mpsc::channel(16);
    lobby.register(
        user,
        Conn {
            id: ConnId(id),
            session: SessionId(id as i64),
            tx,
            ink: InkQueue::new(INK_TEST_QUEUE),
            kick: Kick::child_of(&CancellationToken::new()),
            auth_epoch: epoch,
        },
        Presence::Online,
        no_history(),
    )
}

/// A session revoked between IDENTIFY's auth lookup and its registration never registers; a
/// lookup that starts after the revocation does (even with a reused session rowid), and a
/// revocation of the whole User covers all its sessions.
#[test]
fn a_revocation_after_the_auth_lookup_refuses_the_registration() {
    let (alice, bob) = (profile("alice"), profile("bob"));
    let (lobby, _sfu) = test_lobby(&[alice.clone(), bob.clone()]);
    let looked_up = lobby.auth_epoch();
    lobby.kick_session(SessionId(5));
    assert_eq!(
        register_with(&lobby, &alice.id, 5, looked_up),
        Err(RegisterError::Revoked)
    );
    assert_eq!(
        register_with(&lobby, &alice.id, 5, lobby.auth_epoch()),
        Ok(())
    );
    assert_eq!(
        register_with(&lobby, &bob.id, 6, looked_up),
        Ok(()),
        "another session"
    );

    let looked_up = lobby.auth_epoch();
    lobby.kick_user(&bob.id);
    assert_eq!(
        register_with(&lobby, &bob.id, 7, looked_up),
        Err(RegisterError::Revoked)
    );
    assert!(!lobby.set_session(&bob.id, ConnId(6), SessionId(8), looked_up));
    assert!(lobby.set_session(&alice.id, ConnId(5), SessionId(5), looked_up));
    assert_eq!(
        register_with(&lobby, &bob.id, 7, lobby.auth_epoch()),
        Ok(())
    );
}

fn share_screen(
    lobby: &Lobby,
    user: &UserId,
    conn: &TestConn,
    vsid: VoiceSessionId,
    sfu: &mut SfuInbox,
) {
    let quality = Quality::UpTo1440;
    let start = VoiceCommand::ShareStart {
        audio: false,
        quality,
    };
    lobby.voice(user, conn.id, None, start, Instant::now());
    let cmd = sfu.try_recv();
    assert!(
        cmd.as_ref()
            .is_some_and(|c| c.op == SfuOp::StartShare { quality }),
        "the share's quality reaches the SFU: {cmd:?}"
    );
    let started = SfuEvent {
        user: user.clone(),
        vsid,
        kind: SfuEventKind::ShareVideoStarted,
    };
    lobby.on_sfu_event(started, Instant::now());
}

/// Alice is In Voice and shares her screen, Bob watches, Carol is only connected. Removing her
/// ends her Voice first (the watcher gets the share-ended upsert), then removes her for
/// everyone else and closes her connection with 4006.
#[test]
fn member_removed_ends_voice_then_removes_then_closes_4006() {
    let (alice, bob, carol) = (profile("alice"), profile("bob"), profile("carol"));
    let (lobby, mut sfu) = test_lobby(&[bob.clone(), alice.clone(), carol.clone()]);
    let mut a = connect(&lobby, &alice.id, 1, 64);
    let mut b = connect(&lobby, &bob.id, 2, 64);
    let mut c = connect(&lobby, &carol.id, 3, 64);
    let vsid = join_voice(&lobby, &alice.id, &a, &mut sfu);
    join_voice(&lobby, &bob.id, &b, &mut sfu);
    share_screen(&lobby, &alice.id, &a, vsid, &mut sfu);
    let watch = VoiceCommand::Watch {
        sharer: alice.id.clone(),
    };
    lobby.voice(&bob.id, b.id, None, watch, Instant::now());
    assert!(matches!(
        sfu.try_recv(),
        Some(SfuCmd {
            op: SfuOp::Watch { .. },
            ..
        })
    ));
    let sharing = |frames: &[ServerMsg]| {
        voice_of(frames, "alice")
            .last()
            .copied()
            .flatten()
            .and_then(|v| v.screen_share)
    };
    assert!(sharing(&b.frames()).is_some(), "bob saw alice's share");
    a.frames();
    c.frames();
    let looked_up = lobby.auth_epoch();

    assert!(lobby.member_removed(&alice.id));

    let mine = a.frames();
    assert!(
        matches!(mine.last(), Some(ServerMsg::VoiceEnded(e))
            if e.reason == VoiceEndReason::Kicked && e.voice_session_id == vsid),
        "alice's last frame is her Voice ending: {mine:?}"
    );
    assert!(
        !mine
            .iter()
            .any(|f| matches!(f, ServerMsg::MemberRemoved(_))),
        "alice never hears her own removal: {mine:?}"
    );
    assert_eq!(a.kick.reason(), CloseReason::Removed);
    assert_eq!(CloseReason::Removed.code(), 4006);
    for (who, frames) in [("bob", b.frames()), ("carol", c.frames())] {
        let upsert = frames
            .iter()
            .position(|f| matches!(f, ServerMsg::MemberUpsert { member } if member.id == alice.id))
            .unwrap_or_else(|| panic!("{who} sees alice leave Voice: {frames:?}"));
        let ServerMsg::MemberUpsert { member } = &frames[upsert] else {
            unreachable!()
        };
        assert_eq!(member.voice, None, "{who}: the share and Voice ended");
        let removed = frames
            .iter()
            .position(|f| matches!(f, ServerMsg::MemberRemoved(r) if r.user_id == alice.id))
            .unwrap_or_else(|| panic!("{who} hears MEMBER_REMOVED: {frames:?}"));
        assert!(upsert < removed, "{who}: Voice ends first: {frames:?}");
        assert_eq!(
            removed,
            frames.len() - 1,
            "{who}: nothing after: {frames:?}"
        );
    }
    assert_eq!(
        sfu.try_recv(),
        Some(SfuCmd {
            user: alice.id.clone(),
            vsid,
            op: SfuOp::Leave
        })
    );

    lobby.unregister(&alice.id, a.id);
    assert!(b.frames().is_empty() && c.frames().is_empty());
    assert_eq!(
        lobby.counts(),
        Counts {
            members: 2,
            connections: 2,
            online: 2,
            in_voice: 1,
        }
    );
    assert_eq!(
        register_with(&lobby, &alice.id, 9, lobby.auth_epoch()),
        Err(RegisterError::NotAMember)
    );
    lobby.vouched_in(
        &AccessRequestId::random(),
        &bob.id,
        UnixMs(2),
        alice.clone(),
    );
    assert_eq!(
        register_with(&lobby, &alice.id, 9, looked_up),
        Err(RegisterError::Revoked)
    );
}

fn message(seq: i64, author: &Profile) -> Message {
    Message {
        id: MessageId::new(seq).unwrap(),
        author: AuthorRef {
            id: author.id.clone(),
            username: author.username.to_string(),
            avatar: None,
            deactivated: false,
        },
        content: format!("{seq}"),
        attachments: Vec::new(),
        nonce: format!("nonce-{seq}"),
        created_at: UnixMs(seq),
        edited_at: None,
        rev: None,
    }
}

#[test]
fn concurrent_broadcasts_reach_every_connection_in_one_order() {
    let people: Vec<Profile> = (0..4).map(|i| profile(&format!("user{i}"))).collect();
    let (lobby, _sfu) = test_lobby(&people);
    let mut conns: Vec<TestConn> = people
        .iter()
        .enumerate()
        .map(|(i, p)| connect(&lobby, &p.id, i as u64 + 1, 4096))
        .collect();
    for c in &mut conns {
        c.frames();
    }
    std::thread::scope(|s| {
        for (t, author) in people.iter().enumerate() {
            let lobby = lobby.clone();
            s.spawn(move || {
                for i in 0..200 {
                    lobby.broadcast_message(&message((t * 1000 + i + 1) as i64, author));
                }
            });
        }
    });
    let orders: Vec<Vec<i64>> = conns
        .iter_mut()
        .map(|c| {
            c.frames()
                .into_iter()
                .filter_map(|f| match f {
                    ServerMsg::MessageCreate { message } => Some(message.id.get()),
                    _ => None,
                })
                .collect()
        })
        .collect();
    assert_eq!(orders[0].len(), 800);
    assert!(
        orders.iter().all(|o| *o == orders[0]),
        "O1: one order for all"
    );
}

/// Bob's reader is stalled: nothing drains his reliable queue (8 frames) or his ink queue. Carol's
/// 200 batches overflow only the ink queue, so he stays connected in Voice and Chat still
/// reaches him.
#[test]
fn a_flood_of_ink_at_a_stalled_watcher_drops_ink_and_never_kicks() {
    let (alice, bob, carol) = (profile("alice"), profile("bob"), profile("carol"));
    let (lobby, mut sfu) = test_lobby(&[alice.clone(), bob.clone(), carol.clone()]);
    let mut a = connect(&lobby, &alice.id, 1, 64);
    let mut b = connect(&lobby, &bob.id, 2, 8);
    let mut c = connect(&lobby, &carol.id, 3, 64);
    let vsid = join_voice(&lobby, &alice.id, &a, &mut sfu);
    b.frames();
    join_voice(&lobby, &bob.id, &b, &mut sfu);
    b.frames();
    join_voice(&lobby, &carol.id, &c, &mut sfu);
    b.frames();
    share_screen(&lobby, &alice.id, &a, vsid, &mut sfu);
    b.frames();
    for (who, conn) in [(&bob, &b), (&carol, &c)] {
        let watch = VoiceCommand::Watch {
            sharer: alice.id.clone(),
        };
        lobby.voice(&who.id, conn.id, None, watch, Instant::now());
    }
    let rid: Rid = "d1".parse().unwrap();
    let on = VoiceCommand::SetDrawing(true);
    lobby.voice(&alice.id, a.id, Some(rid), on, Instant::now());
    let share = a
        .frames()
        .iter()
        .rev()
        .find_map(|f| match f {
            ServerMsg::MemberUpsert { member } if member.id == alice.id => {
                member.voice.and_then(|v| v.screen_share)
            }
            _ => None,
        })
        .expect("alice's share");
    let epoch = share.drawing.expect("drawing is on");
    b.frames();
    c.frames();

    for seq in 0..200 {
        let batch = InkBatch {
            share_id: share.share_id,
            epoch,
            stroke: StrokeId(1),
            seq,
            prev: None,
            points: vec![InkPoint { x: 1, y: 2 }],
            end: false,
        };
        lobby.voice(
            &carol.id,
            c.id,
            None,
            VoiceCommand::Ink(batch),
            Instant::now(),
        );
    }

    let mut waiting = Vec::new();
    while let Some(frame) = b.ink.try_pop() {
        match ServerMsg::decode(frame.as_str()).unwrap() {
            ServerMsg::Ink(relay) => waiting.push((relay.author_id, relay.batch.seq)),
            other => panic!("{other:?}"),
        }
    }
    let newest: Vec<u32> = (200 - INK_TEST_QUEUE as u32..200).collect();
    assert_eq!(
        waiting.iter().map(|(_, seq)| *seq).collect::<Vec<_>>(),
        newest,
        "the oldest batches made room"
    );
    assert!(waiting.iter().all(|(author, _)| *author == carol.id));
    assert_eq!(
        a.ink.try_pop().map(|_| ()),
        Some(()),
        "the Sharer gets ink too"
    );
    assert_eq!(c.ink.try_pop(), None, "never back to the author");

    lobby.broadcast_message(&message(1, &alice));
    assert!(
        b.kick.kicked().now_or_never().is_none(),
        "ink never kicks the reader"
    );
    let reliable = b.frames();
    assert!(
        matches!(reliable.as_slice(), [ServerMsg::MessageCreate { .. }]),
        "Chat arrives and no ink rides the reliable queue: {reliable:?}"
    );
    assert_eq!(lobby.counts().in_voice, 3);
}

mod ready_fold {
    //! The READY-fold law: a connection that folded every event since its READY sees exactly
    //! what a fresh connection's READY says.

    use std::collections::{BTreeMap, HashMap};

    use proptest::prelude::*;

    use super::*;

    const PEOPLE: usize = 4;

    #[derive(Debug, Clone)]
    enum Op {
        Connect(usize),
        Disconnect(usize),
        Presence(usize, Presence),
        Join(usize),
        MediaConnected(usize),
        Mute(usize, bool),
        Speaking(usize, bool),
        Leave(usize),
        Message(usize),
    }

    fn presence() -> impl Strategy<Value = Presence> {
        prop_oneof![
            Just(Presence::Online),
            Just(Presence::Idle),
            Just(Presence::Dnd),
            Just(Presence::Offline)
        ]
    }

    fn op() -> impl Strategy<Value = Op> {
        let who = 0..PEOPLE;
        prop_oneof![
            who.clone().prop_map(Op::Connect),
            who.clone().prop_map(Op::Disconnect),
            (who.clone(), presence()).prop_map(|(i, p)| Op::Presence(i, p)),
            who.clone().prop_map(Op::Join),
            who.clone().prop_map(Op::MediaConnected),
            (who.clone(), any::<bool>()).prop_map(|(i, m)| Op::Mute(i, m)),
            (who.clone(), any::<bool>()).prop_map(|(i, m)| Op::Speaking(i, m)),
            who.clone().prop_map(Op::Leave),
            who.prop_map(Op::Message),
        ]
    }

    fn fold(state: &mut BTreeMap<UserId, Member>, frames: Vec<ServerMsg>) {
        for f in frames {
            match f {
                ServerMsg::Ready(r) => {
                    *state = r
                        .members
                        .iter()
                        .map(|m| (m.id.clone(), m.clone()))
                        .collect();
                }
                ServerMsg::MemberUpsert { member } => {
                    state.insert(member.id.clone(), member);
                }
                ServerMsg::VoiceSpeaking(s) => {
                    if let Some(v) = state.get_mut(&s.user_id).and_then(|m| m.voice.as_mut()) {
                        v.speaking = s.speaking;
                    }
                }
                _ => {}
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn a_folded_view_equals_a_fresh_ready(ops in prop::collection::vec(op(), 1..60)) {
            let mut people: Vec<Profile> =
                (0..PEOPLE).map(|i| profile(&format!("person{i}"))).collect();
            let observer = profile("observer");
            let fresh = profile("fresh");
            people.push(observer.clone());
            people.push(fresh.clone());
            let (lobby, mut sfu) = test_lobby(&people);
            let mut watch = connect(&lobby, &observer.id, 1000, 4096);
            let mut view = BTreeMap::new();
            fold(&mut view, watch.frames());
            let mut conns: HashMap<usize, TestConn> = HashMap::new();
            let mut vsids: HashMap<usize, VoiceSessionId> = HashMap::new();
            let mut next_conn = 1;
            let now = Instant::now();
            for op in ops {
                match op {
                    Op::Connect(i) => {
                        next_conn += 1;
                        conns.insert(i, connect(&lobby, &people[i].id, next_conn, 4096));
                    }
                    Op::Disconnect(i) => {
                        if let Some(c) = conns.remove(&i) {
                            lobby.unregister(&people[i].id, c.id);
                        }
                    }
                    Op::Presence(i, p) => {
                        if let Some(c) = conns.get(&i) {
                            lobby.set_presence(&people[i].id, c.id, p);
                        }
                    }
                    Op::Join(i) | Op::Mute(i, _) | Op::Leave(i) => {
                        let Some(c) = conns.get(&i) else { continue };
                        let cmd = match op {
                            Op::Join(_) => VoiceCommand::Join { muted: false, deafened: false },
                            Op::Mute(_, m) => VoiceCommand::SetState { muted: m, deafened: false },
                            _ => VoiceCommand::Leave,
                        };
                        lobby.voice(&people[i].id, c.id, Some("r1".parse().unwrap()), cmd, now);
                    }
                    Op::MediaConnected(i) | Op::Speaking(i, _) => {
                        if let Some(vsid) = vsids.get(&i) {
                            let kind = match op {
                                Op::Speaking(_, s) => SfuEventKind::Speaking(s),
                                _ => SfuEventKind::Connected,
                            };
                            let ev = SfuEvent { user: people[i].id.clone(), vsid: *vsid, kind };
                            lobby.on_sfu_event(ev, now);
                        }
                    }
                    Op::Message(i) => {
                        let seq = i64::try_from(next_conn).unwrap() + 10;
                        lobby.broadcast_message(&message(seq, &people[i]));
                    }
                }
                while let Some(cmd) = sfu.try_recv() {
                    if let SfuCmd { user, vsid, op: SfuOp::Join { .. } } = cmd {
                        let i = people.iter().position(|p| p.id == user).unwrap();
                        vsids.insert(i, vsid);
                    }
                }
                fold(&mut view, watch.frames());
            }
            let mut late = connect(&lobby, &fresh.id, 9999, 4096);
            fold(&mut view, watch.frames());
            let mut truth = BTreeMap::new();
            fold(&mut truth, late.frames());
            prop_assert_eq!(view, truth);
        }
    }
}

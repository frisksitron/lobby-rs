//! The voice machine's table (one test per transition) and proptest invariants over random input
//! sequences.

use lobby_protocol::{InkPoint, RtcTrack, StrokeId, TrackPurpose};
use proptest::prelude::*;

use super::*;

fn user(n: u8) -> UserId {
    format!("usr_{:024x}", u128::from(n) + 1).parse().unwrap()
}

fn caps(n: u32) -> VoiceConfig {
    VoiceConfig {
        max_participants: n,
        max_sharers: 4,
    }
}

fn rid(s: &str) -> Option<Rid> {
    Some(s.parse().unwrap())
}

fn cmd(u: &UserId, conn: u64, rid_: &str, cmd: VoiceCommand) -> VoiceInput {
    VoiceInput::Cmd {
        user: u.clone(),
        conn: ConnId(conn),
        rid: rid(rid_),
        cmd,
    }
}

fn join() -> VoiceCommand {
    VoiceCommand::Join {
        muted: false,
        deafened: false,
    }
}

fn vsid(n: u64) -> VoiceSessionId {
    VoiceSessionId::new(n).unwrap()
}

fn frames(fx: &[VoiceEffect]) -> Vec<(&'static str, Option<String>)> {
    fx.iter()
        .filter_map(|e| match e {
            VoiceEffect::ToConn { msg, .. } => {
                let rid = match msg {
                    ServerMsg::Ack(a) => Some(a.rid.to_string()),
                    ServerMsg::Error(e) => e.rid.as_ref().map(ToString::to_string),
                    _ => None,
                };
                Some((msg.kind(), rid))
            }
            VoiceEffect::Refused { rid, .. } => {
                Some(("ERROR", rid.as_ref().map(ToString::to_string)))
            }
            _ => None,
        })
        .collect()
}

fn refusal(fx: &[VoiceEffect]) -> Option<VoiceRefusal> {
    fx.iter().find_map(|e| match e {
        VoiceEffect::Refused { refusal, .. } => Some(*refusal),
        _ => None,
    })
}

fn sfu_cmds(fx: &[VoiceEffect]) -> Vec<&SfuCmd> {
    fx.iter()
        .filter_map(|e| match e {
            VoiceEffect::Sfu(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn sfu_event(u: &UserId, vsid: VoiceSessionId, kind: SfuEventKind) -> VoiceInput {
    VoiceInput::Sfu(SfuEvent {
        user: u.clone(),
        vsid,
        kind,
    })
}

fn sfu_cmd(u: &UserId, vsid: VoiceSessionId, op: SfuOp) -> SfuCmd {
    SfuCmd {
        user: u.clone(),
        vsid,
        op,
    }
}

fn active(m: &mut VoiceMachine, u: &UserId, now: Instant) -> VoiceSessionId {
    let fx = m.step(cmd(u, 1, "j", join()), now);
    let SfuCmd {
        vsid,
        op: SfuOp::Join { .. },
        ..
    } = *sfu_cmds(&fx)[0]
    else {
        panic!("{fx:?}")
    };
    m.step(sfu_event(u, vsid, SfuEventKind::Connected), now);
    vsid
}

fn offer(u: &UserId, vsid: VoiceSessionId, rev: u32) -> VoiceInput {
    let kind = SfuEventKind::Offer {
        rev,
        sdp: Sdp::new("v=0"),
        tracks: vec![RtcTrack {
            mid: "0".into(),
            user_id: u.clone(),
            purpose: TrackPurpose::VoiceAudio,
        }],
    };
    sfu_event(u, vsid, kind)
}

#[test]
fn join_acks_with_a_new_session_and_asks_the_sfu() {
    let mut m = VoiceMachine::new(caps(25));
    let a = user(1);
    let now = Instant::now();
    let fx = m.step(
        cmd(
            &a,
            1,
            "j1",
            VoiceCommand::Join {
                muted: false,
                deafened: true,
            },
        ),
        now,
    );
    // Broadcast first, then the ACK, then the SFU.
    assert!(matches!(&fx[0], VoiceEffect::MemberChanged(u) if *u == a));
    let VoiceEffect::ToConn {
        msg: ServerMsg::Ack(ack),
        conn,
        ..
    } = &fx[1]
    else {
        panic!("{fx:?}")
    };
    assert_eq!(*conn, ConnId(1));
    assert_eq!(ack.voice_session_id, Some(vsid(1)));
    assert_eq!(
        sfu_cmds(&fx),
        [&sfu_cmd(&a, vsid(1), SfuOp::Join { muted: true })]
    );
    // Deafened ⇒ Muted, and joining until the SFU reports Connected.
    assert_eq!(
        m.view(&a),
        Some(VoiceState {
            joining: true,
            muted: true,
            deafened: true,
            speaking: false,
            screen_share: None,
        })
    );
    assert_eq!(m.next_deadline(), Some(now + JOIN_WATCHDOG));
}

#[test]
fn answers_must_match_the_session_and_the_outstanding_rev() {
    let mut m = VoiceMachine::new(caps(25));
    let a = user(1);
    let now = Instant::now();
    m.step(cmd(&a, 1, "j", join()), now);
    let answer = |v, rev| VoiceCommand::Answer {
        vsid: vsid(v),
        rev,
        sdp: Sdp::new("v=0"),
    };
    let fx = m.step(cmd(&a, 1, "a0", answer(1, 1)), now);
    assert_eq!(refusal(&fx), Some(VoiceRefusal::StaleAnswer));
    m.step(offer(&a, vsid(1), 1), now);
    for (v, rev) in [(2, 1), (1, 2)] {
        let fx = m.step(cmd(&a, 1, "bad", answer(v, rev)), now);
        assert_eq!(
            refusal(&fx),
            Some(VoiceRefusal::StaleAnswer),
            "vsid {v} rev {rev}"
        );
    }
    let fx = m.step(cmd(&a, 1, "a1", answer(1, 1)), now);
    assert!(matches!(
        sfu_cmds(&fx)[..],
        [SfuCmd {
            op: SfuOp::Answer {
                rev: 1,
                rid: Some(_),
                ..
            },
            ..
        }]
    ));
    let applied = SfuEventKind::AnswerApplied { rid: rid("a1") };
    let fx = m.step(sfu_event(&a, vsid(1), applied), now);
    assert_eq!(frames(&fx), [("ACK", Some("a1".into()))]);
}

#[test]
fn a_restart_is_for_the_owners_current_active_session_only() {
    let mut m = VoiceMachine::new(caps(25));
    let a = user(1);
    let now = Instant::now();
    let restart = |v| VoiceCommand::Restart { vsid: vsid(v) };
    let fx = m.step(cmd(&a, 1, "r0", restart(1)), now);
    assert_eq!(
        refusal(&fx),
        Some(VoiceRefusal::RestartInvalid),
        "not in Voice"
    );
    m.step(cmd(&a, 1, "j", join()), now);
    let fx = m.step(cmd(&a, 1, "r1", restart(1)), now);
    assert_eq!(
        refusal(&fx),
        Some(VoiceRefusal::RestartInvalid),
        "still Joining"
    );
    m.step(sfu_event(&a, vsid(1), SfuEventKind::Connected), now);
    for (conn, v, why) in [(1, 2, "another session"), (2, 1, "another connection")] {
        let fx = m.step(cmd(&a, conn, "r2", restart(v)), now);
        assert_eq!(refusal(&fx), Some(VoiceRefusal::RestartInvalid), "{why}");
        assert!(sfu_cmds(&fx).is_empty(), "{why}");
    }
    let fx = m.step(cmd(&a, 1, "r3", restart(1)), now);
    assert_eq!(sfu_cmds(&fx), [&sfu_cmd(&a, vsid(1), SfuOp::Restart)]);
    assert_eq!(frames(&fx), [("ACK", Some("r3".into()))]);
}

#[test]
fn conn_gone_ends_only_the_owning_connections_session() {
    let mut m = VoiceMachine::new(caps(25));
    let a = user(1);
    let now = Instant::now();
    active(&mut m, &a, now);
    // The cleanup of another connection never ends this session.
    let fx = m.step(
        VoiceInput::ConnGone {
            user: a.clone(),
            conn: ConnId(2),
            reason: VoiceEndReason::ConnectionLost,
        },
        now,
    );
    assert!(fx.is_empty());
    let fx = m.step(
        VoiceInput::ConnGone {
            user: a.clone(),
            conn: ConnId(1),
            reason: VoiceEndReason::Replaced,
        },
        now,
    );
    assert!(matches!(
        fx.last(),
        Some(VoiceEffect::ToConn { conn: ConnId(1), msg: ServerMsg::VoiceEnded(v), .. })
            if v.reason == VoiceEndReason::Replaced
    ));
    assert_eq!(m.view(&a), None);
}

#[test]
fn the_fifth_unmute_within_5_s_starts_a_10_s_cooldown_that_outlives_the_session() {
    let a = user(1);
    let t0 = Instant::now();
    let mut m = VoiceMachine::new(caps(25));
    active(&mut m, &a, t0);
    let set = |muted| VoiceCommand::SetState {
        muted,
        deafened: false,
    };
    let ms = Duration::from_millis;
    for i in 0..10 {
        let fx = m.step(cmd(&a, 1, "noop", set(false)), t0 + ms(i));
        assert_eq!(refusal(&fx), None);
    }
    for i in 0..4 {
        let at = t0 + ms(100 + i * 200);
        assert_eq!(refusal(&m.step(cmd(&a, 1, "m", set(true)), at)), None);
        assert_eq!(
            refusal(&m.step(cmd(&a, 1, "u", set(false)), at)),
            None,
            "unmute {i}"
        );
    }
    let fifth = t0 + ms(1000);
    assert_eq!(refusal(&m.step(cmd(&a, 1, "m", set(true)), fifth)), None);
    let fx = m.step(cmd(&a, 1, "u5", set(false)), fifth);
    let Some(VoiceEffect::Refused {
        rid: refused,
        refusal: why,
        ..
    }) = fx.first()
    else {
        panic!("{fx:?}")
    };
    assert_eq!(*why, VoiceRefusal::UnmuteCooldown(fifth + UNMUTE_COOLDOWN));
    assert_eq!(refused.as_ref().map(Rid::as_str), Some("u5"));
    assert!(
        m.view(&a).unwrap().muted,
        "the refused unmute changed nothing"
    );
    assert!(sfu_cmds(&fx).is_empty(), "nothing reached the SFU queue");
    m.step(cmd(&a, 1, "l", VoiceCommand::Leave), fifth);
    active(&mut m, &a, fifth);
    m.step(cmd(&a, 1, "m", set(true)), fifth + ms(1));
    let fx = m.step(cmd(&a, 1, "u6", set(false)), fifth + ms(9_999));
    assert_eq!(
        refusal(&fx),
        Some(VoiceRefusal::UnmuteCooldown(fifth + UNMUTE_COOLDOWN))
    );
    let fx = m.step(cmd(&a, 1, "u7", set(false)), fifth + ms(10_000));
    assert_eq!(refusal(&fx), None);
    assert!(!m.view(&a).unwrap().muted);
}

#[test]
fn the_sixth_join_within_15_s_starts_a_15_s_cooldown() {
    let (a, b) = (user(1), user(2));
    let t0 = Instant::now();
    let mut m = VoiceMachine::new(caps(25));
    let ms = Duration::from_millis;
    for i in 0..5 {
        let at = t0 + ms(i * 100);
        let fx = m.step(cmd(&a, 1, "j", join()), at);
        assert_eq!(refusal(&fx), None, "join {i}");
        m.step(cmd(&a, 1, "l", VoiceCommand::Leave), at);
    }
    let fx = m.step(cmd(&a, 1, "j6", join()), t0 + ms(1_000));
    let Some(VoiceEffect::Refused {
        rid: refused,
        refusal: why,
        ..
    }) = fx.first()
    else {
        panic!("{fx:?}")
    };
    let ends = t0 + ms(1_000) + JOIN_COOLDOWN;
    assert_eq!(*why, VoiceRefusal::JoinCooldown(ends));
    assert_eq!(refused.as_ref().map(Rid::as_str), Some("j6"));
    assert_eq!(fx.len(), 1, "no session, no SFU command");
    assert_eq!(m.view(&a), None);
    assert_eq!(
        refusal(&m.step(cmd(&b, 1, "jb", join()), t0 + ms(1_000))),
        None,
        "another Member joins freely"
    );
    let fx = m.step(cmd(&a, 1, "j4", join()), t0 + ms(15_999));
    assert_eq!(refusal(&fx), Some(VoiceRefusal::JoinCooldown(ends)));
    let fx = m.step(cmd(&a, 1, "j5", join()), t0 + ms(16_000));
    assert_eq!(refusal(&fx), None);
    assert!(m.view(&a).is_some());
}

/// A Join that arrives while In Voice is an attempt too.
#[test]
fn a_refused_join_in_a_session_counts_toward_the_cooldown() {
    let a = user(1);
    let t0 = Instant::now();
    let mut m = VoiceMachine::new(caps(25));
    m.step(cmd(&a, 1, "j1", join()), t0);
    let fx = m.step(cmd(&a, 1, "j2", join()), t0);
    assert_eq!(refusal(&fx), Some(VoiceRefusal::AlreadyInVoice));
    for i in 3..JOIN_LIMIT {
        let fx = m.step(cmd(&a, 1, &format!("j{i}"), join()), t0);
        assert_eq!(refusal(&fx), Some(VoiceRefusal::AlreadyInVoice));
    }
    let fx = m.step(cmd(&a, 1, "j6", join()), t0);
    assert_eq!(
        refusal(&fx),
        Some(VoiceRefusal::JoinCooldown(t0 + JOIN_COOLDOWN))
    );
}

fn video_started(u: &UserId, v: VoiceSessionId) -> VoiceInput {
    sfu_event(u, v, SfuEventKind::ShareVideoStarted)
}

fn share_start() -> VoiceCommand {
    VoiceCommand::ShareStart(ShareStart {
        audio: false,
        quality: Quality::UpTo2160,
    })
}

fn watch(sharer: &UserId) -> VoiceCommand {
    VoiceCommand::Watch {
        sharer: sharer.clone(),
    }
}

fn changed(fx: &[VoiceEffect], u: &UserId) -> bool {
    fx.iter()
        .any(|e| matches!(e, VoiceEffect::MemberChanged(m) if m == u))
}

#[test]
fn watch_needs_a_streaming_share_of_someone_else() {
    let mut m = VoiceMachine::new(caps(25));
    let (a, b, c) = (user(1), user(2), user(3));
    let now = Instant::now();
    let fx = m.step(cmd(&b, 1, "w0", watch(&a)), now);
    assert_eq!(refusal(&fx), Some(VoiceRefusal::NotInVoice));
    let va = active(&mut m, &a, now);
    let vb = active(&mut m, &b, now);
    let fx = m.step(cmd(&b, 1, "w1", watch(&a)), now);
    assert_eq!(
        refusal(&fx),
        Some(VoiceRefusal::ShareNotAvailable),
        "not sharing"
    );
    m.step(cmd(&a, 1, "s", share_start()), now);
    let fx = m.step(cmd(&b, 1, "w2", watch(&a)), now);
    assert_eq!(
        refusal(&fx),
        Some(VoiceRefusal::ShareNotAvailable),
        "Pending"
    );
    m.step(video_started(&a, va), now);
    let fx = m.step(cmd(&a, 1, "w3", watch(&a)), now);
    assert_eq!(refusal(&fx), Some(VoiceRefusal::WatchSelf), "self");
    let fx = m.step(cmd(&b, 1, "w4", watch(&a)), now);
    assert_eq!(frames(&fx), [("ACK", Some("w4".into()))]);
    assert_eq!(
        sfu_cmds(&fx),
        [&sfu_cmd(&b, vb, SfuOp::Watch { sharer: a.clone() })]
    );
    assert_eq!(m.watching(&b), Some(&a));
    let fx = m.step(cmd(&b, 1, "w5", watch(&c)), now);
    assert_eq!(refusal(&fx), Some(VoiceRefusal::ShareNotAvailable));
    assert_eq!(
        m.watching(&b),
        Some(&a),
        "a refused Watch keeps the old one"
    );
    let fx = m.step(cmd(&b, 1, "u1", VoiceCommand::Unwatch), now);
    assert_eq!(frames(&fx), [("ACK", Some("u1".into()))]);
    assert_eq!(sfu_cmds(&fx).len(), 1);
    let fx = m.step(cmd(&b, 1, "u2", VoiceCommand::Unwatch), now);
    assert_eq!(frames(&fx), [("ACK", Some("u2".into()))]);
    assert!(sfu_cmds(&fx).is_empty());
    assert_eq!(m.watching(&b), None);
}

#[test]
fn stopping_a_share_or_leaving_voice_ends_it_for_every_viewer() {
    let mut m = VoiceMachine::new(caps(25));
    let (a, b) = (user(1), user(2));
    let now = Instant::now();
    let va = active(&mut m, &a, now);
    active(&mut m, &b, now);
    m.step(cmd(&a, 1, "s", share_start()), now);
    m.step(video_started(&a, va), now);
    m.step(cmd(&b, 1, "w", watch(&a)), now);
    let fx = m.step(cmd(&a, 1, "x", VoiceCommand::ShareStop), now);
    assert!(changed(&fx, &a), "everyone sees the share end");
    assert_eq!(sfu_cmds(&fx), [&sfu_cmd(&a, va, SfuOp::StopShare)]);
    assert_eq!(frames(&fx), [("ACK", Some("x".into()))]);
    assert_eq!(m.view(&a).unwrap().screen_share, None);
    assert_eq!(m.watching(&b), None);
    let fx = m.step(cmd(&a, 1, "x2", VoiceCommand::ShareStop), now);
    assert_eq!(frames(&fx), [("ACK", Some("x2".into()))]);
    assert!(sfu_cmds(&fx).is_empty());
    m.step(cmd(&a, 1, "s2", share_start()), now);
    m.step(video_started(&a, va), now);
    m.step(cmd(&b, 1, "w2", watch(&a)), now);
    let fx = m.step(cmd(&a, 1, "l", VoiceCommand::Leave), now);
    assert_eq!(sfu_cmds(&fx), [&sfu_cmd(&a, va, SfuOp::Leave)]);
    assert_eq!(m.watching(&b), None);
    assert_eq!(m.view(&a), None);
}

#[test]
fn a_share_still_pending_at_its_watchdog_ends_the_session_and_frees_the_slot() {
    let mut m = VoiceMachine::new(caps(25));
    let (a, b) = (user(1), user(2));
    let now = Instant::now();
    let va = active(&mut m, &a, now);
    let vb = active(&mut m, &b, now);
    assert_eq!(m.next_deadline(), None);
    m.step(cmd(&a, 1, "s", share_start()), now);
    m.step(cmd(&b, 1, "s", share_start()), now);
    m.step(video_started(&b, vb), now);
    assert_eq!(m.sharers(), 2);
    assert_eq!(m.next_deadline(), Some(now + SHARE_WATCHDOG));

    let fx = m.step(
        VoiceInput::Tick,
        now + SHARE_WATCHDOG - Duration::from_secs(1),
    );
    assert!(fx.is_empty());
    let fx = m.step(VoiceInput::Tick, now + SHARE_WATCHDOG);
    assert_eq!(sfu_cmds(&fx), [&sfu_cmd(&a, va, SfuOp::Leave)]);
    assert!(frames(&fx).contains(&("VOICE_ENDED", None)), "{fx:?}");
    assert!(
        fx.iter().any(|e| matches!(
            e,
            VoiceEffect::Refused {
                refusal: VoiceRefusal::ShareTimeout,
                ..
            }
        )),
        "{fx:?}"
    );
    assert_eq!(m.view(&a), None);
    assert_eq!(m.sharers(), 1, "only the share that streams is left");
    assert_eq!(m.next_deadline(), None);
}

fn source(width: u32, height: u32) -> SourceSize {
    SourceSize {
        width: width.try_into().unwrap(),
        height: height.try_into().unwrap(),
    }
}

#[test]
fn every_share_start_gets_a_new_id_and_a_source_report_shows_in_the_entity() {
    let mut m = VoiceMachine::new(caps(25));
    let a = user(1);
    let now = Instant::now();
    let va = active(&mut m, &a, now);
    let entity = |m: &VoiceMachine| m.view(&a).unwrap().screen_share;

    m.step(cmd(&a, 1, "s1", share_start()), now);
    m.step(cmd(&a, 1, "s1b", share_start()), now);
    let report = |size| VoiceInput::Cmd {
        user: a.clone(),
        conn: ConnId(1),
        rid: None,
        cmd: VoiceCommand::ShareSource(size),
    };
    let fx = m.step(report(source(1920, 1080)), now);
    assert!(!changed(&fx, &a), "Pending shares stay private: {fx:?}");
    assert!(
        sfu_cmds(&fx).iter().any(|c| c.op
            == SfuOp::ShareSource {
                size: source(1920, 1080)
            }),
        "the SFU hears the size it sends at: {fx:?}"
    );
    m.step(video_started(&a, va), now);
    let first = entity(&m).unwrap();
    assert_eq!(first.source, Some(source(1920, 1080)));
    assert_eq!(first.drawing, None);

    let fx = m.step(report(source(1280, 720)), now);
    assert!(changed(&fx, &a), "a new size is announced");
    assert_eq!(entity(&m).unwrap().source, Some(source(1280, 720)));
    assert_eq!(entity(&m).unwrap().share_id, first.share_id);
    let fx = m.step(report(source(1280, 720)), now);
    assert!(fx.is_empty(), "the same size is not");

    m.step(cmd(&a, 1, "x", VoiceCommand::ShareStop), now);
    assert!(
        m.step(report(source(800, 600)), now).is_empty(),
        "no share to apply it to"
    );
    m.step(cmd(&a, 1, "s2", share_start()), now);
    m.step(video_started(&a, va), now);
    let second = entity(&m).unwrap();
    assert_ne!(second.share_id, first.share_id);
    assert_eq!(second.source, None, "the new share has reported nothing");
}

fn drawing(on: bool) -> VoiceCommand {
    VoiceCommand::SetDrawing(on)
}

fn stroke(share: ScreenShare, epoch: DrawingEpoch) -> InkBatch {
    InkBatch {
        share_id: share.share_id,
        epoch,
        stroke: StrokeId(1),
        seq: 0,
        prev: None,
        points: vec![InkPoint { x: 10, y: 20 }],
        end: false,
    }
}

fn ink(u: &UserId, batch: InkBatch) -> VoiceInput {
    VoiceInput::Cmd {
        user: u.clone(),
        conn: ConnId(1),
        rid: None,
        cmd: VoiceCommand::Ink(batch),
    }
}

fn relayed_to(fx: &[VoiceEffect]) -> Vec<UserId> {
    let mut to: Vec<UserId> = fx
        .iter()
        .filter_map(|e| match e {
            VoiceEffect::Ink { to, .. } => Some(to),
            _ => None,
        })
        .flatten()
        .map(|(u, _)| u.clone())
        .collect();
    to.sort();
    to
}

/// A (Sharer) shares and B watches; C is In Voice but watches nothing; D never joined.
struct Board {
    m: VoiceMachine,
    now: Instant,
    a: UserId,
    b: UserId,
    c: UserId,
    d: UserId,
}

impl Board {
    fn new() -> Self {
        let mut m = VoiceMachine::new(caps(25));
        let (a, b, c, d) = (user(1), user(2), user(3), user(4));
        let now = Instant::now();
        let va = active(&mut m, &a, now);
        active(&mut m, &b, now);
        active(&mut m, &c, now);
        m.step(cmd(&a, 1, "s", share_start()), now);
        m.step(video_started(&a, va), now);
        m.step(cmd(&b, 1, "w", watch(&a)), now);
        Board { m, now, a, b, c, d }
    }

    fn share(&self) -> ScreenShare {
        self.m.view(&self.a).unwrap().screen_share.unwrap()
    }

    fn set_drawing(&mut self, on: bool) -> Vec<VoiceEffect> {
        self.m.step(cmd(&self.a, 1, "d", drawing(on)), self.now)
    }

    fn epoch(&mut self) -> DrawingEpoch {
        self.set_drawing(true);
        self.share().drawing.expect("drawing is on")
    }

    fn ink(&mut self, who: &UserId, batch: InkBatch) -> Vec<VoiceEffect> {
        self.m.step(ink(who, batch), self.now)
    }
}

#[test]
fn the_sharer_and_a_watcher_may_draw_and_everyone_else_receives_it() {
    let mut t = Board::new();
    let epoch = t.epoch();
    let batch = stroke(t.share(), epoch);
    let (a, b, c) = (t.a.clone(), t.b.clone(), t.c.clone());

    let fx = t.ink(&b, batch.clone());
    assert_eq!(
        relayed_to(&fx),
        std::slice::from_ref(&a),
        "the Sharer, not the author"
    );
    let VoiceEffect::Ink { relay, .. } = &fx[0] else {
        panic!("{fx:?}")
    };
    assert_eq!((&relay.author_id, &relay.batch), (&b, &batch));

    t.m.step(cmd(&c, 1, "w2", watch(&a)), t.now);
    let fx = t.ink(&a, batch.clone());
    assert_eq!(relayed_to(&fx), [b.clone(), c.clone()], "every Watcher");
    let fx = t.ink(&c, batch);
    assert_eq!(relayed_to(&fx), [a, b]);
}

#[test]
fn a_member_in_voice_who_does_not_watch_or_not_in_voice_may_not_draw() {
    let mut t = Board::new();
    let epoch = t.epoch();
    let batch = stroke(t.share(), epoch);
    let (c, d) = (t.c.clone(), t.d.clone());
    assert!(
        t.ink(&c, batch.clone()).is_empty(),
        "In Voice, not watching"
    );
    assert!(t.ink(&d, batch.clone()).is_empty(), "not In Voice");
    // Joining is not Active yet.
    t.m.step(cmd(&d, 1, "j", join()), t.now);
    assert!(t.ink(&d, batch.clone()).is_empty(), "Joining");
    let b = t.b.clone();
    let fx = t.m.step(
        VoiceInput::Cmd {
            user: b.clone(),
            conn: ConnId(2),
            rid: None,
            cmd: VoiceCommand::Ink(batch),
        },
        t.now,
    );
    assert!(fx.is_empty(), "another connection of the Member");
}

#[test]
fn ink_is_dropped_while_drawing_is_off_and_after_the_share_ends() {
    let mut t = Board::new();
    let (a, b) = (t.a.clone(), t.b.clone());
    let off = stroke(t.share(), DrawingEpoch(0));
    assert!(t.ink(&b, off).is_empty(), "drawing was never on");

    let epoch = t.epoch();
    let batch = stroke(t.share(), epoch);
    assert!(!t.ink(&b, batch.clone()).is_empty());
    t.set_drawing(false);
    assert!(t.ink(&b, batch.clone()).is_empty(), "drawing is off");
    assert_eq!(t.share().drawing, None);

    let epoch = t.epoch();
    let batch = stroke(t.share(), epoch);
    t.m.step(cmd(&a, 1, "x", VoiceCommand::ShareStop), t.now);
    assert!(t.ink(&b, batch).is_empty(), "the share is gone");
}

#[test]
fn a_stale_epoch_or_share_id_is_dropped_and_each_change_advances_the_epoch() {
    let mut t = Board::new();
    let (a, b) = (t.a.clone(), t.b.clone());
    let first = t.epoch();
    let old = stroke(t.share(), first);
    assert!(!t.ink(&b, old.clone()).is_empty());

    let fx = t.set_drawing(false);
    assert!(changed(&fx, &a), "viewers learn drawing went off");
    let second = t.epoch();
    assert_ne!(second, first, "off and on advances the epoch");
    assert!(
        t.ink(&b, old.clone()).is_empty(),
        "ink queued before a quick off and on never comes back"
    );
    let fx = t.set_drawing(true);
    assert!(!changed(&fx, &a), "drawing was already on");
    assert_eq!(t.share().drawing, Some(second), "and the epoch stays");

    let fx = t.m.step(
        VoiceInput::Cmd {
            user: a.clone(),
            conn: ConnId(1),
            rid: None,
            cmd: VoiceCommand::ShareSource(source(1920, 1080)),
        },
        t.now,
    );
    assert!(changed(&fx, &a));
    let third = t.share().drawing.expect("still on");
    assert_ne!(third, second, "a new source size advances the epoch");
    let current = stroke(t.share(), second);
    assert!(t.ink(&b, current).is_empty(), "drawn against the old size");
    let current = stroke(t.share(), third);
    assert!(!t.ink(&b, current).is_empty());

    t.m.step(cmd(&a, 1, "x", VoiceCommand::ShareStop), t.now);
    t.m.step(cmd(&a, 1, "s2", share_start()), t.now);
    let va = vsid(1);
    t.m.step(video_started(&a, va), t.now);
    t.m.step(cmd(&b, 1, "w2", watch(&a)), t.now);
    t.set_drawing(true);
    let fresh = t.share();
    assert_ne!(fresh.share_id, old.share_id);
    let stale_id = InkBatch {
        epoch: fresh.drawing.unwrap(),
        ..old
    };
    assert!(
        t.ink(&b, stale_id).is_empty(),
        "an old share id is dropped even at the current epoch"
    );
}

#[test]
fn only_the_sharer_sets_drawing_and_a_batch_names_the_share_it_belongs_to() {
    let mut t = Board::new();
    let (a, b, c, d) = (t.a.clone(), t.b.clone(), t.c.clone(), t.d.clone());
    let fx = t.m.step(cmd(&b, 1, "d1", drawing(true)), t.now);
    assert_eq!(
        refusal(&fx),
        Some(VoiceRefusal::ShareNotAvailable),
        "no share of their own"
    );
    let fx = t.m.step(cmd(&d, 1, "d2", drawing(true)), t.now);
    assert_eq!(refusal(&fx), Some(VoiceRefusal::NotInVoice));
    assert_eq!(t.share().drawing, None);
    let fx = t.set_drawing(true);
    assert_eq!(frames(&fx), [("ACK", Some("d".into()))]);

    // C shares too, and A watches C while sharing: a batch for A's own share is A's to draw.
    let vc = vsid(3);
    t.m.step(cmd(&c, 1, "s3", share_start()), t.now);
    t.m.step(video_started(&c, vc), t.now);
    t.m.step(cmd(&a, 1, "w3", watch(&c)), t.now);
    let own = stroke(t.share(), t.share().drawing.unwrap());
    let fx = t.ink(&a, own);
    assert_eq!(relayed_to(&fx), std::slice::from_ref(&b), "to A's Watchers");
    let theirs = t.m.view(&c).unwrap().screen_share.unwrap();
    let fx = t.ink(&a, stroke(theirs, DrawingEpoch(0)));
    assert!(fx.is_empty(), "C's drawing is off");
}

#[derive(Debug, Clone)]
enum Op {
    Join(u8, u64, bool, bool),
    Leave(u8, u64),
    Set(u8, u64, bool, bool),
    Speak(u8, u64, bool),
    Answer(u8, u64, u64, u32),
    Offer(u8, u64, u32),
    Connected(u8, u64),
    Lost(u8, u64),
    Failed(u8, u64, bool),
    Gone(u8, u64),
    Tick(u64),
    ShareStart(u8, u64),
    ShareStop(u8, u64),
    VideoStarted(u8, u64),
    Watch(u8, u64, u8),
    Unwatch(u8, u64),
    Restart(u8, u64, u64),
}

fn op() -> impl Strategy<Value = Op> {
    let u = 0u8..3;
    let c = 1u64..3;
    let v = 1u64..6;
    prop_oneof![
        (u.clone(), c.clone(), any::<bool>(), any::<bool>())
            .prop_map(|(u, c, m, d)| Op::Join(u, c, m, d)),
        (u.clone(), c.clone()).prop_map(|(u, c)| Op::Leave(u, c)),
        (u.clone(), c.clone(), any::<bool>(), any::<bool>())
            .prop_map(|(u, c, m, d)| Op::Set(u, c, m, d)),
        (u.clone(), v.clone(), any::<bool>()).prop_map(|(u, v, s)| Op::Speak(u, v, s)),
        (u.clone(), c.clone(), v.clone(), 0u32..4).prop_map(|(u, c, v, r)| Op::Answer(u, c, v, r)),
        (u.clone(), v.clone(), 1u32..4).prop_map(|(u, v, r)| Op::Offer(u, v, r)),
        (u.clone(), v.clone()).prop_map(|(u, v)| Op::Connected(u, v)),
        (u.clone(), v.clone()).prop_map(|(u, v)| Op::Lost(u, v)),
        (u.clone(), v.clone(), any::<bool>()).prop_map(|(u, v, f)| Op::Failed(u, v, f)),
        (u.clone(), c.clone()).prop_map(|(u, c)| Op::Gone(u, c)),
        (0u64..20).prop_map(Op::Tick),
        (u.clone(), c.clone()).prop_map(|(u, c)| Op::ShareStart(u, c)),
        (u.clone(), c.clone()).prop_map(|(u, c)| Op::ShareStop(u, c)),
        (u.clone(), v.clone()).prop_map(|(u, v)| Op::VideoStarted(u, v)),
        (u.clone(), c.clone(), 0u8..3).prop_map(|(u, c, s)| Op::Watch(u, c, s)),
        (u.clone(), c.clone()).prop_map(|(u, c)| Op::Unwatch(u, c)),
        (u, c, v).prop_map(|(u, c, v)| Op::Restart(u, c, v)),
    ]
}

fn input(op: &Op) -> (VoiceInput, Option<&'static str>, u64) {
    let rid_ = Some("r1");
    match *op {
        Op::Join(u, c, muted, deafened) => (
            cmd(&user(u), c, "r1", VoiceCommand::Join { muted, deafened }),
            rid_,
            0,
        ),
        Op::Leave(u, c) => (cmd(&user(u), c, "r1", VoiceCommand::Leave), rid_, 0),
        Op::Set(u, c, muted, deafened) => (
            cmd(
                &user(u),
                c,
                "r1",
                VoiceCommand::SetState { muted, deafened },
            ),
            rid_,
            0,
        ),
        Op::Speak(u, v, s) => (
            sfu_event(&user(u), vsid(v), SfuEventKind::Speaking(s)),
            None,
            0,
        ),
        Op::Answer(u, c, v, rev) => (
            cmd(
                &user(u),
                c,
                "r1",
                VoiceCommand::Answer {
                    vsid: vsid(v),
                    rev,
                    sdp: Sdp::new("v=0"),
                },
            ),
            None,
            0,
        ),
        Op::Offer(u, v, rev) => (offer(&user(u), vsid(v), rev), None, 0),
        Op::Connected(u, v) => (
            sfu_event(&user(u), vsid(v), SfuEventKind::Connected),
            None,
            0,
        ),
        Op::Lost(u, v) => (sfu_event(&user(u), vsid(v), SfuEventKind::Lost), None, 0),
        Op::Failed(u, v, fatal) => (
            sfu_event(
                &user(u),
                vsid(v),
                SfuEventKind::NegotiationFailed { rid: None, fatal },
            ),
            None,
            0,
        ),
        Op::Gone(u, c) => (
            VoiceInput::ConnGone {
                user: user(u),
                conn: ConnId(c),
                reason: VoiceEndReason::ConnectionLost,
            },
            None,
            0,
        ),
        Op::Tick(secs) => (VoiceInput::Tick, None, secs),
        Op::ShareStart(u, c) => (cmd(&user(u), c, "r1", share_start()), rid_, 0),
        Op::ShareStop(u, c) => (cmd(&user(u), c, "r1", VoiceCommand::ShareStop), rid_, 0),
        Op::VideoStarted(u, v) => (video_started(&user(u), vsid(v)), None, 0),
        Op::Watch(u, c, s) => (cmd(&user(u), c, "r1", watch(&user(s))), rid_, 0),
        Op::Unwatch(u, c) => (cmd(&user(u), c, "r1", VoiceCommand::Unwatch), rid_, 0),
        Op::Restart(u, c, v) => (
            cmd(&user(u), c, "r1", VoiceCommand::Restart { vsid: vsid(v) }),
            rid_,
            0,
        ),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Invariants: `voice` present iff a session; deafened ⇒ muted; speaking ⇒ Active ∧ ¬muted; SFU
    /// commands only for the current or a just-ended generation; exactly one terminal frame for
    /// every Join, Leave and State command.
    #[test]
    fn invariants_hold_over_any_input_sequence(ops in proptest::collection::vec(op(), 1..60)) {
        // Three users and caps below that, so both caps bind and the asserts below can fail.
        let mut m = VoiceMachine::new(VoiceConfig { max_participants: 2, max_sharers: 1 });
        let t0 = Instant::now();
        // Two Members start Active: random input rarely finds the Answer that activates a session.
        active(&mut m, &user(0), t0);
        active(&mut m, &user(1), t0);
        let mut elapsed = 0u64;
        for op in &ops {
            let (input, terminal_rid, advance) = input(op);
            elapsed += advance;
            let now = t0 + Duration::from_secs(elapsed);
            let before: HashMap<UserId, VoiceSessionId> =
                m.sessions.iter().map(|(u, s)| (u.clone(), s.id)).collect();
            let fx = m.step(input, now);
            for u in 0..3 {
                let u = user(u);
                let view = m.view(&u);
                prop_assert_eq!(view.is_some(), m.sessions.contains_key(&u));
                if let Some(v) = view {
                    prop_assert!(!v.deafened || v.muted, "deafened ⇒ muted: {:?}", v);
                    prop_assert!(!v.speaking || (!v.joining && !v.muted), "speaking: {:?}", v);
                    prop_assert!(v.screen_share.is_none() || !v.joining, "a share needs Active: {:?}", v);
                }
                if let Some(sharer) = m.watching(&u) {
                    prop_assert!(*sharer != u);
                    prop_assert!(m.view(sharer).is_some_and(|v| v.screen_share.is_some()), "{:?} watches a share that is not Streaming", u);
                }
            }
            for c in sfu_cmds(&fx) {
                let (u, v) = (&c.user, c.vsid);
                let current = m.sessions.get(u).map(|s| s.id);
                let previous = before.get(u).copied();
                let stale_offer_cleanup = c.op == SfuOp::Leave;
                prop_assert!(
                    current == Some(v) || previous == Some(v) || stale_offer_cleanup,
                    "{:?} for a generation that never existed", c
                );
                if c.op == SfuOp::Restart {
                    prop_assert!(
                        current == Some(v) && m.view(u).is_some_and(|s| !s.joining),
                        "{:?} for a session that is not the current Active one", c
                    );
                }
            }
            if let Some(r) = terminal_rid {
                let terminal = frames(&fx)
                    .into_iter()
                    .filter(|(k, rid)| (*k == "ACK" || *k == "ERROR") && rid.as_deref() == Some(r))
                    .count();
                prop_assert_eq!(terminal, 1, "{:?} -> {:?}", op, fx);
            }
            prop_assert!(m.participants() <= 2);
            prop_assert!(m.sharers() <= 1);
        }
    }
}

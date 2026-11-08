//! End to end: a real in-process `lobby-server` (str0m SFU on loopback UDP) and two
//! Members running the shipped media engine with a synthetic tone as the microphone
//! and a capture ring as the speakers.
//!
//! `calls` is the call itself. The tests here are the media path under failure: an outage
//! heals, and UDP dying moves the call to TCP.

use std::{num::NonZeroU32, time::Duration};

use lobby_client::{IssueKind, LobbyState, MediaConfig, StunTimings, UiEffect, VoicePhase};
use lobby_protocol::RestartPolicy;
use lobby_server::{MediaPath, testing::TestServer};

mod calls;
mod mic;
mod support;

use crate::common::{Member, WAIT, active};
use support::{alice_and_bob, in_voice_together_with, until_both_hear};

/// The path is down after about 1 s of silence, and each of 3 restarts gets `timeout`.
const QUICK_STUN: StunTimings = StunTimings {
    initial_rto: Duration::from_millis(50),
    max_rto: Duration::from_millis(250),
    retransmits: 4,
};

fn quick_restarts(timeout: Duration) -> RestartPolicy {
    RestartPolicy {
        attempts: NonZeroU32::new(3).unwrap(),
        timeout,
    }
}

fn quick(timeout: Duration) -> impl Fn(&mut MediaConfig) {
    move |m| {
        m.stun = QUICK_STUN;
        m.restart = quick_restarts(timeout);
    }
}

async fn in_voice_quick(
    a: &mut Member,
    b: &mut Member,
    timeout: Duration,
) -> (rtrb::Consumer<f32>, rtrb::Consumer<f32>) {
    in_voice_together_with(a, b, quick(timeout), quick(timeout)).await
}

fn reconnecting(s: &LobbyState) -> bool {
    matches!(
        s.voice.phase,
        VoicePhase::InVoice {
            reconnecting: true,
            ..
        }
    )
}

fn vsid_of(c: &Member) -> lobby_protocol::VoiceSessionId {
    match c.state().voice.phase {
        VoicePhase::InVoice { vsid, .. } => vsid,
        phase => panic!("not In Voice: {phase:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn voice_recovers_from_a_media_outage_without_rejoining() {
    let (server, relay) = TestServer::start_behind_relay(&["alice", "bob"], |_| {}).await;
    let (mut a, mut b) = alice_and_bob(&server).await;
    let mut speakers = in_voice_quick(&mut a, &mut b, Duration::from_millis(1500)).await;
    until_both_hear(&mut a, &mut b, &mut speakers, WAIT).await;
    let vsid = vsid_of(&a);

    let media = a.voice_media_addr().await.expect("a's media socket");
    relay.blackhole_client(media, true);
    relay.blackhole_tcp(true);
    a.wait_until("alice reconnecting", reconnecting).await;
    relay.blackhole_client(media, false);
    relay.blackhole_tcp(false);

    until_both_hear(&mut a, &mut b, &mut speakers, WAIT).await;
    a.wait_until("alice recovered", |s| !reconnecting(s)).await;
    assert_eq!(vsid_of(&a), vsid, "the same Voice session");
    assert!(active(b.state(), "alice"), "bob sees alice In Voice");
    assert_eq!(a.voice_ended().await, &[]);
    assert!(
        !a.take_effects()
            .iter()
            .any(|fx| matches!(fx, UiEffect::Issue(_))),
        "no Issue for a recovered outage"
    );
    assert!(server.server.sfu_ice_restarts() >= 1);
    assert_eq!(server.server.sfu_peers_lost(), 0);
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

/// UDP dies mid-call while TCP still works: alice's client moves her media to TCP and the call
/// goes on in the same Voice session, with no Issue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn voice_moves_to_tcp_when_udp_dies_mid_call() {
    let (server, relay) = TestServer::start_behind_relay(&["alice", "bob"], |cfg| {
        cfg.sfu.dead_peer_after = Duration::from_secs(2);
        cfg.sfu.dead_peer_after_restart = Duration::from_secs(15);
    })
    .await;
    let (mut a, mut b) = alice_and_bob(&server).await;
    let mut speakers = in_voice_quick(&mut a, &mut b, Duration::from_millis(2500)).await;
    until_both_hear(&mut a, &mut b, &mut speakers, WAIT).await;
    let vsid = vsid_of(&a);
    let alice = server.user_id_of("alice").await;
    assert_eq!(server.server.sfu_media_path(&alice), Some(MediaPath::Udp));

    relay.blackhole_client(a.voice_media_addr().await.expect("a's media socket"), true);
    until_both_hear(&mut a, &mut b, &mut speakers, WAIT).await;
    assert_eq!(
        server.server.sfu_media_path(&alice),
        Some(MediaPath::Tcp),
        "heard again over TCP"
    );
    assert_eq!(vsid_of(&a), vsid, "the same Voice session");
    assert_eq!(a.voice_ended().await, &[]);
    assert_eq!(a.issue(IssueKind::VoiceEnded), None);
    assert_eq!(server.server.sfu_peers_lost(), 0);
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

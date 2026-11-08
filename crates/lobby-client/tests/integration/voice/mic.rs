//! What a Member is told about their own microphone: talking while muted, and a device that
//! sends only zeros. And what they hear of it: MONITOR with no call on.

use std::time::{Duration, Instant};

use lobby_client::{AudioSource, IssueKind, MediaConfig, MicTest, Tone};
use lobby_server::testing::TestServer;

use super::support::{alice_and_bob, drain, tone_share, tone_with_capture};
use crate::common::{WAIT, active};

/// Polls the threads' output; the tone and the speakers run on the real clock.
const POLL: Duration = Duration::from_millis(20);

#[test]
fn monitor_with_no_call_plays_the_microphone_through_the_rack_until_dropped() {
    let (media, mut speakers) = tone_with_capture(440.0);
    media.monitor.set_on(true);
    let levels = media.levels.clone();
    let monitor = MicTest::monitored(media);
    let mut heard = Vec::new();
    let started = Instant::now();
    while heard.len() < 48_000 {
        assert!(
            started.elapsed() < WAIT,
            "a second of the speakers within {WAIT:?}"
        );
        std::thread::sleep(POLL);
        heard.extend(drain(&mut speakers));
    }
    assert_eq!(monitor.failure(), None);
    let share = tone_share(&heard[heard.len() / 2..], 440.0);
    assert!(
        share > 0.5,
        "the speakers play the microphone's tone: {share}"
    );
    let rack = levels.read();
    assert!(
        rack.mic_db > -40.0 && rack.out_db > -40.0,
        "the rack's levels follow it: {rack:?}"
    );

    drop(monitor);
    let started = Instant::now();
    while !speakers.is_abandoned() {
        assert!(
            started.elapsed() < WAIT,
            "the speakers stop within {WAIT:?}"
        );
        std::thread::sleep(POLL);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn talking_while_muted_pulses_the_mute_button_once() {
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut a, b) = alice_and_bob(&server).await;
    a.join_voice(MediaConfig::fake()).await.unwrap();
    a.wait_until("alice In Voice", |s| active(s, "alice")).await;
    assert_eq!(
        a.state().voice.mute_nudges,
        0,
        "talking unmuted is not nudged"
    );

    a.toggle_mute().await.unwrap();
    a.wait_until("nudged", |s| s.voice.mute_nudges == 1).await;
    assert_eq!(a.issue(IssueKind::MicSilent), None);
    a.leave_voice().await;
    assert_eq!(a.state().voice.mute_nudges, 0, "a call starts clean");
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_microphone_of_zeros_is_one_issue() {
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut a, b) = alice_and_bob(&server).await;
    a.join_voice(MediaConfig {
        source: AudioSource::Synthetic(Tone {
            hz: 440.0,
            amplitude: 0.0,
        }),
        mic_silent_after: Some(10),
        ..MediaConfig::fake()
    })
    .await
    .unwrap();
    let said = a.wait_issue(IssueKind::MicSilent).await;
    assert!(said.contains("microphone privacy"), "{said}");
    a.run_for(Duration::from_millis(600)).await;
    let raised = a
        .take_effects()
        .iter()
        .filter(
            |fx| matches!(fx, lobby_client::UiEffect::Issue(i) if i.kind == IssueKind::MicSilent),
        )
        .count();
    assert_eq!(raised, 1, "told once per call");
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

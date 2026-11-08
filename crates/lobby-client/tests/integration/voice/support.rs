//! Two signed-in clients with a tone for a microphone and a capture ring for speakers, and
//! what to ask of them.

use std::time::Duration;

use lobby_client::{
    AudioSink, AudioSource, LobbyState, MediaConfig, MemoryTokenStore, Tone, testing::test_client,
};
use lobby_protocol::VoiceState;
use lobby_server::testing::TestServer;

use crate::common::{Member, WAIT, active, origin, until};

const SAMPLE_RATE: f32 = 48_000.0;

pub fn voice_of(state: &LobbyState, name: &str) -> Option<VoiceState> {
    state
        .members
        .values()
        .find(|m| m.username.as_str() == name)
        .and_then(|m| m.voice)
}

pub async fn run_both(a: &mut Member, b: &mut Member, d: Duration) {
    tokio::join!(a.run_for(d), b.run_for(d));
}

pub fn tone_share(samples: &[f32], hz: f32) -> f32 {
    let w = std::f32::consts::TAU * hz / SAMPLE_RATE;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &x in samples {
        let s = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    let total: f32 = samples.iter().map(|x| x * x).sum();
    if total == 0.0 {
        return 0.0;
    }
    (2.0 * power / (samples.len() as f32 * total)).min(1.0)
}

pub fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|x| x * x).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

pub fn drain(ring: &mut rtrb::Consumer<f32>) -> Vec<f32> {
    let mut out = Vec::with_capacity(ring.slots());
    while let Ok(s) = ring.pop() {
        out.push(s);
    }
    out
}

pub fn tone_with_capture(hz: f32) -> (MediaConfig, rtrb::Consumer<f32>) {
    let (tx, rx) = rtrb::RingBuffer::new(48_000 * 10);
    (
        MediaConfig {
            source: AudioSource::Synthetic(Tone::hz(hz)),
            sink: AudioSink::Capture(tx),
            ..MediaConfig::fake()
        },
        rx,
    )
}

pub async fn alice_and_bob(server: &TestServer) -> (Member, Member) {
    let origin = origin(server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));
    let a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    let b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();
    (a, b)
}

/// Both join Voice with a tone as the microphone and wait until both are In Voice on both.
/// Keep the returned speakers: they are what `a` and `b` hear.
pub async fn in_voice_together_with(
    a: &mut Member,
    b: &mut Member,
    tweak_a: impl FnOnce(&mut MediaConfig),
    tweak_b: impl FnOnce(&mut MediaConfig),
) -> (rtrb::Consumer<f32>, rtrb::Consumer<f32>) {
    let (mut media_a, heard_by_a) = tone_with_capture(440.0);
    let (mut media_b, heard_by_b) = tone_with_capture(660.0);
    tweak_a(&mut media_a);
    tweak_b(&mut media_b);
    a.join_voice(media_a).await.unwrap();
    b.join_voice(media_b).await.unwrap();
    until(a, b, "both In Voice on both clients", |sa, sb| {
        ["alice", "bob"]
            .iter()
            .all(|n| active(sa, n) && active(sb, n))
    })
    .await;
    (heard_by_a, heard_by_b)
}

/// How much of `hz` comes out of `speaker` over the next second, after a moment for the jitter
/// buffer.
async fn hearing(
    a: &mut Member,
    b: &mut Member,
    speaker: &mut rtrb::Consumer<f32>,
    hz: f32,
) -> f32 {
    run_both(a, b, Duration::from_millis(300)).await;
    drain(speaker);
    run_both(a, b, Duration::from_secs(1)).await;
    tone_share(&drain(speaker), hz)
}

/// Until `speaker` plays `hz` again, within `WAIT`.
pub async fn until_hears(
    a: &mut Member,
    b: &mut Member,
    speaker: &mut rtrb::Consumer<f32>,
    hz: f32,
) {
    let started = std::time::Instant::now();
    while hearing(a, b, speaker, hz).await <= 0.5 {
        assert!(
            started.elapsed() < WAIT,
            "{hz} Hz not heard within {WAIT:?}"
        );
    }
}

pub async fn until_silent(a: &mut Member, b: &mut Member, speaker: &mut rtrb::Consumer<f32>) {
    let started = std::time::Instant::now();
    loop {
        drain(speaker);
        run_both(a, b, Duration::from_secs(1)).await;
        let level = rms(&drain(speaker));
        if level < 0.001 {
            return;
        }
        assert!(
            started.elapsed() < WAIT,
            "still heard after {WAIT:?}: rms {level}"
        );
    }
}

pub async fn until_both_hear(
    a: &mut Member,
    b: &mut Member,
    (heard_by_a, heard_by_b): &mut (rtrb::Consumer<f32>, rtrb::Consumer<f32>),
    within: Duration,
) {
    let started = std::time::Instant::now();
    loop {
        let at_a = hearing(a, b, heard_by_a, 660.0).await;
        let at_b = hearing(a, b, heard_by_b, 440.0).await;
        if at_a > 0.5 && at_b > 0.5 {
            return;
        }
        assert!(
            started.elapsed() < within,
            "not heard within {within:?}: alice hears bob {at_a:.2}, bob hears alice {at_b:.2}"
        );
    }
}

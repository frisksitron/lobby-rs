//! The call itself: who hears whom, mute, leaving.

use std::time::Duration;

use lobby_client::{LobbyState, MemoryTokenStore, VoicePhase, testing::test_client};
use lobby_protocol::UserId;
use lobby_server::testing::TestServer;

use super::support::{
    drain, run_both, tone_share, tone_with_capture, until_both_hear, until_hears, until_silent,
    voice_of,
};
use crate::common::{Member, WAIT, active, origin, until};

/// A fresh observer's READY must equal what each client folded, for
/// presence and Voice (`speaking` aside: it changes with every VAD transition).
async fn check_invariants(server: &TestServer, clients: &mut [&mut Member]) {
    let store = MemoryTokenStore::new();
    let client = test_client(&store);
    let observer = Member::sign_in(&client, &origin(server), "observer", &server.mail)
        .await
        .unwrap();
    let truth = observer.state();
    let me = truth.me.clone().unwrap();
    let view = |s: &LobbyState| -> Vec<(UserId, Option<(bool, bool)>)> {
        s.members
            .values()
            .filter(|m| m.id != me)
            .map(|m| (m.id.clone(), m.voice.map(|v| (v.joining, v.muted))))
            .collect()
    };
    let expected = view(truth);
    for c in clients.iter_mut() {
        c.wait_until("voice state equal to a fresh READY", |s| {
            view(s) == expected
        })
        .await;
    }
    observer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_members_hear_each_other_mute_and_leave() {
    let server = TestServer::start_with_members(&["alice", "bob", "observer"], |_| {}).await;
    let origin = origin(&server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));
    let mut a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    let mut b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();

    let (media_a, heard_by_a) = tone_with_capture(440.0);
    let (media_b, heard_by_b) = tone_with_capture(660.0);
    a.join_voice(media_a).await.unwrap();
    assert_eq!(a.state().voice.phase, VoicePhase::Joining);
    b.join_voice(media_b).await.unwrap();
    until(&mut a, &mut b, "both In Voice on both clients", |sa, sb| {
        ["alice", "bob"]
            .iter()
            .all(|n| active(sa, n) && active(sb, n))
    })
    .await;
    assert!(matches!(a.state().voice.phase, VoicePhase::InVoice { .. }));

    let mut speakers = (heard_by_a, heard_by_b);
    until_both_hear(&mut a, &mut b, &mut speakers, WAIT).await;
    let (heard_by_a, heard_by_b) = &mut speakers;
    until(&mut a, &mut b, "both speaking on both", |sa, sb| {
        ["alice", "bob"].iter().all(|n| {
            voice_of(sa, n).is_some_and(|v| v.speaking)
                && voice_of(sb, n).is_some_and(|v| v.speaking)
        })
    })
    .await;
    run_both(&mut a, &mut b, Duration::from_millis(500)).await;
    let at_b = drain(heard_by_b);
    assert!(tone_share(&at_b, 660.0) < 0.05, "b does not hear itself");
    check_invariants(&server, &mut [&mut a, &mut b]).await;

    let junk = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let media_addr = a.voice_media_addr().await.expect("a's media socket");
    for _ in 0..50 {
        junk.send_to(b"definitely not webrtc", media_addr).unwrap();
        junk.send_to(&[0x80u8; 200], media_addr).unwrap();
    }

    a.toggle_mute().await.unwrap();
    until(&mut a, &mut b, "a muted on both", |sa, sb| {
        voice_of(sa, "alice").is_some_and(|v| v.muted && !v.speaking)
            && voice_of(sb, "alice").is_some_and(|v| v.muted && !v.speaking)
    })
    .await;
    until_silent(&mut a, &mut b, heard_by_b).await;
    until_hears(&mut a, &mut b, heard_by_a, 660.0).await;

    a.toggle_mute().await.unwrap();
    until(&mut a, &mut b, "a unmuted on b", |_, sb| {
        voice_of(sb, "alice").is_some_and(|v| !v.muted)
    })
    .await;
    until_both_hear(&mut a, &mut b, &mut speakers, WAIT).await;

    a.leave_voice().await;
    assert_eq!(a.state().voice.phase, VoicePhase::Idle);
    until(&mut a, &mut b, "a out of Voice on both", |sa, sb| {
        voice_of(sa, "alice").is_none() && voice_of(sb, "alice").is_none()
    })
    .await;
    check_invariants(&server, &mut [&mut a, &mut b]).await;
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

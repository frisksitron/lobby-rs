//! End to end: a real in-process `lobby-server`, Members, and the `ChaosProxy`
//! breaking the path between them: half-open sockets are found stale and replaced, cut
//! connections come back by themselves, and presence (idle, Do Not Disturb) survives it all.

use std::time::Duration;

use lobby_client::{
    MemoryTokenStore, Phase,
    testing::{ChaosProxy, test_client},
};
use lobby_protocol::Presence;
use lobby_server::testing::TestServer;

use crate::common::{Member, origin, presence};

async fn fast_heartbeat_server(names: &[&str]) -> TestServer {
    TestServer::start_with_members(names, |c| {
        c.ws.ping_interval = Duration::from_millis(200);
        c.ws.pong_timeout = Duration::from_secs(2);
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_half_open_path_goes_stale_and_is_replaced_without_losing_the_lobby() {
    let server = fast_heartbeat_server(&["alice", "bob"]).await;
    let proxy = ChaosProxy::start(server.addr).await;
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let mut a = Member::sign_in(
        &test_client(&store_a),
        &proxy.origin(),
        "alice",
        &server.mail,
    )
    .await
    .unwrap();
    let mut b = Member::sign_in(
        &test_client(&store_b),
        &origin(&server),
        "bob",
        &server.mail,
    )
    .await
    .unwrap();
    a.send("before the cable was pulled").await;
    // Both sockets of the WebSocket's path stay open but nothing moves: a pulled cable. (REST
    // keeps working: its pooled connection is idle and evicted after 20 s in real outages.)
    proxy.freeze_websockets();
    a.wait_phase_within(Duration::from_secs(8), "the stale redial", |p| {
        matches!(p, Phase::Connecting { attempt: 0 })
    })
    .await;
    a.wait_connected().await;
    let m = b.send("did you miss me?").await;
    a.wait_until("bob's Message", |s| s.chat.contains_key(&m.id))
        .await;
    assert_eq!(presence(b.state(), "alice"), Some(Presence::Online));
    a.send("back on the new socket").await;
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_connection_comes_back_by_itself_and_reloads_what_it_missed() {
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let proxy = ChaosProxy::start(server.addr).await;
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let mut a = Member::sign_in(
        &test_client(&store_a),
        &proxy.origin(),
        "alice",
        &server.mail,
    )
    .await
    .unwrap();
    let mut b = Member::sign_in(
        &test_client(&store_b),
        &origin(&server),
        "bob",
        &server.mail,
    )
    .await
    .unwrap();
    proxy.set_blackhole(true);
    proxy.cut_all();
    a.wait_until("the backoff", |s| {
        matches!(s.phase, Phase::Reconnecting { attempt: 1, .. })
    })
    .await;
    b.wait_until("alice offline", |s| {
        presence(s, "alice") == Some(Presence::Offline)
    })
    .await;
    let missed = b.send("while you were away").await;
    proxy.set_blackhole(false);
    // The first reconnect attempt after the 2 s ± 20 % tier; the blackholed one of it times out
    // only if it was dialed during the blackhole, so allow for a second attempt.
    a.wait_phase_within(Duration::from_secs(15), "reconnected", |p| {
        *p == Phase::Connected
    })
    .await;
    a.wait_connected().await;
    assert!(
        a.state().chat.contains_key(&missed.id),
        "the newest page was reloaded"
    );
    b.wait_until("alice online", |s| {
        presence(s, "alice") == Some(Presence::Online)
    })
    .await;
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

/// A client that was away gets what it missed in READY, the Messages after the newest one it
/// held, and keeps what it had (no reload of the newest page, no request of its own).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_back_from_an_outage_backfills_the_messages_it_missed() {
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let proxy = ChaosProxy::start(server.addr).await;
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let mut a = Member::sign_in(
        &test_client(&store_a),
        &proxy.origin(),
        "alice",
        &server.mail,
    )
    .await
    .unwrap();
    let mut b = Member::sign_in(
        &test_client(&store_b),
        &origin(&server),
        "bob",
        &server.mail,
    )
    .await
    .unwrap();
    let before = a.send("before the outage").await;
    proxy.set_refusing(true);
    proxy.cut_all();
    a.wait_until("the backoff", |s| {
        matches!(s.phase, Phase::Reconnecting { .. })
    })
    .await;
    let mut missed = Vec::new();
    for i in 0..3 {
        missed.push(b.send(&format!("missed {i}")).await.id);
        tokio::time::sleep(Duration::from_millis(220)).await;
    }
    proxy.set_refusing(false);
    a.wait_phase_within(Duration::from_secs(15), "reconnected", |p| {
        *p == Phase::Connected
    })
    .await;
    a.wait_until("the missed Messages", |s| {
        missed.iter().all(|id| s.chat.contains_key(id))
    })
    .await;
    assert!(a.state().chat.contains_key(&before.id), "kept what it had");
    let asked: Vec<String> = proxy
        .requests()
        .into_iter()
        .filter(|line| line.starts_with("GET /api/v1/messages"))
        .collect();
    assert!(asked.is_empty(), "READY carried the history: {asked:?}");
    assert_eq!(a.state().chat.len(), 4);
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

fn ws_dials(proxy: &ChaosProxy) -> usize {
    proxy
        .requests()
        .iter()
        .filter(|line| line.starts_with("GET /api/v1/ws"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_and_dnd_reach_others_and_survive_a_reconnect() {
    let server = fast_heartbeat_server(&["alice", "bob"]).await;
    let proxy = ChaosProxy::start(server.addr).await;
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let mut a = Member::sign_in(
        &test_client(&store_a),
        &proxy.origin(),
        "alice",
        &server.mail,
    )
    .await
    .unwrap();
    let mut b = Member::sign_in(
        &test_client(&store_b),
        &origin(&server),
        "bob",
        &server.mail,
    )
    .await
    .unwrap();
    a.set_idle(true).await;
    b.wait_until("alice idle", |s| {
        presence(s, "alice") == Some(Presence::Idle)
    })
    .await;
    a.wait_until("alice sees herself idle", |s| {
        s.me_presence() == Some(Presence::Idle)
    })
    .await;
    let dials_before = ws_dials(&proxy);
    proxy.freeze_websockets();
    // The first redial goes at once, so its Connecting snapshot can be overtaken by Connected
    // before a step reads it: the proxy's count of WebSocket dials is the proof it happened.
    a.wait_phase_within(Duration::from_secs(8), "the stale redial", |_| {
        ws_dials(&proxy) > dials_before
    })
    .await;
    a.wait_connected().await;
    a.wait_until("idle again after READY", |s| {
        s.me_presence() == Some(Presence::Idle)
    })
    .await;
    b.wait_until("alice idle again", |s| {
        presence(s, "alice") == Some(Presence::Idle)
    })
    .await;
    a.set_dnd(true).await;
    b.wait_until("alice in Do Not Disturb", |s| {
        presence(s, "alice") == Some(Presence::Dnd)
    })
    .await;
    proxy.freeze_websockets();
    // A second loss this soon after the last one backs off (one redial without a wait is
    // allowed per stable session), so this one returns after the 2 s tier.
    a.wait_phase_within(Duration::from_secs(8), "the loss", |p| {
        *p != Phase::Connected
    })
    .await;
    a.wait_phase_within(Duration::from_secs(10), "back", |p| *p == Phase::Connected)
        .await;
    a.wait_connected().await;
    assert_eq!(
        a.state().me_presence(),
        Some(Presence::Dnd),
        "READY says what IDENTIFY asked for"
    );
    b.run_for(Duration::from_millis(300)).await;
    assert_eq!(presence(b.state(), "alice"), Some(Presence::Dnd));
    a.set_dnd(false).await;
    b.wait_until("alice idle", |s| {
        presence(s, "alice") == Some(Presence::Idle)
    })
    .await;
    a.set_idle(false).await;
    b.wait_until("alice online", |s| {
        presence(s, "alice") == Some(Presence::Online)
    })
    .await;
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

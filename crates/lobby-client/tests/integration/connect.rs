//! The connection lifecycle against `FakeLobby`, for what only the other end can observe (close
//! codes, abandoned requests) and for servers that do not exist (another protocol version).

use std::time::Duration;

use lobby_client::{
    LobbyInfoError, MemoryTokenStore, Phase,
    testing::{FakeBehavior, FakeLobby, Observed, fast_reconnects, test_client, tokens_for_tests},
};
use lobby_protocol::{PROTOCOL_VERSION, close};

use crate::common::Member;

fn signed_in(fake: &FakeLobby) -> (MemoryTokenStore, lobby_client::Client) {
    let store = MemoryTokenStore::new();
    use lobby_client::TokenStore as _;
    store.save(&fake.origin(), &tokens_for_tests("t")).unwrap();
    let client = test_client(&store);
    (store, client)
}

#[tokio::test]
async fn shutting_down_closes_the_socket_with_1000() {
    let mut fake = FakeLobby::start(FakeBehavior::default()).await;
    let (_store, client) = signed_in(&fake);
    let mut s = Member::start(&client, &fake.origin());
    s.wait_connected().await;
    s.shutdown().await;
    assert_eq!(fake.next_close().await, Some(close::NORMAL));
}

#[tokio::test]
async fn another_protocol_version_is_refused_at_the_url_step_and_at_hello() {
    let mut fake = FakeLobby::start(FakeBehavior {
        protocol_version: PROTOCOL_VERSION + 1,
        ..FakeBehavior::default()
    })
    .await;
    let (_store, client) = signed_in(&fake);
    let err = client
        .lobby_info(&fake.origin())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        err,
        LobbyInfoError::ProtocolMismatch {
            server: PROTOCOL_VERSION + 1
        }
    );
    assert_eq!(fake.next_observed().await, Observed::LobbyInfoRequested);

    let mut s = Member::start(&client, &fake.origin());
    s.wait_phase(Phase::ProtocolMismatch).await;
    // No credential went out: the version is checked before IDENTIFY.
    assert_eq!(fake.next_close().await, Some(close::NORMAL));
    s.shutdown().await;
}

/// The close-code table as the client applies it.
#[tokio::test]
async fn each_close_code_gets_its_reconnect() {
    // 4008 (slow consumer) and a socket that ends after READY: straight back, attempt 0.
    for code in [close::SLOW_CONSUMER] {
        let fake = FakeLobby::start(FakeBehavior {
            close_with: Some(code),
            ..FakeBehavior::default()
        })
        .await;
        let (_store, client) = signed_in(&fake);
        let mut s = Member::start(&client, &fake.origin());
        s.wait_phase(Phase::Connected).await;
        s.wait_phase_within(Duration::from_secs(2), "the immediate redial", |p| {
            matches!(p, Phase::Connecting { attempt: 0 })
        })
        .await;
        s.wait_phase(Phase::Connected).await;
        // The same right after READY again: one redial without a wait is enough, now it backs off.
        s.wait_until("the backoff", |st| {
            matches!(st.phase, Phase::Reconnecting { attempt: 1, .. })
        })
        .await;
        s.shutdown().await;
    }
    // 1001 (server restart), 4001, 4002, 4029: the normal backoff from attempt 1.
    for code in [
        close::GOING_AWAY,
        close::IDENTIFY_TIMEOUT,
        close::PONG_TIMEOUT,
        close::IDENTIFY_RATE_LIMITED,
    ] {
        let fake = FakeLobby::start(FakeBehavior {
            close_with: Some(code),
            ..FakeBehavior::default()
        })
        .await;
        let (_store, client) = signed_in(&fake);
        let mut s = Member::start(&client, &fake.origin());
        s.wait_until(&format!("the backoff after {code}"), |st| {
            matches!(st.phase, Phase::Reconnecting { attempt: 1, .. })
        })
        .await;
        let retry_in = s.state().phase.retry_in().unwrap();
        assert!(
            retry_in >= Duration::from_millis(1600) && retry_in <= Duration::from_millis(2400),
            "{code}: 2 s ± 20 %, got {retry_in:?}"
        );
        s.shutdown().await;
    }
    // 4000 once is retried; the same refusal twice is this client's bug: Unavailable.
    let fake = FakeLobby::start(FakeBehavior {
        close_with: Some(close::PROTOCOL_VIOLATION),
        ..FakeBehavior::default()
    })
    .await;
    let (_store, client) = signed_in(&fake);
    let mut s = Member::start(
        &client.with_reconnect_policy(fast_reconnects()),
        &fake.origin(),
    );
    s.wait_phase_within(
        Duration::from_secs(3),
        "Unavailable after 4000 twice",
        |p| matches!(p, Phase::Unavailable { .. }),
    )
    .await;
    s.shutdown().await;
}

/// The Lobby never comes back: ten attempts on the tiers' delays, then Unavailable until
/// "Try now", which starts over.
#[tokio::test]
async fn ten_failed_attempts_give_up_until_retry_now() {
    let mut fake = FakeLobby::start(FakeBehavior {
        refuse_ws: true,
        ..FakeBehavior::default()
    })
    .await;
    let (_store, client) = signed_in(&fake);
    let mut s = Member::start(
        &client.with_reconnect_policy(fast_reconnects()),
        &fake.origin(),
    );
    s.wait_phase_within(Duration::from_secs(5), "Unavailable", |p| {
        matches!(p, Phase::Unavailable { .. })
    })
    .await;
    assert_eq!(s.state().phase, Phase::Unavailable { attempts: 10 });
    let refused = fake
        .observed_now()
        .into_iter()
        .filter(|o| *o == Observed::WsRefused)
        .count();
    assert_eq!(refused, 11, "the first connect and ten reconnect attempts");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(fake.observed_now().is_empty());
    s.retry_now().await;
    s.wait_until("a fresh start", |st| {
        matches!(st.phase, Phase::Connecting { attempt: 0 })
    })
    .await;
    s.wait_until("the backoff again", |st| {
        matches!(st.phase, Phase::Reconnecting { attempt: 1, .. })
    })
    .await;
    s.shutdown().await;
}

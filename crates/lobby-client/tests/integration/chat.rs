//! End to end: a real in-process `lobby-server` and two Members whose Sessions run on the same
//! driver, `ApiClient` and connection actor as the desktop's.

use std::time::Duration;

use lobby_client::{
    IssueKind, MemoryTokenStore, Phase, copy,
    message_text::MessageText,
    testing::{ManualClock, test_client},
};
use lobby_protocol::{Presence, UserId};
use lobby_server::testing::TestServer;

use crate::common::{Member, origin, presence};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quick_sends_arrive_in_order_without_waiting_on_a_timer() {
    let server = TestServer::start_with_members(&["alice"], |c| c.http.rate_limits = true).await;
    let store = MemoryTokenStore::new();
    let client = test_client(&store).with_clock(ManualClock::new());
    let mut a = Member::sign_in(&client, &origin(&server), "alice", &server.mail)
        .await
        .unwrap();
    let texts = ["ok", "lol", "wait", "what", "no"];
    for text in texts {
        a.send_raw(text).await.unwrap();
    }
    a.wait_until("every send delivered", |s| {
        s.pending.is_empty() && s.chat.len() == texts.len()
    })
    .await;
    let arrived: Vec<&str> = a
        .state()
        .chat
        .values()
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(arrived, texts);
    a.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

/// The invariant checker: a fresh observer's READY and history page are
/// what every client's folded state must converge to.
async fn check_invariants(server: &TestServer, clients: &mut [&mut Member]) {
    let store = MemoryTokenStore::new();
    let client = test_client(&store);
    let observer = Member::sign_in(&client, &origin(server), "observer", &server.mail)
        .await
        .unwrap();
    let truth = observer.state();
    let me = truth.me.clone().unwrap();
    let expected_members: Vec<(UserId, Presence)> = truth
        .members
        .values()
        .filter(|m| m.id != me)
        .map(|m| (m.id.clone(), m.presence))
        .collect();
    for c in clients.iter_mut() {
        c.wait_until("convergence with a fresh READY", |s| {
            let members: Vec<(UserId, Presence)> = s
                .members
                .values()
                .filter(|m| m.id != me)
                .map(|m| (m.id.clone(), m.presence))
                .collect();
            members == expected_members && s.chat == truth.chat
        })
        .await;
    }
    observer.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_members_chat_with_presence_and_ordering() {
    let server = TestServer::start_with_members(&["alice", "bob", "observer"], |_| {}).await;
    let origin = origin(&server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));

    let mut a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    assert_eq!(presence(a.state(), "alice"), Some(Presence::Online));
    assert_eq!(presence(a.state(), "bob"), Some(Presence::Offline));
    let mut b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();
    a.wait_until("bob online", |s| {
        presence(s, "bob") == Some(Presence::Online)
    })
    .await;
    assert_eq!(presence(b.state(), "alice"), Some(Presence::Online));
    let sidebar: Vec<String> = b
        .state()
        .sidebar()
        .iter()
        .map(|m| m.username.to_string())
        .collect();
    assert_eq!(
        sidebar,
        ["bob", "alice", "observer"],
        "me first, online, offline"
    );

    let m1 = a.send("hello bob").await;
    let m2 = b.send("hi alice\nsecond line").await;
    tokio::time::sleep(Duration::from_millis(210)).await;
    let m3 = a.send("<script>alert(1)</script>").await;
    assert!(m1.id < m2.id && m2.id < m3.id);
    for c in [&mut a, &mut b] {
        let m3_id = m3.id;
        c.wait_until("all three", move |s| s.chat.contains_key(&m3_id))
            .await;
    }
    let texts = |c: &Member| -> Vec<String> {
        c.state()
            .chat
            .values()
            .map(|m| MessageText::of(&m.content).plain)
            .collect()
    };
    assert_eq!(texts(&a), texts(&b));
    assert_eq!(
        texts(&a),
        [
            "hello bob",
            "hi alice\nsecond line",
            "<script>alert(1)</script>"
        ]
    );
    assert_eq!(m2.author.username, "bob");

    let refused = |sent: Result<(), lobby_client::Issue>| {
        let issue = sent.expect_err("refused");
        (issue.kind, issue.message.to_string())
    };
    assert_eq!(
        refused(a.send_raw(&"x".repeat(8001)).await),
        (IssueKind::SendRefused, copy::MESSAGE_TOO_LONG.to_owned())
    );
    assert_eq!(
        refused(a.send_raw(&"> ".repeat(100)).await),
        (IssueKind::SendRefused, copy::MESSAGE_TOO_NESTED.to_owned())
    );
    assert_eq!(
        a.send_raw("   ").await,
        Ok(()),
        "nothing to send says nothing"
    );
    assert!(a.state().pending.is_empty());

    check_invariants(&server, &mut [&mut a, &mut b]).await;

    b.shutdown().await;
    a.wait_until("bob offline", |s| {
        presence(s, "bob") == Some(Presence::Offline)
    })
    .await;
    a.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_with_stored_tokens_skips_sign_in_and_shows_missed_messages() {
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let origin = origin(&server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));
    let mut a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    let b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();
    assert!(client_b.has_session(&origin).await.unwrap().unwrap());
    b.shutdown().await;

    let missed = a.send("while you were away").await;

    let mut b = Member::start(&client_b, &origin);
    b.wait_connected().await;
    assert_eq!(b.state().my_username().unwrap().as_str(), "bob");
    assert!(b.state().chat.contains_key(&missed.id));
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signing_in_elsewhere_replaces_the_session_and_reconnect_takes_it_back() {
    let server = TestServer::start_with_members(&["alice"], |_| {}).await;
    let origin = origin(&server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let mut first = Member::sign_in(&test_client(&store_a), &origin, "alice", &server.mail)
        .await
        .unwrap();
    let mut second = Member::sign_in(&test_client(&store_b), &origin, "alice", &server.mail)
        .await
        .unwrap();
    first.wait_phase(Phase::SessionReplaced).await;
    first.retry_now().await;
    first.wait_connected().await;
    second.wait_phase(Phase::SessionReplaced).await;
    first.shutdown().await;
    second.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_restart_reconnects_by_itself_and_restores_the_lobby() {
    let server = TestServer::start_with_members(&["alice"], |_| {}).await;
    let origin = origin(&server);
    let store = MemoryTokenStore::new();
    let mut a = Member::sign_in(&test_client(&store), &origin, "alice", &server.mail)
        .await
        .unwrap();
    let before = a.send("before the restart").await;
    let (report, server) = server.restart().await;
    assert!(report.is_clean(), "{report:?}");
    a.wait_until("reconnecting", |s| {
        matches!(
            s.phase,
            Phase::Reconnecting { .. } | Phase::Connecting { attempt: 1.. }
        )
    })
    .await;
    // A send while disconnected waits in the feed and goes out when the connection is
    // back.
    a.send_raw("written while the Lobby restarted")
        .await
        .unwrap();
    assert!(a.state().pending.iter().all(|p| p.queued));
    // Back by itself, after the first tier's 2 s ± 20 %.
    a.wait_phase_within(Duration::from_secs(10), "reconnected", |p| {
        *p == Phase::Connected
    })
    .await;
    a.wait_connected().await;
    assert!(a.state().chat.contains_key(&before.id));
    a.wait_until("the queued send delivered", |s| {
        s.pending.is_empty()
            && s.chat
                .values()
                .any(|m| m.content.contains("written while the Lobby restarted"))
    })
    .await;
    let after = a.send("after the restart").await;
    assert!(after.id > before.id);
    a.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_access_token_is_refreshed_and_reidentified_before_it_expires() {
    // 125 s TTL: the refresh is due 2 min early, so after the 5 s floor.
    let server = TestServer::start_with_members(&["alice", "bob"], |c| {
        c.auth.access_ttl = Duration::from_secs(125);
    })
    .await;
    let origin = origin(&server);
    let store = MemoryTokenStore::new();
    let mut a = Member::sign_in(&test_client(&store), &origin, "alice", &server.mail)
        .await
        .unwrap();
    let first = store.peek(&origin).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(9);
    while store.peek(&origin).unwrap() == first {
        assert!(tokio::time::Instant::now() < deadline, "no refresh");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.send("after the refresh").await;
    assert_eq!(a.state().phase, Phase::Connected);
    a.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_socket_refreshes_and_reconnects_by_itself() {
    // A 3 s TTL is shorter than the 5 s refresh floor: the server closes 4003 first.
    let server = TestServer::start_with_members(&["alice"], |c| {
        c.auth.access_ttl = Duration::from_secs(3);
    })
    .await;
    let origin = origin(&server);
    let store = MemoryTokenStore::new();
    let mut a = Member::sign_in(&test_client(&store), &origin, "alice", &server.mail)
        .await
        .unwrap();
    a.wait_until("the 4003 reconnect", |s| {
        matches!(s.phase, Phase::Connecting { .. })
    })
    .await;
    a.wait_connected().await;
    a.send("still here").await;
    a.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

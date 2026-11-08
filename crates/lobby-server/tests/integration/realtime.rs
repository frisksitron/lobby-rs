//! IDENTIFY ⇒ READY, presence, re-IDENTIFY, replacement, MESSAGE_SEND and history, over real
//! loopback sockets.

use std::time::Duration;

use crate::common::*;
use lobby_server::testing::TestServer;
use serde_json::{Value, json};

fn member_names(ready: &Value) -> Vec<(String, String)> {
    ready["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["username"].as_str().unwrap().to_owned(),
                m["presence"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_presence_and_offline_on_close() {
    let env = TestServer::start_with_members(&["bob", "alice", "Carol"], |_| {}).await;
    let alice = sign_in(&env, "alice").await;
    let (mut a, ready) = identify(&env.ws_url(), &access(&alice)).await;
    assert_eq!(ready["me"], alice["user"]["id"]);
    assert_eq!(ready["accessRequests"], json!([]));
    assert_eq!(ready["lastMessageSeq"], 0);
    assert_eq!(ready["lobby"]["name"], "Test Lobby");
    assert_eq!(
        member_names(&ready),
        [
            ("alice".into(), "online".into()),
            ("bob".into(), "offline".into()),
            ("Carol".into(), "offline".into())
        ]
    );
    assert!(ready["connId"].is_string());

    let bob = sign_in(&env, "bob").await;
    let (mut b, ready_b) = identify(&env.ws_url(), &access(&bob)).await;
    assert_eq!(
        member_names(&ready_b)[..2],
        [
            ("alice".into(), "online".into()),
            ("bob".into(), "online".into())
        ]
    );
    let up = next_of(&mut a, "MEMBER_UPSERT").await;
    assert_eq!(up["member"]["username"], "bob");
    assert_eq!(up["member"]["presence"], "online");

    futures_util::SinkExt::close(&mut b).await.unwrap();
    let down = next_of(&mut a, "MEMBER_UPSERT").await;
    assert_eq!(down["member"]["username"], "bob");
    assert_eq!(down["member"]["presence"], "offline");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_connection_replaces_the_older_with_4009_and_no_presence_change() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut watcher, _) = identify(&env.ws_url(), &access(&sign_in(&env, "bob").await)).await;
    let (mut first, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    assert_eq!(
        next_of(&mut watcher, "MEMBER_UPSERT").await["member"]["presence"],
        "online"
    );
    let (_second, ready) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    assert_eq!(member_names(&ready)[0], ("alice".into(), "online".into()));
    assert_eq!(expect_close(&mut first).await, 4009);
    expect_quiet(&mut watcher, Duration::from_millis(300)).await;
    assert_eq!(env.server.counts().online, 2);
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identify_failures_close_with_their_codes() {
    let short = TestServer::start_with_members(&["alice"], |c| {
        c.auth.access_ttl = Duration::from_millis(300);
    })
    .await;
    let stale = access(&sign_in(&short, "alice").await);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut ws = ws_connect(&short.ws_url()).await;
    next_json(&mut ws).await;
    send_text(
        &mut ws,
        &json!({"t":"IDENTIFY","d":{"token": stale, "presence": "online"}}).to_string(),
    )
    .await;
    assert_eq!(next_json(&mut ws).await["d"]["code"], "AUTH_EXPIRED");
    assert_eq!(expect_close(&mut ws).await, 4003);
    assert!(short.shutdown().await.is_clean());

    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut ws, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    let bob = access(&sign_in(&env, "bob").await);
    send_text(
        &mut ws,
        &json!({"t":"IDENTIFY","d":{"token": &bob, "presence": "online"}}).to_string(),
    )
    .await;
    assert_eq!(next_of(&mut ws, "ERROR").await["code"], "INVALID_REQUEST");
    send_text(
        &mut ws,
        &json!({"t":"IDENTIFY","rid":"r9","d":{"token": &bob, "presence": "online"}}).to_string(),
    )
    .await;
    let error = next_of(&mut ws, "ERROR").await;
    assert_eq!(
        (error["code"].clone(), error["rid"].clone()),
        (json!("AUTH_FAILED"), json!("r9"))
    );
    assert_eq!(expect_close(&mut ws).await, 4003);
    assert!(env.shutdown().await.is_clean());
}

async fn member(env: &TestServer, name: &str) -> (Ws, String) {
    let token = access(&sign_in(env, name).await);
    let (ws, _) = identify(&env.ws_url(), &token).await;
    (ws, token)
}

/// No send slot comes back within a test, however slowly its sends reach the Lobby.
const SEND_EVERY_NEVER: Duration = Duration::from_secs(3600);

/// The send throttle is part of what `http.rate_limits` switches: back to back sends all go
/// through with it off; with it on, a burst of five does, and the sixth is refused with when
/// to try again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_send_throttle_follows_the_rate_limits_switch() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let (mut ws, _) = member(&env, "alice").await;
    for i in 0..7 {
        send_and_confirm(&mut ws, &format!("quick {i}")).await;
    }
    assert!(env.shutdown().await.is_clean());

    let env = TestServer::start_with_members(&["alice"], |c| {
        c.http.rate_limits = true;
        c.ws.send_every = SEND_EVERY_NEVER;
    })
    .await;
    let (mut ws, _) = member(&env, "alice").await;
    let rids: Vec<String> = (0..6).map(|_| nonce()).collect();
    for (i, rid) in rids.iter().enumerate() {
        send_text(&mut ws, &message_send(rid, &format!("quick {i}"))).await;
    }
    for rid in &rids[..5] {
        let created = next_of(&mut ws, "MESSAGE_CREATE").await;
        assert_eq!(created["message"]["nonce"], rid.as_str());
    }
    let refused = next_of(&mut ws, "ERROR").await;
    assert_eq!(refused["code"], "RATE_LIMITED");
    assert_eq!(refused["rid"], rids[5].as_str());
    let after = refused["retryAfterMs"].as_u64().unwrap();
    assert!(
        after > 0 && u128::from(after) <= SEND_EVERY_NEVER.as_millis(),
        "{refused}"
    );
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_send_validation_table() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let (mut ws, _) = member(&env, "alice").await;
    let too_many: Vec<String> = (0..11).map(|i| format!("blb_{i:024x}")).collect();
    let cases = [
        (
            json!({"t":"MESSAGE_SEND","d":{"content":"hi"}}),
            "INVALID_REQUEST",
            "MESSAGE_SEND needs a rid of 16-64 characters",
        ),
        (
            json!({"t":"MESSAGE_SEND","rid":"short","d":{"content":"hi"}}),
            "INVALID_REQUEST",
            "MESSAGE_SEND needs a rid of 16-64 characters",
        ),
        (
            json!({"t":"MESSAGE_SEND","rid":nonce(),"d":{"content":"hi","attachmentIds":too_many}}),
            "INVALID_REQUEST",
            "Too many attachments",
        ),
        (
            json!({"t":"MESSAGE_SEND","rid":nonce(),"d":{"content":"x".repeat(8001)}}),
            "MESSAGE_TOO_LONG",
            "Message is too long",
        ),
        (
            json!({"t":"MESSAGE_SEND","rid":nonce(),"d":{"content":" \n\t "}}),
            "INVALID_REQUEST",
            "Message is empty",
        ),
        (
            json!({"t":"MESSAGE_SEND","rid":nonce(),"d":{"content":">".repeat(100)}}),
            "MESSAGE_TOO_NESTED",
            "Message is nested too deeply",
        ),
        (
            json!({"t":"MESSAGE_SEND","rid":nonce(),"d":{"content":"x","attachmentIds":["blb_2582c529769d2a353bd49743"]}}),
            "ATTACHMENT_INVALID",
            "One or more attachments are no longer available",
        ),
        (
            json!({"t":"MESSAGE_PIN","rid":"t1","d":{}}),
            "INVALID_REQUEST",
            "Unknown command",
        ),
        (
            json!({"t":"MESSAGE_SEND","rid":nonce(),"d":{"content":7}}),
            "INVALID_REQUEST",
            "Invalid MESSAGE_SEND payload",
        ),
    ];
    for (frame, code, message) in cases {
        send_text(&mut ws, &frame.to_string()).await;
        let error = next_of(&mut ws, "ERROR").await;
        assert_eq!(error["code"], code, "{frame}");
        assert_eq!(error["message"], message, "{frame}");
        assert_eq!(
            error["rid"], frame["rid"],
            "the terminal outcome carries the rid"
        );
    }
    send_text(&mut ws, "{nope").await;
    assert_eq!(
        next_of(&mut ws, "ERROR").await["message"],
        "Malformed frame"
    );

    send_text(&mut ws, &message_send(&nonce(), &"y".repeat(8000))).await;
    assert_eq!(
        next_of(&mut ws, "MESSAGE_CREATE").await["message"]["content"]
            .as_str()
            .unwrap()
            .len(),
        8000
    );
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_are_stored_as_typed_broadcast_to_all_and_replays_go_to_the_sender_only() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut a, token) = member(&env, "alice").await;
    let (mut b, _) = member(&env, "bob").await;
    next_of(&mut a, "MEMBER_UPSERT").await;

    let rid = nonce();
    let raw = "**hi** <script>alert(1)</script>\n- a";
    send_text(&mut a, &message_send(&rid, raw)).await;
    let mine = next_of(&mut a, "MESSAGE_CREATE").await["message"].clone();
    let theirs = next_of(&mut b, "MESSAGE_CREATE").await["message"].clone();
    assert_eq!(mine, theirs, "one shape, one serialization, for everyone");
    assert_eq!(mine["nonce"], rid.as_str());
    assert_eq!(mine["content"], raw);
    assert_eq!(mine["author"]["username"], "alice");

    tokio::time::sleep(Duration::from_millis(210)).await;
    send_text(&mut a, &message_send(&rid, "different text, same nonce")).await;
    let replay = next_of(&mut a, "MESSAGE_CREATE").await["message"].clone();
    assert_eq!(replay, mine);
    expect_quiet(&mut b, Duration::from_millis(300)).await;
    let page = get_auth(env.addr, "/api/v1/messages", &token).await.json();
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);

    // A fresh connection's READY is consistent with what was committed.
    let (_c, ready) = identify(&env.ws_url(), &access(&sign_in(&env, "bob").await)).await;
    assert_eq!(ready["lastMessageSeq"], mine["id"]);
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_pages_in_both_directions() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut a, token) = member(&env, "alice").await;
    let (mut b, _) = member(&env, "bob").await;
    // Alternating senders, each at most every 220 ms: under the per-Member throttle.
    for i in 1..=7 {
        let ws = if i % 2 == 0 { &mut b } else { &mut a };
        send_and_confirm(ws, &format!("{i}")).await;
        tokio::time::sleep(Duration::from_millis(110)).await;
    }
    let ids = |page: &Value| -> Vec<i64> {
        page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_i64().unwrap())
            .collect()
    };
    let fetch = |q: &'static str| {
        let token = token.clone();
        let addr = env.addr;
        async move { get_auth(addr, &format!("/api/v1/messages{q}"), &token).await }
    };
    let all = fetch("").await.json();
    assert_eq!(ids(&all), [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(all["hasMore"], false);
    let newest = fetch("?limit=3").await.json();
    assert_eq!(
        (ids(&newest), newest["hasMore"].clone()),
        (vec![5, 6, 7], json!(true))
    );
    let older = fetch("?limit=3&before=5").await.json();
    assert_eq!(
        (ids(&older), older["hasMore"].clone()),
        (vec![2, 3, 4], json!(true))
    );
    let oldest = fetch("?limit=3&before=2").await.json();
    assert_eq!(
        (ids(&oldest), oldest["hasMore"].clone()),
        (vec![1], json!(false))
    );
    let gap = fetch("?limit=2&after=4").await.json();
    assert_eq!(
        (ids(&gap), gap["hasMore"].clone()),
        (vec![5, 6], json!(true))
    );
    let tail = fetch("?after=6").await.json();
    assert_eq!(
        (ids(&tail), tail["hasMore"].clone()),
        (vec![7], json!(false))
    );
    assert_eq!(all["messages"][1]["author"]["username"], "bob");

    for (q, message) in [
        (
            "?limit=0",
            "Query parameter 'limit' must be between 1 and 100",
        ),
        (
            "?limit=101",
            "Query parameter 'limit' must be between 1 and 100",
        ),
        ("?limit=x", "limit"),
        ("?before=0", "before"),
        ("?after=-1", "after"),
        ("?before=3&after=1", "Use either 'before' or 'after'"),
    ] {
        let res = fetch(q).await;
        assert_envelope(&res, 400, "INVALID_REQUEST");
        let said = res.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(said.contains(message), "{q}: {said}");
    }
    assert!(env.shutdown().await.is_clean());
}

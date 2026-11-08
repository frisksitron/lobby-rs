//! Voice signalling over the real WebSocket: join ⇒ ACK and the server's offer, the In Voice
//! snapshot for everyone, terminal outcomes, and Voice ending with the connection. Media is
//! covered by the SFU loopback tests and the client e2e.

use std::time::Duration;

use crate::common::*;
use futures_util::StreamExt as _;
use lobby_protocol::{UserId, VoiceSessionId};
use lobby_server::testing::TestServer;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

fn voice_join(rid: &str) -> String {
    json!({"t": "VOICE_JOIN", "rid": rid, "d": {"muted": false, "deafened": false}}).to_string()
}

async fn upsert_of(ws: &mut Ws, name: &str) -> Value {
    loop {
        let d = next_of(ws, "MEMBER_UPSERT").await;
        if d["member"]["username"] == name {
            return d["member"].clone();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_is_acked_with_a_session_then_offered_and_everyone_sees_joining() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut watcher, _) = identify(&env.ws_url(), &access(&sign_in(&env, "bob").await)).await;
    let (mut a, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    upsert_of(&mut watcher, "alice").await;

    send_text(&mut a, &voice_join("j1")).await;
    // The broadcast first, then the ACK, then the server's offer.
    let up = next_json(&mut a).await;
    assert_eq!(up["t"], "MEMBER_UPSERT", "{up}");
    assert_eq!(
        up["d"]["member"]["voice"],
        json!({"joining": true, "muted": false, "deafened": false, "speaking": false})
    );
    let ack = next_json(&mut a).await;
    assert_eq!(
        (ack["t"].as_str(), ack["d"]["rid"].as_str()),
        (Some("ACK"), Some("j1"))
    );
    let vsid = ack["d"]["voiceSessionId"]
        .as_u64()
        .expect("a voice session id");
    let offer = next_of(&mut a, "RTC_OFFER").await;
    assert_eq!(offer["voiceSessionId"], vsid);
    assert_eq!(offer["rev"], 1);
    let sdp = offer["sdp"].as_str().unwrap();
    assert!(
        sdp.contains("m=audio") && sdp.contains("a=ice-lite"),
        "{sdp}"
    );
    assert!(sdp.contains("typ host"), "the SFU's host candidate: {sdp}");
    let tracks = offer["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0]["purpose"], "voice-audio");
    assert_eq!(tracks[0]["userId"], up["d"]["member"]["id"]);
    assert_eq!(
        upsert_of(&mut watcher, "alice").await["voice"]["joining"],
        true
    );

    send_text(&mut a, &voice_join("j2")).await;
    let err = next_of(&mut a, "ERROR").await;
    assert_eq!(
        (err["code"].as_str(), err["rid"].as_str()),
        (Some("VOICE_STATE_INVALID_TRANSITION"), Some("j2"))
    );
    let stale = json!({"t": "RTC_ANSWER", "rid": "a1",
        "d": {"voiceSessionId": vsid, "rev": 7, "sdp": "v=0"}});
    send_text(&mut a, &stale.to_string()).await;
    let err = next_of(&mut a, "ERROR").await;
    assert_eq!(err["code"], "VOICE_NEGOTIATION_INVALID_STATE");
    let mute =
        json!({"t": "VOICE_STATE_SET", "rid": "m1", "d": {"muted": true, "deafened": false}});
    send_text(&mut a, &mute.to_string()).await;
    assert_eq!(
        next_of(&mut a, "ERROR").await["code"],
        "VOICE_STATE_INVALID_TRANSITION"
    );
    send_text(
        &mut a,
        &json!({"t": "VOICE_LEAVE", "rid": "l1"}).to_string(),
    )
    .await;
    let left = next_json(&mut a).await;
    assert_eq!(left["t"], "MEMBER_UPSERT");
    assert!(left["d"]["member"].get("voice").is_none(), "{left}");
    assert_eq!(next_json(&mut a).await["d"]["rid"], "l1");
    assert!(
        upsert_of(&mut watcher, "alice")
            .await
            .get("voice")
            .is_none()
    );
    expect_quiet(&mut a, Duration::from_millis(300)).await;
    assert_eq!(env.server.counts().in_voice, 0);
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sixth_join_within_15_s_gets_the_cooldown_and_no_session() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let (mut a, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    let leave = json!({"t": "VOICE_LEAVE", "rid": "l"}).to_string();
    for i in 0..5 {
        send_text(&mut a, &voice_join(&format!("j{i}"))).await;
        next_of(&mut a, "RTC_OFFER").await;
        send_text(&mut a, &leave).await;
        loop {
            let frame = next_json(&mut a).await;
            if frame["t"] == "ACK" && frame["d"]["rid"] == "l" {
                break;
            }
        }
    }
    send_text(&mut a, &voice_join("j6")).await;
    let err = next_of(&mut a, "ERROR").await;
    assert_eq!(
        (err["code"].as_str(), err["rid"].as_str()),
        (Some("VOICE_JOIN_COOLDOWN"), Some("j6"))
    );
    let wait = err["retryAfterMs"].as_u64().expect("a retry delay");
    assert!((14_000..=15_000).contains(&wait), "{err}");
    assert_eq!(env.server.counts().in_voice, 0);
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn voice_commands_need_a_rid_and_voice_ends_when_the_socket_closes() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut watcher, _) = identify(&env.ws_url(), &access(&sign_in(&env, "bob").await)).await;
    let (mut a, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    upsert_of(&mut watcher, "alice").await;
    send_text(
        &mut a,
        &json!({"t": "VOICE_JOIN", "d": {"muted": false, "deafened": false}}).to_string(),
    )
    .await;
    assert_eq!(next_of(&mut a, "ERROR").await["code"], "INVALID_REQUEST");
    send_text(&mut a, &voice_join("j1")).await;
    next_of(&mut a, "RTC_OFFER").await;
    assert_eq!(
        upsert_of(&mut watcher, "alice").await["voice"]["joining"],
        true
    );
    assert_eq!(env.server.counts().in_voice, 1);
    drop(a);
    let gone = upsert_of(&mut watcher, "alice").await;
    assert!(gone.get("voice").is_none(), "{gone}");
    assert_eq!(
        upsert_of(&mut watcher, "alice").await["presence"],
        "offline"
    );
    assert_eq!(env.server.counts().in_voice, 0);
    assert!(env.shutdown().await.is_clean());
}

/// A Member In Voice. Raw WebSockets carry no media, so the server is told the session connected.
struct Voiced {
    ws: Ws,
    id: UserId,
    vsid: VoiceSessionId,
}

async fn voiced(env: &TestServer, name: &str) -> Voiced {
    let voiced = joining(env, name).await;
    env.server.fake_media_connected(&voiced.id, voiced.vsid);
    voiced
}

/// A Member who joined Voice, whose media has not connected.
async fn joining(env: &TestServer, name: &str) -> Voiced {
    let (mut ws, ready) = identify(&env.ws_url(), &access(&sign_in(env, name).await)).await;
    let id: UserId = ready["me"].as_str().unwrap().parse().unwrap();
    send_text(&mut ws, &voice_join("j")).await;
    let vsid = loop {
        let frame = next_json(&mut ws).await;
        if frame["t"] == "ACK" && frame["d"]["rid"] == "j" {
            break frame["d"]["voiceSessionId"].as_u64().unwrap();
        }
    };
    let vsid = VoiceSessionId::new(vsid).unwrap();
    Voiced { ws, id, vsid }
}

async fn acked(ws: &mut Ws, rid: &str) {
    loop {
        let frame = next_json(ws).await;
        assert!(
            !(frame["t"] == "ERROR" && frame["d"]["rid"] == rid),
            "{rid} refused: {frame}"
        );
        if frame["t"] == "ACK" && frame["d"]["rid"] == rid {
            return;
        }
    }
}

/// Watching needs Voice: a Watch from a Member out of Voice, or whose Voice still connects, is
/// refused; once their Voice is on, the same Watch is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_needs_voice() {
    let env = TestServer::start_with_members(&["alice", "bob", "carol"], |_| {}).await;
    let mut alice = voiced(&env, "alice").await;
    let start = json!({"t": "SCREEN_SHARE_START", "rid": "s",
        "d": {"audio": false, "quality": "2160"}});
    send_text(&mut alice.ws, &start.to_string()).await;
    acked(&mut alice.ws, "s").await;
    env.server.fake_share_video_started(&alice.id, alice.vsid);
    let watch = |rid: &str| {
        json!({"t": "SCREEN_SHARE_WATCH", "rid": rid, "d": {"sharerId": alice.id}}).to_string()
    };
    let refused = async |ws: &mut Ws, rid: &str| {
        send_text(ws, &watch(rid)).await;
        let err = next_of(ws, "ERROR").await;
        assert_eq!(
            (err["code"].as_str(), err["rid"].as_str()),
            (Some("NOT_IN_VOICE"), Some(rid)),
            "{err}"
        );
    };

    let (mut bob, _) = identify(&env.ws_url(), &access(&sign_in(&env, "bob").await)).await;
    refused(&mut bob, "out").await;
    let mut carol = joining(&env, "carol").await;
    refused(&mut carol.ws, "joining").await;
    env.server.fake_media_connected(&carol.id, carol.vsid);
    send_text(&mut carol.ws, &watch("on")).await;
    acked(&mut carol.ws, "on").await;
    assert!(env.shutdown().await.is_clean());
}

/// `sharer` shares and lets viewers draw, and each of `viewers` watches. Answers the share's id
/// and its drawing epoch.
async fn sharing_with_ink(
    env: &TestServer,
    sharer: &mut Voiced,
    viewers: &mut [Voiced],
) -> (u64, u64) {
    let start = json!({"t": "SCREEN_SHARE_START", "rid": "s",
        "d": {"audio": false, "quality": "2160"}});
    send_text(&mut sharer.ws, &start.to_string()).await;
    acked(&mut sharer.ws, "s").await;
    env.server.fake_share_video_started(&sharer.id, sharer.vsid);
    let drawing = json!({"t": "DRAWING_SET", "rid": "d", "d": {"enabled": true}});
    send_text(&mut sharer.ws, &drawing.to_string()).await;
    acked(&mut sharer.ws, "d").await;
    for viewer in viewers {
        let watch = json!({"t": "SCREEN_SHARE_WATCH", "rid": "w",
            "d": {"sharerId": sharer.id}});
        send_text(&mut viewer.ws, &watch.to_string()).await;
        acked(&mut viewer.ws, "w").await;
    }
    // A fresh READY says what the share looks like now.
    let (_ws, ready) = identify(&env.ws_url(), &access(&sign_in(env, "snap").await)).await;
    let sharer_id = json!(sharer.id);
    let share = ready["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == sharer_id)
        .map(|m| m["voice"]["screenShare"].clone())
        .unwrap();
    (
        share["shareId"].as_u64().unwrap(),
        share["drawing"].as_u64().expect("drawing is on"),
    )
}

fn ink_frame(share: u64, epoch: u64, seq: u32, points: usize) -> String {
    let points: Vec<Value> = (0..points).map(|i| json!({"x": i, "y": 7})).collect();
    json!({"t": "INK", "d": {"shareId": share, "epoch": epoch, "stroke": 1, "seq": seq,
        "points": points, "end": false}})
    .to_string()
}

/// Everything `ws` is sent within `wait`.
async fn drain(ws: &mut Ws, wait: Duration) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut frames = Vec::new();
    while let Ok(Some(Ok(msg))) = tokio::time::timeout_at(deadline, ws.next()).await {
        if let Message::Text(t) = msg {
            frames.push(serde_json::from_str(&t).unwrap());
        }
    }
    frames
}

/// 200 batches go at a Watcher who reads none of them while Chat is sent: the ink is lossy, and
/// the Watcher stays connected in Voice with the Chat message delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flood_of_ink_at_a_watcher_who_is_not_reading_never_kicks_or_ends_voice() {
    let drawers = ["dora", "erin", "fred", "gwen", "hank"];
    let mut names = vec!["alice", "bob", "snap"];
    names.extend(drawers);
    let env = TestServer::start_with_members(&names, |_| {}).await;
    let mut alice = voiced(&env, "alice").await;
    let bob = voiced(&env, "bob").await;
    let mut viewers = vec![bob];
    for name in drawers {
        viewers.push(voiced(&env, name).await);
    }
    let (share, epoch) = sharing_with_ink(&env, &mut alice, &mut viewers).await;
    let mut bob = viewers.remove(0);

    for seq in 0..40 {
        for drawer in &mut viewers {
            send_text(&mut drawer.ws, &ink_frame(share, epoch, seq, 64)).await;
        }
        if seq == 20 {
            send_and_confirm(&mut alice.ws, "anyone there?").await;
        }
    }

    let seen = drain(&mut bob.ws, Duration::from_millis(500)).await;
    let ink: Vec<&Value> = seen.iter().filter(|f| f["t"] == "INK").collect();
    assert!(!ink.is_empty() && ink.len() <= 200, "{} batches", ink.len());
    let first = &mut viewers[0];
    let own = drain(&mut first.ws, Duration::from_millis(200)).await;
    assert!(
        own.iter()
            .all(|f| f["t"] != "INK" || f["d"]["authorId"] != json!(first.id)),
        "never back to the author"
    );
    assert!(
        seen.iter()
            .any(|f| f["t"] == "MESSAGE_CREATE" && f["d"]["message"]["content"] == "anyone there?"),
        "Chat arrived"
    );
    assert!(
        seen.iter().all(|f| f["t"] != "VOICE_ENDED"),
        "Voice was not ended"
    );
    assert_eq!(env.server.counts().in_voice, 7);

    // The connection is alive: Bob can still talk.
    let reply = send_and_confirm(&mut bob.ws, "yes").await;
    assert_eq!(reply["content"], "yes");
    assert!(env.shutdown().await.is_clean());
}

/// A batch too long or with a coordinate out of range is rejected at the edge: no relay and no
/// ERROR for the author. The next good batch still arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_or_oversized_batch_is_dropped_silently() {
    let env = TestServer::start_with_members(&["alice", "bob", "snap"], |_| {}).await;
    let mut alice = voiced(&env, "alice").await;
    let mut bob = voiced(&env, "bob").await;
    let (share, epoch) = sharing_with_ink(&env, &mut alice, std::slice::from_mut(&mut bob)).await;

    send_text(&mut bob.ws, &ink_frame(share, epoch, 0, 65)).await;
    let out_of_range = ink_frame(share, epoch, 1, 1).replace("\"x\":0", "\"x\":65536");
    assert!(out_of_range.contains("65536"));
    send_text(&mut bob.ws, &out_of_range).await;
    send_text(&mut bob.ws, &ink_frame(share, epoch + 1, 2, 3)).await;
    send_text(&mut bob.ws, &ink_frame(share, epoch, 3, 64)).await;

    let seen = drain(&mut alice.ws, Duration::from_millis(500)).await;
    let ink: Vec<&Value> = seen.iter().filter(|f| f["t"] == "INK").collect();
    assert_eq!(ink.len(), 1, "{seen:?}");
    assert_eq!(ink[0]["d"]["batch"]["seq"], 3);
    assert_eq!(ink[0]["d"]["authorId"], json!(bob.id));
    let to_bob = drain(&mut bob.ws, Duration::from_millis(200)).await;
    assert!(to_bob.iter().all(|f| f["t"] != "ERROR"), "{to_bob:?}");
    assert!(env.shutdown().await.is_clean());
}

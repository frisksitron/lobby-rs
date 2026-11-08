//! The Lobby's owner: the bootstrap Member owns the Lobby, removes Members (who may Ask to Join
//! again and come back as themselves) and hands ownership on, over real loopback sockets.

use crate::common::{admission::*, *};
use lobby_server::testing::{TestServer, email_of};
use serde_json::{Value, json};

async fn remove(env: &TestServer, token: &str, user: &str) -> HttpResponse {
    post_auth(
        env,
        &format!("/api/v1/members/{user}/remove"),
        token,
        &json!({}),
    )
    .await
}

async fn transfer(env: &TestServer, token: &str, user: &str) -> HttpResponse {
    post_auth(env, "/api/v1/lobby/owner", token, &json!({"userId": user})).await
}

fn assert_detail(res: &HttpResponse, status: u16, code: &str, detail: &str) {
    assert_envelope(res, status, code);
    assert_eq!(res.json()["error"]["detail"], detail, "{}", res.body);
}

fn user_id(signed_in: &Value) -> String {
    signed_in["user"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_member_is_closed_with_4006_gone_for_everyone_and_cannot_sign_in() {
    let env = TestServer::start_with_members(&["alice", "bob", "carol"], |_| {}).await;
    let alice = access(&sign_in(&env, "alice").await);
    let bob_signed_in = sign_in(&env, "bob").await;
    let bob = user_id(&bob_signed_in);
    let (mut bob_ws, _) = identify(&env.ws_url(), &access(&bob_signed_in)).await;
    let (mut carol_ws, _) = identify(&env.ws_url(), &access(&sign_in(&env, "carol").await)).await;

    let res = remove(&env, &alice, &bob).await;
    assert_eq!(res.status, 204, "{}", res.body);

    assert_eq!(expect_close(&mut bob_ws).await, 4006);
    assert_eq!(
        next_of(&mut carol_ws, "MEMBER_REMOVED").await["userId"],
        bob
    );
    // His code no longer signs him in: he is a newcomer who may ask again.
    assert_eq!(
        verified(&env, &email_of("bob")).await["next"],
        "ask_to_join"
    );
    assert_envelope(
        &get_auth(env.addr, "/api/v1/messages", &access(&bob_signed_in)).await,
        401,
        "AUTH_FAILED",
    );
    assert_envelope(
        &refresh(env.addr, &refresh_token(&bob_signed_in["tokens"])).await,
        401,
        "AUTH_FAILED",
    );
    let (_ws, ready) = identify(&env.ws_url(), &alice).await;
    let names: Vec<&str> = ready["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alice", "carol"]);
    assert_envelope(&remove(&env, &alice, &bob).await, 404, "NOT_FOUND");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_member_asks_again_and_comes_back_as_themself() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let alice = access(&sign_in(&env, "alice").await);
    let bob_signed_in = sign_in(&env, "bob").await;
    let bob = user_id(&bob_signed_in);
    let (mut bob_ws, _) = identify(&env.ws_url(), &access(&bob_signed_in)).await;
    let said = send_and_confirm(&mut bob_ws, "before").await;
    let res = remove(&env, &alice, &bob).await;
    assert_eq!(res.status, 204, "{}", res.body);
    assert_eq!(expect_close(&mut bob_ws).await, 4006);
    let (mut alice_ws, _) = identify(&env.ws_url(), &alice).await;

    // Their username stays theirs: someone else cannot ask with it.
    let mallory = ticket(&env, &email_of("mallory"), "ask_to_join").await;
    assert_detail(
        &join(&env, &mallory, "bob").await,
        409,
        "CONFLICT",
        "USERNAME_TAKEN",
    );
    let (request, wait) = ask_to_join(&env, "bob", &mut alice_ws).await;
    let res = post_auth(
        &env,
        &format!("/api/v1/access-requests/{request}/vouch"),
        &alice,
        &json!({}),
    )
    .await;
    assert_eq!(res.status, 204, "{}", res.body);
    next_of(&mut alice_ws, "ACCESS_REQUEST_REMOVED").await;
    let back = next_json(&mut alice_ws).await;
    assert_eq!(back["t"], "MEMBER_UPSERT", "{back}");
    assert_eq!(back["d"]["member"]["id"], bob, "the same User");

    let status = join_status(&env, &wait).await;
    assert_eq!(status.status, 200, "{}", status.body);
    let session = status.json()["session"].clone();
    assert_eq!(user_id(&session), bob);
    let page = get_auth(env.addr, "/api/v1/messages", &access(&session))
        .await
        .json();
    let mine = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == said["id"])
        .expect("the old Message is still there")
        .clone();
    assert_eq!(mine["author"]["id"], bob);
    assert!(mine["author"].get("deactivated").is_none(), "{mine}");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ownership_moves_to_another_member_for_everyone() {
    let env = TestServer::start_with_members(&["alice", "bob", "carol"], |_| {}).await;
    let alice_signed_in = sign_in(&env, "alice").await;
    let alice = access(&alice_signed_in);
    let bob_signed_in = sign_in(&env, "bob").await;
    let carol_signed_in = sign_in(&env, "carol").await;
    let (mut sockets, mut readies) = (Vec::new(), Vec::new());
    for signed_in in [&alice_signed_in, &bob_signed_in, &carol_signed_in] {
        let (ws, ready) = identify(&env.ws_url(), &access(signed_in)).await;
        sockets.push(ws);
        readies.push(ready);
    }
    assert!(
        readies
            .iter()
            .all(|r| r["lobby"]["owner"] == user_id(&alice_signed_in))
    );
    let bob = user_id(&bob_signed_in);

    let res = transfer(&env, &alice, &bob).await;
    assert_eq!(res.status, 204, "{}", res.body);
    for ws in &mut sockets {
        assert_eq!(next_of(ws, "LOBBY_UPDATE").await["lobby"]["owner"], bob);
    }
    assert_eq!(get(env.addr, "/api/v1/lobby").await.json()["owner"], bob);

    let carol = user_id(&carol_signed_in);
    assert_detail(
        &remove(&env, &alice, &carol).await,
        403,
        "FORBIDDEN",
        "NOT_OWNER",
    );
    let res = remove(&env, &access(&bob_signed_in), &user_id(&alice_signed_in)).await;
    assert_eq!(res.status, 204, "{}", res.body);
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_owner_removes_or_transfers_and_the_owner_cannot_be_removed() {
    let env = TestServer::start_with_members(&["alice", "bob", "carol"], |_| {}).await;
    let alice_signed_in = sign_in(&env, "alice").await;
    let (alice, owner) = (access(&alice_signed_in), user_id(&alice_signed_in));
    let bob_signed_in = sign_in(&env, "bob").await;
    let bob = access(&bob_signed_in);
    let carol = user_id(&sign_in(&env, "carol").await);
    let (mut ws, _) = identify(&env.ws_url(), &bob).await;

    for refused in [
        remove(&env, &bob, &carol).await,
        remove(&env, &bob, &owner).await,
        transfer(&env, &bob, &user_id(&bob_signed_in)).await,
    ] {
        assert_detail(&refused, 403, "FORBIDDEN", "NOT_OWNER");
    }
    assert_detail(
        &remove(&env, &alice, &owner).await,
        409,
        "CONFLICT",
        "CANNOT_REMOVE_OWNER",
    );
    let nobody = "usr_000000000000000000000000";
    assert_envelope(&remove(&env, &alice, nobody).await, 404, "NOT_FOUND");
    assert_envelope(&transfer(&env, &alice, nobody).await, 404, "NOT_FOUND");
    assert_envelope(&remove(&env, &alice, "bob").await, 400, "INVALID_REQUEST");
    assert_eq!(transfer(&env, &alice, &owner).await.status, 204);
    expect_quiet(&mut ws, std::time::Duration::from_millis(200)).await;
    assert_eq!(get(env.addr, "/api/v1/lobby").await.json()["owner"], owner);
    assert!(env.shutdown().await.is_clean());
}

async fn leave(env: &TestServer, token: &str) -> HttpResponse {
    let bearer = format!("Bearer {token}");
    http(
        env.addr,
        "DELETE",
        "/api/v1/users/me",
        &[("Authorization", bearer.as_str())],
        "",
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_who_leaves_ends_every_session_is_gone_for_the_others_and_keeps_their_old_messages()
 {
    let env = TestServer::start_with_members(&["alice", "bob", "carol"], |_| {}).await;
    let alice = access(&sign_in(&env, "alice").await);
    let bob_signed_in = sign_in(&env, "bob").await;
    let bob = access(&bob_signed_in);
    let bob_elsewhere = sign_in(&env, "bob").await;
    let bob_id = user_id(&bob_signed_in);
    let (mut bob_ws, _) = identify(&env.ws_url(), &bob).await;
    let (mut carol_ws, _) = identify(&env.ws_url(), &access(&sign_in(&env, "carol").await)).await;
    send_and_confirm(&mut bob_ws, "bob was here").await;
    next_of(&mut carol_ws, "MESSAGE_CREATE").await;

    let res = leave(&env, &bob).await;
    assert_eq!(res.status, 204, "{}", res.body);

    assert_eq!(expect_close(&mut bob_ws).await, 4006);
    assert_eq!(
        next_of(&mut carol_ws, "MEMBER_REMOVED").await["userId"],
        bob_id
    );
    // Every session of his is over, and he cannot sign in again.
    assert_envelope(
        &get_auth(env.addr, "/api/v1/messages", &access(&bob_elsewhere)).await,
        401,
        "AUTH_FAILED",
    );
    assert_envelope(
        &refresh(env.addr, &refresh_token(&bob_signed_in["tokens"])).await,
        401,
        "AUTH_FAILED",
    );
    assert_eq!(
        verified(&env, &email_of("bob")).await["next"],
        "ask_to_join"
    );
    // What he said stays, under his name, as the work of someone who is gone.
    let page = get_auth(env.addr, "/api/v1/messages", &alice).await.json();
    let author = &page["messages"][0]["author"];
    assert_eq!(author["username"], "bob", "{page}");
    assert_eq!(author["deactivated"], true, "{page}");
    let (_ws, ready) = identify(&env.ws_url(), &alice).await;
    let names: Vec<&str> = ready["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alice", "carol"]);
    // A second Leave is from someone who is no longer a Member.
    assert_envelope(&leave(&env, &bob).await, 401, "AUTH_FAILED");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_cannot_leave_until_ownership_is_handed_on() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let alice_signed_in = sign_in(&env, "alice").await;
    let alice = access(&alice_signed_in);
    let bob_signed_in = sign_in(&env, "bob").await;
    let (mut alice_ws, _) = identify(&env.ws_url(), &alice).await;

    let refused = leave(&env, &alice).await;
    assert_detail(&refused, 409, "CONFLICT", "CANNOT_REMOVE_OWNER");
    assert_eq!(
        refused.json()["error"]["message"],
        "The Lobby's owner cannot leave. Hand ownership to another Member first."
    );
    // Nothing changed: still a Member, still connected, still able to speak.
    send_and_confirm(&mut alice_ws, "still here").await;

    assert_eq!(
        transfer(&env, &alice, &user_id(&bob_signed_in))
            .await
            .status,
        204
    );
    assert_eq!(leave(&env, &alice).await.status, 204);
    assert_eq!(expect_close(&mut alice_ws).await, 4006);
    let bob = access(&bob_signed_in);
    let (_ws, ready) = identify(&env.ws_url(), &bob).await;
    assert_eq!(ready["lobby"]["owner"], user_id(&bob_signed_in));
    assert_eq!(ready["members"].as_array().unwrap().len(), 1);

    assert_envelope(
        &http(env.addr, "DELETE", "/api/v1/users/me", &[], "").await,
        401,
        "AUTH_FAILED",
    );
    assert!(env.shutdown().await.is_clean());
}

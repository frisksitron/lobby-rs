//! Refresh rotation and the bearer check, over real loopback sockets.

use std::time::Duration;

use crate::common::*;
use lobby_server::testing::TestServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_rotates_reissues_after_a_lost_response_and_revokes_on_reuse() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let signed_in = sign_in(&env, "alice").await;
    let r0 = refresh_token(&signed_in["tokens"]);
    let a0 = access(&signed_in);

    let rotated = refresh(env.addr, &r0).await;
    assert_eq!(rotated.status, 200, "{}", rotated.body);
    let t1 = rotated.json();
    assert_ne!(refresh_token(&t1), r0);
    assert_envelope(
        &get_auth(env.addr, "/api/v1/messages", &a0).await,
        401,
        "AUTH_FAILED",
    );

    // The response "was lost": the client presents r0 again while the new pair is unused.
    let reissued = refresh(env.addr, &r0).await;
    assert_eq!(reissued.status, 200, "{}", reissued.body);
    let t2 = reissued.json();
    assert_ne!(refresh_token(&t2), refresh_token(&t1));
    let a2 = t2["accessToken"].as_str().unwrap().to_owned();
    // Using the reissued pair ends the grace...
    let (mut ws, _) = identify(&env.ws_url(), &a2).await;
    // ...so r0 now is reuse: the session is deleted and its socket closed with 4005.
    let reused = refresh(env.addr, &r0).await;
    assert_envelope(&reused, 401, "AUTH_FAILED");
    assert_eq!(reused.json()["error"]["message"], "Invalid refresh token");
    assert_eq!(expect_close(&mut ws).await, 4005);
    assert_envelope(
        &refresh(env.addr, &refresh_token(&t2)).await,
        401,
        "AUTH_FAILED",
    );
    assert_envelope(
        &get_auth(env.addr, "/api/v1/messages", &a2).await,
        401,
        "AUTH_FAILED",
    );
    assert_envelope(&refresh(env.addr, "nonsense").await, 401, "AUTH_FAILED");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bearer_routes_need_a_valid_unexpired_token() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        c.auth.access_ttl = Duration::from_millis(500);
    })
    .await;
    assert_envelope(&get(env.addr, "/api/v1/messages").await, 401, "AUTH_FAILED");
    assert_envelope(
        &get_auth(env.addr, "/api/v1/messages", "made-up").await,
        401,
        "AUTH_FAILED",
    );
    let token = access(&sign_in(&env, "alice").await);
    assert_eq!(
        get_auth(env.addr, "/api/v1/messages", &token).await.status,
        200
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    let expired = get_auth(env.addr, "/api/v1/messages", &token).await;
    assert_envelope(&expired, 401, "AUTH_EXPIRED");
    assert!(env.shutdown().await.is_clean());
}

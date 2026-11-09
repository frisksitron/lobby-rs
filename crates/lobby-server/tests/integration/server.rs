//! In-process server tests over real loopback sockets. Every test ends with a clean
//! `ShutdownReport`.

use std::time::Duration;

use crate::common::*;
use lobby_server::testing::TestServer;
use serde_json::json;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest as _, http::HeaderValue};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_and_hello() {
    let env = TestServer::start().await;
    let addr = env.addr;

    let health = get(addr, "/health").await;
    assert_eq!(health.status, 200);
    assert_eq!(health.json(), json!({"status": "ok"}));

    let info = get(addr, "/api/v1/lobby").await;
    assert_eq!(info.status, 200, "{}", info.body);
    let mut info = info.json();
    // Generated once per database: a valid id that a restart keeps.
    let instance_id = info["instanceId"].take();
    assert!(
        instance_id
            .as_str()
            .is_some_and(|id| id.parse::<lobby_protocol::LobbyInstanceId>().is_ok()),
        "{instance_id}"
    );
    assert_eq!(
        info,
        json!({
            "instanceId": null,
            "name": "Test Lobby",
            "uploadMaxBytes": 10_485_760,
            "magicCodeSends": 1000,
            "joinRequestsEnabled": true,
            "setUp": false,
            "protocolVersion": 1,
            "serverVersion": env!("CARGO_PKG_VERSION"),
        })
    );

    let mut ws = ws_connect(&env.ws_url()).await;
    let hello = next_json(&mut ws).await;
    assert_eq!(
        hello,
        json!({"t": "HELLO", "d": {
            "protocolVersion": 1,
            "serverVersion": env!("CARGO_PKG_VERSION"),
            "heartbeatMs": 10_000,
            "identifyTimeoutMs": 10_000,
        }})
    );

    let report = env.shutdown().await;
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(expect_close(&mut ws).await, 1001);
    // Windows retries a refused connect for about 2 s; loopback accepts at once when it does.
    let connect = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(addr),
    );
    assert!(!matches!(connect.await, Ok(Ok(_))), "the port is closed");
}

#[tokio::test]
async fn requests_with_an_origin_header_get_403() {
    let env = TestServer::start().await;
    let res = http(
        env.addr,
        "GET",
        "/api/v1/lobby",
        &[("Origin", "https://evil.example")],
        "",
    )
    .await;
    assert_envelope(&res, 403, "FORBIDDEN");
    assert_eq!(
        res.json()["error"]["message"],
        "Browser origins are not allowed"
    );

    let mut req = env.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("Origin", HeaderValue::from_static("https://evil.example"));
    match tokio_tungstenite::connect_async(req).await {
        Err(tungstenite::Error::Http(res)) => assert_eq!(res.status(), 403),
        other => panic!("upgrade with Origin must be refused, got {other:?}"),
    }
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test]
async fn the_address_opens_a_landing_page_and_other_paths_stay_json_errors() {
    let env = TestServer::start_with(|cfg| cfg.lobby.name = "<script>&".into()).await;
    let addr = env.addr;

    let page = get(addr, "/").await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(page.header("cache-control"), Some("no-cache"));
    let policy = page.header("content-security-policy").unwrap();
    assert!(policy.starts_with("default-src 'none';"), "{policy}");
    assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
    assert!(page.body.contains("&lt;script&gt;&amp;"), "{}", page.body);
    assert!(!page.body.contains("<script>&"), "{}", page.body);

    assert_envelope(&get(addr, "/no/such/route").await, 404, "NOT_FOUND");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test]
async fn every_error_has_the_json_envelope() {
    let env = TestServer::start().await;
    let addr = env.addr;
    assert_envelope(&get(addr, "/no/such/route").await, 404, "NOT_FOUND");
    assert_envelope(
        &http(addr, "POST", "/api/v1/lobby", &[], "{}").await,
        405,
        "INVALID_REQUEST",
    );
    assert_envelope(&get(addr, "/api/v1/ws").await, 400, "INVALID_REQUEST");
    assert_envelope(
        &http(
            addr,
            "GET",
            "/api/v1/lobby",
            &[("Content-Length", "66560")],
            "",
        )
        .await,
        413,
        "PAYLOAD_TOO_LARGE",
    );
    assert!(env.shutdown().await.is_clean());
}

//! Uploads, `/media`, and attachments in Messages, over real loopback sockets.

use std::io::Cursor;

use crate::common::*;
use image::{DynamicImage, ImageFormat, RgbImage};
use lobby_protocol::BlobId;
use lobby_server::testing::TestServer;
use serde_json::{Value, json};

const MAX_UPLOAD: usize = 1 << 20;

fn png(width: u32, height: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
    }));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn encode_component(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn upload_raw(env: &TestServer, token: &str, name: Option<&str>, body: &[u8]) -> RawResponse {
    let path = match name {
        Some(n) => format!("/api/v1/uploads?name={}", encode_component(n)),
        None => "/api/v1/uploads".to_owned(),
    };
    let bearer = format!("Bearer {token}");
    http_bytes(
        env.addr,
        "POST",
        &path,
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/octet-stream"),
        ],
        body,
    )
    .await
}

async fn upload(env: &TestServer, token: &str, name: &str, body: &[u8]) -> Value {
    let res = upload_raw(env, token, Some(name), body).await;
    assert_eq!(res.status, 201, "{}", String::from_utf8_lossy(&res.body));
    res.json()
}

async fn media(
    env: &TestServer,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> RawResponse {
    http_bytes(env.addr, method, path, headers, b"").await
}

fn send_frame(rid: &str, content: &str, ids: &[String]) -> String {
    json!({"t": "MESSAGE_SEND", "rid": rid, "d": {"content": content, "attachmentIds": ids}})
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_is_201_with_its_attachment_and_every_refusal_is_a_400() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let token = access(&sign_in(&env, "alice").await);

    let image = upload(&env, &token, "cat.png", &png(960, 480)).await;
    assert_eq!(image["name"], "cat.png");
    assert_eq!(image["mimeType"], "image/png");
    assert_eq!(image["preview"], json!({"width": 480, "height": 240}));
    assert!(image["size"].as_u64().unwrap() > 100);
    assert!(image["id"].as_str().unwrap().starts_with("blb_"));

    let text = upload(&env, &token, "notes.txt", b"just some notes").await;
    assert_eq!(text["mimeType"], "text/plain");
    assert_eq!(text["size"], 15);
    assert!(
        text.get("preview").is_none(),
        "an absent preview is absent, never null: {text}"
    );

    // The name is the server's to sanitize, and a missing one becomes upload.bin.
    let bat = upload(
        &env,
        &token,
        r"..\..\AppData\Roaming\Startup\x.bat",
        b"echo hi",
    )
    .await;
    assert_eq!(bat["name"], "x.bat");
    let unnamed = upload_raw(&env, &token, None, b"bytes").await;
    assert_eq!(unnamed.json()["name"], "upload.bin");

    // The extension and the client's Content-Type decide nothing: the bytes do.
    let renamed_exe = upload_raw(&env, &token, Some("holiday.png"), b"MZ\x90\x00\x03rest").await;
    assert_envelope(&to_http(&renamed_exe), 400, "INVALID_REQUEST");
    assert_eq!(
        renamed_exe.json()["error"]["message"],
        "Executable files are not allowed"
    );
    let script = upload_raw(&env, &token, Some("run"), b"#!/bin/sh\necho hi\n").await;
    assert_eq!(
        script.json()["error"]["message"],
        "Executable files are not allowed"
    );
    let svg = upload_raw(&env, &token, Some("a.svg"), b"<svg xmlns='x'/>").await;
    assert_envelope(&to_http(&svg), 400, "INVALID_REQUEST");
    assert_eq!(svg.json()["error"]["message"], "Unsupported file type");
    let empty = upload_raw(&env, &token, Some("empty.txt"), b"").await;
    assert_envelope(&to_http(&empty), 400, "INVALID_REQUEST");
    assert_eq!(empty.json()["error"]["message"], "Request body is required");

    let anonymous = http_bytes(env.addr, "POST", "/api/v1/uploads", &[], b"x").await;
    assert_envelope(&to_http(&anonymous), 401, "AUTH_FAILED");

    // An image that does not decode is still stored, as a file card without a preview.
    let broken = upload(&env, &token, "broken.png", b"\x89PNG\r\n\x1a\nnot really").await;
    assert_eq!(broken["mimeType"], "image/png");
    assert!(broken.get("preview").is_none());

    // Only the accepted uploads left files behind: the image and its preview, then four more.
    assert_eq!(blob_files(&env).await.len(), 6);
    assert!(env.shutdown().await.is_clean());
}

fn to_http(raw: &RawResponse) -> HttpResponse {
    HttpResponse {
        status: raw.status,
        headers: raw.headers.clone(),
        body: String::from_utf8_lossy(&raw.body).into_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_upload_is_a_413_by_content_length_and_when_streamed() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        c.storage.upload_max_bytes = MAX_UPLOAD as u64;
    })
    .await;
    let token = access(&sign_in(&env, "alice").await);
    let bearer = format!("Bearer {token}");
    let auth = ("Authorization", bearer.as_str());

    let declared = (MAX_UPLOAD * 4).to_string();
    let early = http_bytes(
        env.addr,
        "POST",
        "/api/v1/uploads?name=big.bin",
        &[auth, ("Content-Length", declared.as_str())],
        b"",
    )
    .await;
    assert_envelope(&to_http(&early), 413, "PAYLOAD_TOO_LARGE");
    // One byte over, declared exactly: ours, with the upload copy.
    let one_over = vec![b'a'; MAX_UPLOAD + 1];
    let res = upload_raw(&env, &token, Some("big.txt"), &one_over).await;
    assert_envelope(&to_http(&res), 413, "PAYLOAD_TOO_LARGE");
    assert_eq!(
        res.json()["error"]["message"],
        "File exceeds the upload size limit"
    );

    // Streamed: no Content-Length, chunked, and bigger than the layer's backstop.
    let mut chunked = Vec::new();
    for _ in 0..8 {
        chunked.extend_from_slice(format!("{:x}\r\n", MAX_UPLOAD / 2).as_bytes());
        chunked.extend_from_slice(&vec![b'a'; MAX_UPLOAD / 2]);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    let streamed = http_bytes(
        env.addr,
        "POST",
        "/api/v1/uploads?name=stream.txt",
        &[auth, ("Transfer-Encoding", "chunked")],
        &chunked,
    )
    .await;
    assert_envelope(&to_http(&streamed), 413, "PAYLOAD_TOO_LARGE");
    let mut just_over = Vec::new();
    just_over.extend_from_slice(format!("{:x}\r\n", MAX_UPLOAD + 1).as_bytes());
    just_over.extend_from_slice(&one_over);
    just_over.extend_from_slice(b"\r\n0\r\n\r\n");
    let res = http_bytes(
        env.addr,
        "POST",
        "/api/v1/uploads?name=stream.txt",
        &[auth, ("Transfer-Encoding", "chunked")],
        &just_over,
    )
    .await;
    assert_eq!(res.status, 413, "{}", String::from_utf8_lossy(&res.body));
    assert!(
        blob_files(&env).await.is_empty(),
        "a refused upload leaves no file"
    );

    let exact = vec![b'a'; MAX_UPLOAD];
    let ok = upload(&env, &token, "exact.txt", &exact).await;
    assert_eq!(ok["size"], MAX_UPLOAD);
    assert_eq!(blob_files(&env).await.len(), 1);
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_serves_ranges_validators_and_dispositions() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let token = access(&sign_in(&env, "alice").await);
    let bytes = png(960, 480);
    let image = upload(&env, &token, "naïve cat.png", &bytes).await;
    let id = image["id"].as_str().unwrap().to_owned();
    let full = format!("/media/{id}");
    let etag = format!("\"{id}\"");

    let res = media(&env, "GET", &full, &[]).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.body, bytes);
    assert_eq!(res.header("content-type"), Some("image/png"));
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(
        res.header("etag"),
        Some(etag.as_str()),
        "ours, not tower-http's"
    );
    assert_eq!(res.header("last-modified"), None);
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        res.header("content-security-policy"),
        Some("default-src 'none'; sandbox")
    );
    assert_eq!(
        res.header("content-disposition"),
        Some("inline; filename=\"na_ve cat.png\"; filename*=UTF-8''na%C3%AFve%20cat.png")
    );
    assert_eq!(res.header("accept-ranges"), Some("bytes"));

    let res = media(&env, "GET", &format!("{full}?download=1"), &[]).await;
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("attachment; ")
    );

    let total = bytes.len();
    let res = media(&env, "GET", &full, &[("Range", "bytes=0-3")]).await;
    assert_eq!(res.status, 206);
    assert_eq!(res.body, bytes[..4]);
    let content_range = format!("bytes 0-3/{total}");
    assert_eq!(res.header("content-range"), Some(content_range.as_str()));
    assert_eq!(res.header("etag"), Some(etag.as_str()));
    let res = media(&env, "GET", &full, &[("Range", "bytes=-5")]).await;
    assert_eq!((res.status, res.body), (206, bytes[total - 5..].to_vec()));
    let from = format!("bytes={}-", total - 2);
    let res = media(&env, "GET", &full, &[("Range", from.as_str())]).await;
    assert_eq!((res.status, res.body), (206, bytes[total - 2..].to_vec()));
    let past = format!("bytes={total}-");
    let res = media(&env, "GET", &full, &[("Range", past.as_str())]).await;
    assert_eq!(res.status, 416, "{}", String::from_utf8_lossy(&res.body));
    // If-Range with our tag keeps the range; with another validator it is the whole file.
    let res = media(
        &env,
        "GET",
        &full,
        &[("Range", "bytes=0-3"), ("If-Range", etag.as_str())],
    )
    .await;
    assert_eq!(res.status, 206);
    let res = media(
        &env,
        "GET",
        &full,
        &[("Range", "bytes=0-3"), ("If-Range", "\"other\"")],
    )
    .await;
    assert_eq!((res.status, res.body.len()), (200, total));

    // Validators: our ETag answers 304 (strong, weak, list, star), anything else is a 200,
    // and a capability URL never 412s.
    for value in [
        etag.clone(),
        format!("W/{etag}"),
        format!("\"nope\", {etag}"),
        "*".to_owned(),
    ] {
        let res = media(&env, "GET", &full, &[("If-None-Match", value.as_str())]).await;
        assert_eq!(res.status, 304, "{value}");
        assert!(res.body.is_empty());
        assert_eq!(res.header("etag"), Some(etag.as_str()));
        assert_eq!(
            res.header("cache-control"),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    }
    let res = media(&env, "GET", &full, &[("If-None-Match", "\"nope\"")]).await;
    assert_eq!(res.status, 200);
    for (name, value) in [
        ("If-Match", "\"nope\""),
        ("If-Unmodified-Since", "Sat, 01 Jan 2000 00:00:00 GMT"),
        ("If-Modified-Since", "Sat, 01 Jan 2099 00:00:00 GMT"),
    ] {
        let res = media(&env, "GET", &full, &[(name, value)]).await;
        assert_eq!(res.status, 200, "{name} must not decide anything");
        assert_eq!(res.header("etag"), Some(etag.as_str()), "{name}");
        assert_eq!(res.header("last-modified"), None, "{name}");
    }

    let res = media(&env, "HEAD", &full, &[]).await;
    assert_eq!(res.status, 200);
    assert!(res.body.is_empty());
    let length = total.to_string();
    assert_eq!(res.header("content-length"), Some(length.as_str()));
    assert_eq!(res.header("etag"), Some(etag.as_str()));

    let preview = media(&env, "GET", &format!("{full}/preview"), &[]).await;
    assert_eq!(preview.status, 200);
    assert_eq!(preview.header("content-type"), Some("image/jpeg"));
    assert_eq!(&preview.body[..3], &[0xFF, 0xD8, 0xFF]);
    let preview_tag = format!("\"{id}-preview\"");
    assert_eq!(preview.header("etag"), Some(preview_tag.as_str()));
    assert_eq!(
        preview.header("content-disposition"),
        Some("inline; filename=\"na_ve cat.jpg\"; filename*=UTF-8''na%C3%AFve%20cat.jpg")
    );
    let decoded = image::load_from_memory(&preview.body).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (480, 240));
    let res = media(
        &env,
        "GET",
        &format!("{full}/preview"),
        &[("If-None-Match", preview_tag.as_str())],
    )
    .await;
    assert_eq!(res.status, 304);
    let res = media(
        &env,
        "GET",
        &format!("{full}/preview"),
        &[("If-None-Match", etag.as_str())],
    )
    .await;
    assert_eq!(res.status, 200, "the full image's tag is not the preview's");

    let zip = upload(&env, &token, "a.zip", b"PK\x03\x04rest of a zip").await;
    let res = media(
        &env,
        "GET",
        &format!("/media/{}", zip["id"].as_str().unwrap()),
        &[],
    )
    .await;
    assert_eq!(res.header("content-type"), Some("application/zip"));
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("attachment; filename=\"a.zip\"")
    );
    let text = upload(&env, &token, "t.txt", b"hello").await;
    let missing = media(
        &env,
        "GET",
        &format!("/media/{}/preview", text["id"].as_str().unwrap()),
        &[],
    )
    .await;
    assert_envelope(&to_http(&missing), 404, "NOT_FOUND");
    assert_eq!(
        missing.json()["error"]["message"],
        "Media preview not found"
    );

    let unknown = media(&env, "GET", &format!("/media/{}", BlobId::random()), &[]).await;
    assert_envelope(&to_http(&unknown), 404, "NOT_FOUND");
    assert_eq!(unknown.json()["error"]["message"], "Media not found");
    let malformed = media(&env, "GET", "/media/not-a-blob", &[]).await;
    assert_envelope(&to_http(&malformed), 400, "INVALID_REQUEST");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stored_executable_name_and_a_hostile_one_are_served_safely() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let token = access(&sign_in(&env, "alice").await);
    let bat = upload(&env, &token, r"..\..\x.bat", b"echo hi").await;
    assert_eq!(bat["name"], "x.bat");
    let res = media(
        &env,
        "GET",
        &format!("/media/{}", bat["id"].as_str().unwrap()),
        &[],
    )
    .await;
    assert_eq!(res.header("content-type"), Some("text/plain"));
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("attachment; filename=\"x.bat\"")
    );
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blob_that_is_not_yours_expired_or_claimed_is_attachment_invalid() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let alice = sign_in(&env, "alice").await;
    let bob = sign_in(&env, "bob").await;
    let (alice_token, bob_token) = (access(&alice), access(&bob));
    let (mut ws, _) = identify(&env.ws_url(), &alice_token).await;

    let bobs = upload(&env, &bob_token, "bobs.txt", b"bob's").await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stale = upload(&env, &alice_token, "stale.txt", b"stale").await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mine = upload(&env, &alice_token, "mine.txt", b"mine").await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query("UPDATE blobs SET expires_at = 1 WHERE id = ?1")
        .bind(&stale)
        .execute(&mut sqlite(&env).await)
        .await
        .unwrap();

    let refused = |label: &'static str, ids: Vec<String>| {
        let rid = nonce();
        (label, rid.clone(), send_frame(&rid, "x", &ids))
    };
    for (label, rid, frame) in [
        refused("another Member's", vec![bobs.clone()]),
        refused("expired", vec![stale.clone()]),
        refused("one bad among good", vec![mine.clone(), bobs.clone()]),
    ] {
        send_text(&mut ws, &frame).await;
        let error = next_of(&mut ws, "ERROR").await;
        assert_eq!(error["code"], "ATTACHMENT_INVALID", "{label}");
        assert_eq!(error["rid"], rid.as_str(), "{label}");
        assert_eq!(
            error["message"],
            "One or more attachments are no longer available"
        );
        tokio::time::sleep(std::time::Duration::from_millis(210)).await;
    }

    let rid = nonce();
    send_text(
        &mut ws,
        &send_frame(&rid, "mine", std::slice::from_ref(&mine)),
    )
    .await;
    let created = next_of(&mut ws, "MESSAGE_CREATE").await;
    assert_eq!(created["message"]["id"], 1, "refused sends stored nothing");
    tokio::time::sleep(std::time::Duration::from_millis(210)).await;
    send_text(
        &mut ws,
        &send_frame(&nonce(), "again", std::slice::from_ref(&mine)),
    )
    .await;
    let error = next_of(&mut ws, "ERROR").await;
    assert_eq!(error["code"], "ATTACHMENT_INVALID", "already claimed");

    // A resend of the stored Message (same rid) replays it though its blob is claimed now.
    tokio::time::sleep(std::time::Duration::from_millis(210)).await;
    send_text(
        &mut ws,
        &send_frame(&rid, "mine", std::slice::from_ref(&mine)),
    )
    .await;
    let replay = next_of(&mut ws, "MESSAGE_CREATE").await;
    assert_eq!(replay["message"]["id"], 1);
    assert_eq!(replay["message"]["attachments"][0]["id"], mine.as_str());
    assert!(env.shutdown().await.is_clean());
}

/// Only its author edits or deletes a Message. Everyone sees the edit and the removal before
/// the author's ACK, history follows, and a deleted Message's files go with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_author_edits_and_deletes_a_message_and_nobody_else_can() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let alice = access(&sign_in(&env, "alice").await);
    let bob = access(&sign_in(&env, "bob").await);
    let (mut a, _) = identify(&env.ws_url(), &alice).await;
    let (mut b, _) = identify(&env.ws_url(), &bob).await;
    let cat = upload(&env, &alice, "cat.png", &png(640, 480)).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let attach = std::slice::from_ref(&cat);
    send_text(&mut a, &send_frame(&nonce(), "teh cat", attach)).await;
    let id = next_of(&mut a, "MESSAGE_CREATE").await["message"]["id"].clone();
    next_of(&mut b, "MESSAGE_CREATE").await;
    assert_eq!(
        blob_files(&env).await.len(),
        2,
        "the picture and its preview"
    );

    let edit = |rid: &str, content: &str| {
        json!({"t": "MESSAGE_EDIT", "rid": rid, "d": {"messageId": id, "content": content}})
            .to_string()
    };
    let delete =
        |rid: &str| json!({"t": "MESSAGE_DELETE", "rid": rid, "d": {"messageId": id}}).to_string();
    send_text(&mut b, &edit("b1", "bob was here")).await;
    let refused = next_of(&mut b, "ERROR").await;
    assert_eq!(
        (refused["code"].clone(), refused["rid"].clone()),
        (json!("FORBIDDEN"), json!("b1"))
    );
    send_text(&mut b, &delete("b2")).await;
    let refused = next_of(&mut b, "ERROR").await;
    assert_eq!(
        (refused["code"].clone(), refused["rid"].clone()),
        (json!("FORBIDDEN"), json!("b2"))
    );

    send_text(&mut a, &edit("a1", "the cat")).await;
    let mine = next_json(&mut a).await;
    assert_eq!(mine["t"], "MESSAGE_UPDATE", "the broadcast before the ACK");
    assert_eq!(next_of(&mut a, "ACK").await["rid"], "a1");
    let seen = next_of(&mut b, "MESSAGE_UPDATE").await["message"].clone();
    assert_eq!(seen["content"], "the cat");
    assert!(
        seen["editedAt"]
            .as_i64()
            .is_some_and(|t| t >= seen["createdAt"].as_i64().unwrap())
    );
    assert_eq!(
        seen["attachments"][0]["id"],
        cat.as_str(),
        "an edit keeps the attachments"
    );
    let page = get_auth(env.addr, "/api/v1/messages", &bob).await.json();
    assert_eq!(page["messages"][0]["content"], "the cat");
    assert_eq!(page["messages"][0]["editedAt"], seen["editedAt"]);
    assert_eq!(seen["rev"], 1);
    let changes = |after: u64| {
        let path = format!("/api/v1/messages/changes?after={after}");
        let bob = bob.clone();
        async move { get_auth(env.addr, &path, &bob).await.json() }
    };
    assert_eq!(changes(0).await, json!({"updated": [seen], "removed": []}));

    send_text(&mut a, &delete("a2")).await;
    assert_eq!(next_json(&mut a).await["t"], "MESSAGE_REMOVED");
    assert_eq!(next_of(&mut a, "ACK").await["rid"], "a2");
    assert_eq!(
        next_of(&mut b, "MESSAGE_REMOVED").await,
        json!({"messageId": id, "rev": 2})
    );
    let page = get_auth(env.addr, "/api/v1/messages", &bob).await.json();
    assert_eq!(page["messages"], json!([]));
    let back = json!({"updated": [], "removed": [id]});
    assert_eq!(
        changes(0).await,
        back,
        "a Member away since before the edit"
    );
    assert_eq!(changes(1).await, back, "a Member who heard the edit");
    assert_eq!(changes(2).await, json!({"updated": [], "removed": []}));
    let (_, ready) = identify(&env.ws_url(), &bob).await;
    assert_eq!(ready["lastChangeRev"], 2);
    assert_eq!(
        ready["historyRev"], 2,
        "read with the page, which shows the delete"
    );
    assert_eq!(
        media(&env, "GET", &format!("/media/{cat}"), &[])
            .await
            .status,
        404
    );
    assert!(blob_files(&env).await.is_empty(), "its files went with it");
    send_text(&mut a, &delete("a3")).await;
    assert_eq!(next_of(&mut a, "ERROR").await["code"], "NOT_FOUND");
    assert!(env.shutdown().await.is_clean());
}

/// A deleted Message stays deleted: the same send again (its answer was lost, so the client
/// retries its nonce) is answered with an ACK and stores nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_retried_after_its_message_was_deleted_does_not_bring_it_back() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let alice = access(&sign_in(&env, "alice").await);
    let bob = access(&sign_in(&env, "bob").await);
    let (mut a, _) = identify(&env.ws_url(), &alice).await;
    let (mut b, _) = identify(&env.ws_url(), &bob).await;
    let rid = nonce();
    send_text(&mut a, &message_send(&rid, "the wifi password is hunter2")).await;
    let id = next_of(&mut a, "MESSAGE_CREATE").await["message"]["id"].clone();
    next_of(&mut b, "MESSAGE_CREATE").await;
    let delete = json!({"t": "MESSAGE_DELETE", "rid": "d1", "d": {"messageId": id}});
    send_text(&mut a, &delete.to_string()).await;
    assert_eq!(next_of(&mut a, "ACK").await["rid"], "d1");
    next_of(&mut b, "MESSAGE_REMOVED").await;

    send_text(&mut a, &message_send(&rid, "the wifi password is hunter2")).await;
    let answer = next_json(&mut a).await;
    assert_eq!(
        (answer["t"].as_str(), answer["d"]["rid"].as_str()),
        (Some("ACK"), Some(rid.as_str())),
        "{answer}"
    );
    send_and_confirm(&mut a, "never mind").await;
    assert_eq!(
        next_of(&mut b, "MESSAGE_CREATE").await["message"]["content"],
        "never mind",
        "bob never hears the deleted one again"
    );
    let page = get_auth(env.addr, "/api/v1/messages", &bob).await.json();
    assert_eq!(page["messages"].as_array().map(Vec::len), Some(1));
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_is_limited_generously_and_uploads_are_limited_to_ten_a_minute() {
    let env = TestServer::start_with_members(&["alice"], |c| c.http.rate_limits = true).await;
    let token = access(&sign_in(&env, "alice").await);
    let first = upload(&env, &token, "a.txt", b"x").await;
    let id = first["id"].as_str().unwrap().to_owned();
    for i in 0..100 {
        let res = media(&env, "GET", &format!("/media/{id}"), &[]).await;
        assert_eq!(res.status, 200, "request {i}");
    }
    // Both variants have their own bucket, and a 304 counts like any other answer.
    for _ in 0..50 {
        let res = media(
            &env,
            "GET",
            &format!("/media/{id}"),
            &[("If-None-Match", &format!("\"{id}\""))],
        )
        .await;
        assert_eq!(res.status, 304);
    }
    for _ in 0..9 {
        upload(&env, &token, "b.txt", b"y").await;
    }
    let eleventh = upload_raw(&env, &token, Some("c.txt"), b"z").await;
    assert_envelope(&to_http(&eleventh), 429, "RATE_LIMITED");
    assert!(eleventh.header("retry-after").is_some());
    assert!(env.shutdown().await.is_clean());
}

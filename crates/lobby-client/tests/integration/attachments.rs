//! End to end: a real in-process `lobby-server` with `/uploads` and `/media`, two
//! Members whose Sessions run on the same driver, `ApiClient` and `MediaCache` as the desktop's.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use lobby_client::{
    DraftSource, Intent, IssueKind, MediaCacheError, MemoryTokenStore, Paths, Phase, UiEffect,
    Variant,
    testing::{ChaosProxy, test_client},
};
use lobby_protocol::{Attachment, Message, UnixMs};
use lobby_server::testing::TestServer;

use crate::common::{Member, origin};

const MIB: u64 = 1024 * 1024;

fn png_file(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
    let path = dir.join(name);
    let img = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255])
    });
    image::DynamicImage::ImageRgba8(img)
        .save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    path
}

fn bytes_file(dir: &Path, name: &str, size: u64) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, vec![0x5au8; size as usize]).unwrap();
    path
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|e| {
            let path = e.path();
            if path.is_dir() {
                files_under(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_is_sent_and_the_other_member_sees_the_preview() {
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let origin = origin(&server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));
    let mut a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    let mut b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let photo = png_file(dir.path(), "cat.png", 640, 480);
    let big = bytes_file(dir.path(), "notes.bin", 3 * MIB);

    a.attach(DraftSource::from_path(&photo).unwrap())
        .await
        .unwrap();
    a.attach(DraftSource::from_path(&big).unwrap())
        .await
        .unwrap();
    a.wait_until("both uploads Ready", |s| {
        s.drafts.items().iter().all(|d| d.state.is_ready())
    })
    .await;
    let ready: Vec<&Attachment> = a
        .state()
        .drafts
        .items()
        .iter()
        .filter_map(|d| d.attachment())
        .collect();
    assert_eq!(ready[0].name, "cat.png");
    assert_eq!(ready[0].mime_type, "image/png");
    let preview = ready[0].preview.expect("an image gets a preview");
    assert_eq!((preview.width, preview.height), (480, 360));
    assert_eq!(ready[1].name, "notes.bin");
    assert_eq!(ready[1].size, 3 * MIB);
    assert_eq!(ready[1].preview, None);

    let sent = a.send("look at this").await;
    assert_eq!(sent.attachments.len(), 2);
    assert!(a.state().drafts.is_empty(), "the strip emptied");
    b.wait_until("bob sees the attachments", |s| {
        s.chat
            .values()
            .any(|m| m.attachments.len() == 2 && m.attachments.iter().any(|x| x.preview.is_some()))
    })
    .await;
    let seen: Message = b
        .state()
        .chat
        .values()
        .find(|m| m.nonce == sent.nonce)
        .cloned()
        .unwrap();
    assert_eq!(
        seen.attachments, sent.attachments,
        "the same, in server order"
    );

    let cache_dir = tempfile::tempdir().unwrap();
    let media = client_b.media(
        &origin,
        &Paths {
            data_dir: cache_dir.path().to_owned(),
        },
    );
    // Attachments are read in upload order, and two uploads run concurrently.
    let by_name = |name: &str| seen.attachments.iter().find(|x| x.name == name).unwrap();
    let notes = by_name("notes.bin");
    assert_eq!(
        media
            .file(&notes.id, Variant::Full { size: MIB })
            .await
            .unwrap(),
        Err(MediaCacheError::TooLarge),
        "a body past the size the caller expects is refused"
    );
    assert_eq!(files_under(cache_dir.path()), Vec::<PathBuf>::new());
    let cat = by_name("cat.png");
    let blob = &cat.id;
    let small = media.image(blob, Variant::Preview).await.unwrap().unwrap();
    assert_eq!((small.width, small.height), (480, 360));
    let full = media
        .image(blob, Variant::Full { size: cat.size })
        .await
        .unwrap()
        .unwrap();
    assert_eq!((full.width, full.height), (640, 480));
    let saved = media
        .save_as(cat, cache_dir.path().join("saved").join("cat.png"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(saved).unwrap(),
        std::fs::read(&photo).unwrap(),
        "the saved bytes are the uploaded bytes"
    );
    assert_eq!(
        media.open_copy(by_name("notes.bin")).await.unwrap(),
        Err(MediaCacheError::NotAllowed),
        "a .bin is Save… only"
    );
    assert!(
        media.fetch_counts().values().all(|n| *n == 1),
        "{:?}",
        media.fetch_counts()
    );
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

/// alice removes a photo from the strip while the Lobby, busy with other writes, has its body
/// but has not stored it yet. The Lobby took it, so it keeps it as an unclaimed upload, which
/// expires like any other: no file is left behind without its row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attachment_removed_while_the_lobby_stores_it_leaves_no_file_behind() {
    let server = TestServer::start_with_members(&["alice"], |_| {}).await;
    let store = MemoryTokenStore::new();
    let mut a = Member::sign_in(
        &test_client(&store),
        &origin(&server),
        "alice",
        &server.mail,
    )
    .await
    .unwrap();
    a.wait_connected().await;
    let dir = tempfile::tempdir().unwrap();
    let photo = png_file(dir.path(), "cat.png", 640, 480);

    let held = server.server.hold_writes().await;
    a.attach(DraftSource::from_path(&photo).unwrap())
        .await
        .unwrap();
    let draft = a.state().drafts.items().last().expect("in the strip").id;
    server.server.writes_in_flight(1).await;
    a.act(Intent::RemoveAttachment(draft)).await.unwrap();
    assert!(a.state().drafts.is_empty(), "gone from the strip at once");
    drop(held);
    server.server.writes_settled().await;

    let later = UnixMs(UnixMs::now().0 + 2 * 3_600_000);
    let swept = server.run_cleanup_at(later).await;
    assert_eq!(swept.orphans, 0, "{swept:?}");
    a.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

/// A send whose connection dies before the answer: the Ready attachments come back to the
/// strip, and the re-send after the reconnect goes through with them (the same nonce, so the
/// Lobby replays rather than stores twice if it had taken the first).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_send_restores_its_attachments_and_the_resend_carries_them() {
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
    let dir = tempfile::tempdir().unwrap();
    let photo = png_file(dir.path(), "cat.png", 64, 48);
    a.attach(DraftSource::from_path(&photo).unwrap())
        .await
        .unwrap();
    a.wait_uploaded().await;
    let blob = a.state().drafts.items()[0].attachment().unwrap().id.clone();

    // The frame is written into a frozen path, so no answer ever comes; the cut ends the
    // connection and fails the send at once.
    proxy.freeze_websockets();
    a.send_raw("with a cat").await.expect("accepted");
    assert!(a.state().drafts.is_empty(), "taken by the send");
    proxy.cut_all();
    a.wait_until("the send failed and the attachment is back", |s| {
        s.pending.is_empty() && s.drafts.items().len() == 1
    })
    .await;
    let fx = a.take_effects();
    assert!(
        fx.iter().any(|e| matches!(
            e,
            UiEffect::SendFailed { draft, issue }
                if draft == "with a cat" && issue.kind == IssueKind::SendFailed
        )),
        "{fx:?}"
    );
    assert_eq!(
        a.state().drafts.items()[0].attachment().map(|x| &x.id),
        Some(&blob),
        "Ready as it was"
    );
    a.wait_phase_within(Duration::from_secs(15), "reconnected", |p| {
        *p == Phase::Connected
    })
    .await;
    a.wait_connected().await;
    let sent = a.send("with a cat").await;
    assert_eq!(sent.attachments[0].id, blob);
    b.wait_until("bob sees it", move |s| {
        s.chat
            .values()
            .any(|m| m.attachments.iter().any(|x| x.id == blob))
    })
    .await;
    assert_eq!(
        b.state()
            .chat
            .values()
            .filter(|m| !m.attachments.is_empty())
            .count(),
        1,
        "stored once"
    );
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

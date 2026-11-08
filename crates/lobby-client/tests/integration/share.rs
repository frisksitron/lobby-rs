//! End to end: a real in-process `lobby-server` (str0m SFU on loopback UDP) and two
//! Members running the shipped media engine.
#![cfg(windows)]

use std::time::Duration;

use lobby_client::{
    CaptureFaults, IssueKind, MediaConfig, MemoryTokenStore, ScreenSource, SourceKind, TestMotion,
    VideoFeed, VideoFrame, VideoStats, VoicePhase, copy, testing::test_client,
};
use lobby_protocol::{Quality, encoder::H264_LEVEL};
use lobby_server::{MediaPath, testing::TestServer};

use crate::common::{Member, WAIT, active, origin, streaming, until};

async fn assert_level(viewer: &Member, mode: &str) {
    let id = viewer
        .watched_profile_level()
        .await
        .expect("an SPS was decoded");
    let (profile_idc, constraints, level_idc) = (id >> 16, (id >> 8) & 0xff, id & 0xff);
    assert_eq!(profile_idc, 0x42, "baseline at {mode}: {id:#08x}");
    assert_ne!(
        constraints & 0x40,
        0,
        "constraint_set1 at {mode}: {id:#08x}"
    );
    assert_eq!(
        level_idc,
        u32::from(H264_LEVEL.level_idc()),
        "level at {mode}: {id:#08x}"
    );
}

/// How far, per channel, a decoded corner may be from the test pattern's colour there.
const COLOUR_TOLERANCE: u8 = 8;

/// The test pattern's gradient at `(x, y)` of its 1920×1080 picture, as B, G, R.
fn pattern_colour(x: u32, y: u32) -> [u8; 3] {
    [
        (40 + x * 120 / 1920) as u8,
        (30 + y * 60 / 1080) as u8,
        (60 + (x + y) * 80 / 3000) as u8,
    ]
}

/// The corners of `frame` show the test pattern's colours, so its pixels came through the
/// scaling and both colour conversions intact. The moving square covers one corner at most.
fn assert_pattern_colours(frame: &VideoFrame, what: &str) {
    let (w, h) = (frame.width, frame.height);
    let corners = [(8, 8), (w - 9, 8), (8, h - 9), (w - 9, h - 9)];
    let seen: Vec<([u8; 3], [u8; 3])> = corners
        .iter()
        .map(|&(x, y)| {
            let at = ((y * w + x) * 4) as usize;
            let got = [frame.bgra[at], frame.bgra[at + 1], frame.bgra[at + 2]];
            (got, pattern_colour(x * 1920 / w, y * 1080 / h))
        })
        .collect();
    let close = seen
        .iter()
        .filter(|(got, want)| {
            got.iter()
                .zip(want)
                .all(|(g, w)| g.abs_diff(*w) <= COLOUR_TOLERANCE)
        })
        .count();
    assert!(
        close >= 3,
        "{what}: the corners are not the pattern's colours: {seen:?}"
    );
}

/// Until `feed` closes, however many pictures were still on their way, within `WAIT`.
async fn feed_closes(a: &mut Member, b: &mut Member, feed: &mut VideoFeed) {
    let end = tokio::time::Instant::now() + WAIT;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(end) => panic!("the feed stayed open for {WAIT:?}"),
            more = a.step() => assert!(more, "a's session ended"),
            more = b.step() => assert!(more, "b's session ended"),
            f = feed.frames.recv() => if f.is_none() { break },
        }
    }
}

/// The next `n` pictures of `feed`, however long they take within `WAIT`.
async fn next_frames(
    a: &mut Member,
    b: &mut Member,
    feed: &mut VideoFeed,
    n: usize,
) -> Vec<VideoFrame> {
    let mut got = Vec::new();
    let end = tokio::time::Instant::now() + WAIT;
    while got.len() < n {
        tokio::select! {
            () = tokio::time::sleep_until(end) => {
                panic!("{} of {n} pictures within {WAIT:?}", got.len())
            }
            more = a.step() => assert!(more, "a's session ended"),
            more = b.step() => assert!(more, "b's session ended"),
            f = feed.frames.recv() => got.push(f.expect("the feed stays open")),
        }
    }
    got
}

async fn two_in_voice(server: &TestServer) -> (Member, Member) {
    two_in_voice_with(server, MediaConfig::fake()).await
}

async fn two_in_voice_with(server: &TestServer, alices: MediaConfig) -> (Member, Member) {
    let origin = origin(server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));
    let mut a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    let mut b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();
    a.join_voice(alices).await.unwrap();
    b.join_voice(MediaConfig::fake()).await.unwrap();
    until(&mut a, &mut b, "both In Voice on both", |sa, sb| {
        ["alice", "bob"]
            .iter()
            .all(|n| active(sa, n) && active(sb, n))
    })
    .await;
    (a, b)
}

/// Steps both clients until the SFU sends `name`'s media over UDP. A call can start on its TCP
/// pair, which a relay's blackhole does not cut for UDP alone.
async fn until_on_udp(server: &TestServer, a: &mut Member, b: &mut Member, name: &str) {
    let user = server.user_id_of(name).await;
    let on_udp = tokio::time::timeout(WAIT, async {
        while server.server.sfu_media_path(&user) != Some(MediaPath::Udp) {
            a.run_for(Duration::from_millis(10)).await;
            b.run_for(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(on_udp.is_ok(), "{name}'s media over UDP");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_watches_a_share_from_a_keyframe_and_both_can_stop() {
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut a, mut b) = two_in_voice(&server).await;

    a.start_share(
        ScreenSource::test_pattern(TestMotion::Thirty),
        Quality::UpTo2160,
    )
    .await
    .unwrap();
    assert!(a.state().share.sharing.is_some());
    until(&mut a, &mut b, "alice streaming on both", |sa, sb| {
        streaming(sa, "alice") && streaming(sb, "alice")
    })
    .await;

    let mut feed = b.watch("alice").await.unwrap();
    let frames = next_frames(&mut a, &mut b, &mut feed, 10).await;
    let frame = &frames[0];
    assert_eq!((frame.width, frame.height), (1920, 1080));
    assert_eq!(frame.bgra.len(), 1920 * 1080 * 4);
    assert_level(&b, "1080p").await;
    let last = frames.last().unwrap();
    let px = |x: usize, y: usize| &last.bgra[(y * 1920 + x) * 4..(y * 1920 + x) * 4 + 3];
    assert_ne!(px(10, 10), px(1900, 1000), "a gradient, not a flat picture");
    assert!(
        px(10, 10).iter().any(|c| *c > 20),
        "not black: {:?}",
        px(10, 10)
    );
    assert_pattern_colours(last, "the Viewer's picture");
    let flowing = |video: Option<&VideoStats>| {
        video.is_some_and(|v| {
            v.bits_per_second.is_some_and(|b| b > 0) && v.size == Some((1920, 1080))
        })
    };
    until(
        &mut a,
        &mut b,
        "the share in both clients' stats",
        |sa, sb| {
            let (out, watched) = (sa.voice.stats.as_ref(), sb.voice.stats.as_ref());
            out.is_some_and(|s| flowing(s.share_out.as_ref()) && s.share_in.is_none())
                && watched.is_some_and(|s| flowing(s.share_in.as_ref()) && s.share_out.is_none())
        },
    )
    .await;
    let encoder = a.state().voice.stats.as_ref().and_then(|s| {
        let share = s.share_out.as_ref()?;
        share.encoder.clone()
    });
    assert!(
        encoder.is_some_and(|e| !e.is_empty()),
        "the stats name the encoder"
    );

    b.unwatch().await;
    assert_eq!(b.state().share.watch, None);
    feed_closes(&mut a, &mut b, &mut feed).await;
    assert!(streaming(b.state(), "alice"));

    a.stop_share().await;
    until(&mut a, &mut b, "the share gone on both", |sa, sb| {
        !streaming(sa, "alice") && !streaming(sb, "alice")
    })
    .await;
    let refused = b.watch("alice").await.expect_err("the share is gone");
    assert_eq!(refused.message, copy::SHARE_NOT_AVAILABLE);
    assert_eq!(
        b.issue(IssueKind::ShareFailed).as_deref(),
        Some(copy::SHARE_NOT_AVAILABLE)
    );
    b.take_effects();

    a.start_share(
        ScreenSource::test_pattern(TestMotion::Thirty),
        Quality::UpTo2160,
    )
    .await
    .unwrap();
    until(&mut a, &mut b, "streaming again", |sa, sb| {
        streaming(sa, "alice") && streaming(sb, "alice")
    })
    .await;
    a.stop_share().await;
    a.wait_until("a's own share gone", |s| !streaming(s, "alice"))
        .await;
    // b's Session may not have heard yet: then the Lobby refuses the Watch, with the same words.
    let said = match b.ask_watch("alice").await {
        Ok(()) => b.wait_issue(IssueKind::ShareFailed).await,
        Err(refused) => refused.message.into_owned(),
    };
    assert_eq!(said, copy::SHARE_NOT_AVAILABLE);
    b.wait_until("the Watch over", |s| s.share.watch.is_none())
        .await;

    a.start_share(
        ScreenSource::test_pattern(TestMotion::Thirty),
        Quality::UpTo2160,
    )
    .await
    .unwrap();
    until(&mut a, &mut b, "streaming a third time", |_, sb| {
        streaming(sb, "alice")
    })
    .await;
    a.leave_voice().await;
    assert!(a.state().share.sharing.is_none());
    until(
        &mut a,
        &mut b,
        "alice out of Voice and not sharing",
        |_, sb| !streaming(sb, "alice") && !active(sb, "alice"),
    )
    .await;
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_share_encoded_on_the_gpu_shows_the_same_picture() {
    let Some(name) = lobby_client::hardware_encoder() else {
        eprintln!("skipped: this machine's GPU has no hardware H.264 encoder");
        return;
    };
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let (mut a, mut b) = two_in_voice(&server).await;

    a.start_share(
        ScreenSource::test_pattern(TestMotion::Thirty),
        Quality::UpTo2160,
    )
    .await
    .unwrap();
    until(&mut a, &mut b, "alice streaming on both", |sa, sb| {
        streaming(sa, "alice") && streaming(sb, "alice")
    })
    .await;
    let mut feed = b.watch("alice").await.unwrap();
    let frames = next_frames(&mut a, &mut b, &mut feed, 10).await;
    let last = frames.last().unwrap();
    assert_eq!((last.width, last.height), (1920, 1080));
    assert_pattern_colours(last, "the Viewer's picture");
    assert_level(&b, "the GPU's 1080p").await;
    until(
        &mut a,
        &mut b,
        "the encoder named in alice's stats",
        |sa, _| {
            let share = sa.voice.stats.as_ref().and_then(|s| s.share_out.as_ref());
            share.and_then(|v| v.encoder.as_deref()) == Some(name.as_str())
        },
    )
    .await;
    let mut own = a.own_share_feed().expect("a feed while sharing");
    let copies = next_frames(&mut a, &mut b, &mut own, 1).await;
    assert_eq!((copies[0].width, copies[0].height), (1280, 720));
    assert_pattern_colours(&copies[0], "the Sharer's own view");
    b.unwatch().await;
    a.stop_share().await;
    until(&mut a, &mut b, "the share gone on both", |sa, sb| {
        !streaming(sa, "alice") && !streaming(sb, "alice")
    })
    .await;

    a.start_share(
        ScreenSource::test_pattern(TestMotion::Still),
        Quality::UpTo2160,
    )
    .await
    .unwrap();
    until(&mut a, &mut b, "the still share on both", |sa, sb| {
        streaming(sa, "alice") && streaming(sb, "alice")
    })
    .await;
    let mut feed = b.watch("alice").await.unwrap();
    let still = next_frames(&mut a, &mut b, &mut feed, 1).await;
    assert_pattern_colours(&still[0], "the still share's picture");

    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

/// Alice's media path drops while bob watches her share, and she presses Stop Share while her
/// client gets the path back: the share stops, and bob sees it end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_share_during_a_reconnect_ends_the_share_and_its_viewers_see_it_end() {
    let (server, relay) = TestServer::start_behind_relay(&["alice", "bob"], |_| {}).await;
    let (mut a, mut b) = two_in_voice(&server).await;
    a.start_share(
        ScreenSource::test_pattern(TestMotion::Thirty),
        Quality::UpTo2160,
    )
    .await
    .unwrap();
    until(&mut a, &mut b, "alice streaming on both", |sa, sb| {
        streaming(sa, "alice") && streaming(sb, "alice")
    })
    .await;
    let mut feed = b.watch("alice").await.unwrap();
    next_frames(&mut a, &mut b, &mut feed, 1).await;

    until_on_udp(&server, &mut a, &mut b, "bob").await;
    let alice = a.voice_media_addr().await.expect("alice's media socket");
    // Her TCP pair too, which ICE would otherwise move to.
    relay.blackhole_client(alice, true);
    relay.blackhole_tcp(true);
    until(&mut a, &mut b, "alice reconnecting", |sa, _| {
        matches!(
            sa.voice.phase,
            VoicePhase::InVoice {
                reconnecting: true,
                ..
            }
        )
    })
    .await;
    a.stop_share().await;
    until(&mut a, &mut b, "the share ended on both", |sa, sb| {
        sa.share.sharing.is_none()
            && !streaming(sa, "alice")
            && !streaming(sb, "alice")
            && sb.share.watch.is_none()
    })
    .await;
    relay.blackhole_client(alice, false);
    relay.blackhole_tcp(false);
    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

/// A capture that breaks while it runs (the GPU removed under it, a picture that cannot be read
/// back) ends the share with a notification, as one that cannot start does, and Voice goes on.
/// Windows Graphics Capture stops on such an error without a word, so this shares a real
/// monitor; nothing of it is sent, since its first picture already fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capture_that_fails_while_sharing_ends_the_share_and_says_why() {
    let monitor = lobby_client::list_sources(false)
        .unwrap()
        .into_iter()
        .find(|s| s.kind() == SourceKind::Screen);
    let Some(monitor) = monitor else {
        eprintln!("skipped: this machine has no monitor to capture");
        return;
    };
    let server = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let breaks = MediaConfig {
        capture_faults: CaptureFaults { fail_from: Some(0) },
        ..MediaConfig::fake()
    };
    let (mut a, mut b) = two_in_voice_with(&server, breaks).await;

    a.start_share(monitor, Quality::UpTo2160).await.unwrap();
    a.wait_until("the share over", |s| s.share.sharing.is_none())
        .await;
    a.issue(IssueKind::ShareFailed)
        .expect("a notification says why");
    until(&mut a, &mut b, "nobody streaming", |sa, sb| {
        !streaming(sa, "alice") && !streaming(sb, "alice")
    })
    .await;
    assert!(active(a.state(), "alice"), "still In Voice");

    a.shutdown().await;
    b.shutdown().await;
    assert!(server.shutdown().await.is_clean());
}

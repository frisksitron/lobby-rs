//! Watch end to end: a real in-process `lobby-server` and three Members on the shipped media
//! engine. Alice shares a red test pattern and carol a blue one, and bob watches. Each picture's
//! colour says whose share it shows, so the test checks what bob sees, not frame counts.
#![cfg(windows)]

use std::time::Duration;

use lobby_client::{
    MediaConfig, MemoryTokenStore, ScreenSource, TestMotion, VideoFeed, VideoFrame,
    testing::test_client,
};
use lobby_protocol::Quality;
use lobby_server::testing::TestServer;

use crate::common::{Member, WAIT, active, origin, streaming};

const NAMES: [&str; 3] = ["alice", "bob", "carol"];
const RED: [u8; 3] = [40, 40, 200];
const BLUE: [u8; 3] = [200, 90, 40];

type Members = [Member; 3];
/// How far, per channel, a decoded corner may be from its pattern's colour.
const TOLERANCE: u8 = 30;

/// Whose share `frame` shows, by its colour: three of its corners match (the moving square
/// covers one at most).
fn painted_by(frame: &VideoFrame) -> &'static str {
    let (w, h) = (frame.width, frame.height);
    let corners = [(8, 8), (w - 9, 8), (8, h - 9), (w - 9, h - 9)];
    let near = |want: [u8; 3]| {
        let close = corners.iter().filter(|&&(x, y)| {
            let at = ((y * w + x) * 4) as usize;
            let got = &frame.bgra[at..at + 3];
            got.iter()
                .zip(want)
                .all(|(g, w)| g.abs_diff(w) <= TOLERANCE)
        });
        close.count() >= 3
    };
    match (near(RED), near(BLUE)) {
        (true, false) => "alice",
        (false, true) => "carol",
        _ => "nobody",
    }
}

/// A picture bob got: whose its colour says it is, and whose it says it is.
struct Seen {
    painted: &'static str,
    named: String,
}

async fn two_sharers(server: &TestServer) -> Members {
    let origin = origin(server);
    let mut all = Vec::new();
    for name in NAMES {
        let store = MemoryTokenStore::new();
        let mut c = Member::sign_in(&test_client(&store), &origin, name, &server.mail)
            .await
            .unwrap();
        c.join_voice(MediaConfig::fake()).await.unwrap();
        all.push(c);
    }
    let mut all: Members = all.try_into().ok().expect("three Members");
    step(&mut all, "all In Voice", |all| {
        all.iter()
            .all(|c| NAMES.iter().all(|n| active(c.state(), n)))
    })
    .await;
    for (i, colour) in [(0, RED), (2, BLUE)] {
        let pattern = ScreenSource::test_pattern_tinted(TestMotion::Thirty, colour);
        all[i]
            .start_share(pattern, Quality::UpTo2160)
            .await
            .unwrap();
        let name = NAMES[i];
        step(&mut all, "the share streaming for bob", |all| {
            streaming(all[1].state(), name)
        })
        .await;
    }
    all
}

async fn step(all: &mut Members, what: &str, done: impl Fn(&Members) -> bool) {
    let end = tokio::time::Instant::now() + WAIT;
    while !done(all) {
        let [a, b, c] = &mut *all;
        tokio::select! {
            () = tokio::time::sleep_until(end) => panic!("timed out waiting for {what}"),
            more = a.step() => assert!(more, "alice's session ended"),
            more = b.step() => assert!(more, "bob's session ended"),
            more = c.step() => assert!(more, "carol's session ended"),
        }
    }
}

/// What bob's feed shows until `done` holds of the pictures so far, or for `window` when there is
/// no condition, or until the feed closes.
async fn seen_until(
    all: &mut Members,
    feed: &mut VideoFeed,
    window: Option<Duration>,
    done: impl Fn(&[Seen]) -> bool,
) -> Vec<Seen> {
    let mut seen = Vec::new();
    let end = tokio::time::Instant::now() + window.unwrap_or(WAIT);
    while window.is_some() || !done(&seen) {
        let [a, b, c] = &mut *all;
        tokio::select! {
            () = tokio::time::sleep_until(end) => {
                assert!(window.is_some(), "timed out waiting for the pictures");
                break;
            }
            more = a.step() => assert!(more, "alice's session ended"),
            more = b.step() => assert!(more, "bob's session ended"),
            more = c.step() => assert!(more, "carol's session ended"),
            f = feed.frames.recv() => {
                let Some(f) = f else { break };
                let named = f.sharer.as_ref().and_then(|id| b.state().members.get(id));
                seen.push(Seen {
                    painted: painted_by(&f),
                    named: named.map_or("nobody".into(), |m| m.username.to_string()),
                });
            }
        }
    }
    seen
}

async fn seen_for(all: &mut Members, feed: &mut VideoFeed, window: Duration) -> Vec<Seen> {
    seen_until(all, feed, Some(window), |_| false).await
}

fn ends_on(seen: &[Seen], who: &str) -> bool {
    let tail = &seen[seen.len().saturating_sub(10)..];
    tail.len() == 10 && tail.iter().all(|s| s.painted == who)
}

/// Every picture is of the share it names.
fn assert_named_right(seen: &[Seen], what: &str) {
    let wrong: Vec<_> = seen
        .iter()
        .filter(|s| s.painted != s.named)
        .map(|s| (s.painted, s.named.as_str()))
        .collect();
    assert!(
        wrong.is_empty(),
        "{what}: pictures under another's name: {wrong:?}"
    );
}

/// Alice, carol, then alice again before carol's picture can come, and again after it did:
/// bob ends on alice's picture, and no picture of carol's ever comes under alice's name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn back_and_forth_quickly_never_shows_a_share_under_the_others_name() {
    let server = TestServer::start_with_members(&NAMES, |_| {}).await;
    let mut all = two_sharers(&server).await;
    let mut feed = all[1].watch("alice").await.unwrap();
    let first = seen_until(&mut all, &mut feed, None, |seen| ends_on(seen, "alice")).await;
    assert_named_right(&first, "watching alice");
    for gap in [0, 150, 400] {
        all[1].ask_watch("carol").await.unwrap();
        let between = seen_for(&mut all, &mut feed, Duration::from_millis(gap)).await;
        all[1].ask_watch("alice").await.unwrap();
        let mut seen = seen_until(&mut all, &mut feed, None, |seen| ends_on(seen, "alice")).await;
        seen.extend(seen_for(&mut all, &mut feed, Duration::from_millis(500)).await);
        let what = format!("{gap} ms on carol");
        assert_named_right(&between, &what);
        assert_named_right(&seen, &what);
        assert!(ends_on(&seen, "alice"), "{what}: ends on alice's picture");
        assert_eq!(all[1].watching(), Some(("alice".into(), true)), "{what}");
    }
    for c in all {
        c.shutdown().await;
    }
    assert!(server.shutdown().await.is_clean());
}

//! Measures a Screen Share by numbers: an in-process Lobby, Alice sharing the test pattern, Bob
//! watching. Prints one JSON object on stdout and a short summary on stderr.
//!
//! ```text
//! cargo run -p lobby-client --example share_bench -- \
//!     --size 3840x2160 --workload scroll --quality 2160 --seconds 30
//! ```
//!
//! Flags, all optional:
//! - `--size 1280x720|1920x1080|2560x1440|3840x2160`, the source picture.
//! - `--workload square30|square60|scroll|scroll-texture|scroll-noise`: the bouncing square at
//!   30 or 60 fps, or scrolling text at 60, alone or with a quarter of the picture a moving
//!   texture or fresh noise.
//! - `--seconds N`: seconds measured, after the warm-up (default 60). `--warmup N` (default 10).
//! - `--quality 1080|1440|2160`: the most the share sends (default 2160).
//! - `--downlink-kbps N`: Bob's downlink, shaped by a relay.
//! - `--psnr-every N`: PSNR of every Nth picture of the scroll workloads (default 15, 0 is off),
//!   over the whole picture and over the busy quarter alone.
//!
//! Every picture carries a stamp (frame number and wall-clock capture time), so Bob's pictures
//! are counted and timed by what they say about themselves. Each second the bench also reads
//! the bytes Alice, the SFU and Bob sent by UDP: video, audio, padding, retransmissions, RTCP.

use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};

use lobby_client::{
    MediaConfig, MemoryTokenStore, ScreenSource, TestMotion, VideoFeed, VideoFrame, WatchLoad,
    testing::{
        ScrollSource, Stamp, busy_region, luma_psnr, luma_psnr_in, read_stamp, test_client,
        unix_micros,
    },
};
use lobby_protocol::Quality;
use lobby_server::{
    MediaPath,
    testing::{Bottleneck, Direction, MediaRelay, TestServer},
};
use serde_json::{Value, json};
use tokio::sync::oneshot;

#[path = "../tests/integration/common/mod.rs"]
#[allow(dead_code)]
mod common;

use common::{Member, WAIT, active, origin, streaming, until};

const USAGE: &str = "usage: share_bench [--size WxH] \
[--workload square30|square60|scroll|scroll-texture|scroll-noise] [--seconds N] [--warmup N] \
[--quality 1080|1440|2160] [--downlink-kbps N] [--psnr-every N]";

/// A relay's queue for a shaped downlink, as the slow-link tests use.
const DOWNLINK_QUEUE: Duration = Duration::from_millis(300);
/// Pictures kept for PSNR, which is computed once the share is over: a thread beside a running
/// share held the hardware encoder at 30 fps. Past this many bytes of them, later pictures are
/// not compared.
const PSNR_KEPT_BYTES: usize = 768 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Workload {
    Square30,
    Square60,
    Scroll,
    ScrollTexture,
    ScrollNoise,
}

impl Workload {
    fn motion(self) -> TestMotion {
        match self {
            Workload::Square30 => TestMotion::Thirty,
            Workload::Square60 => TestMotion::Sixty,
            Workload::Scroll => TestMotion::Scroll,
            Workload::ScrollTexture => TestMotion::ScrollTexture,
            Workload::ScrollNoise => TestMotion::ScrollNoise,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Workload::Square30 => "square30",
            Workload::Square60 => "square60",
            Workload::Scroll => "scroll",
            Workload::ScrollTexture => "scroll-texture",
            Workload::ScrollNoise => "scroll-noise",
        }
    }

    /// A scroll's pictures are a function of their number, so PSNR can regenerate them.
    fn scrolls(self) -> bool {
        matches!(
            self,
            Workload::Scroll | Workload::ScrollTexture | Workload::ScrollNoise
        )
    }

    /// Whether the picture has a busy region, which PSNR also measures alone.
    fn busy(self) -> bool {
        matches!(self, Workload::ScrollTexture | Workload::ScrollNoise)
    }
}

struct Args {
    size: (u32, u32),
    workload: Workload,
    seconds: u64,
    warmup: u64,
    quality: Quality,
    downlink_kbps: Option<u64>,
    psnr_every: u32,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        size: (1920, 1080),
        workload: Workload::Scroll,
        seconds: 60,
        warmup: 10,
        quality: Quality::UpTo2160,
        downlink_kbps: None,
        psnr_every: 15,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|_| format!("{flag}: `{text}` is not a number"))
        };
        match flag.as_str() {
            "--size" => {
                let text = value()?;
                out.size = match text.as_str() {
                    "1280x720" => (1280, 720),
                    "1920x1080" => (1920, 1080),
                    "2560x1440" => (2560, 1440),
                    "3840x2160" => (3840, 2160),
                    _ => return Err(format!("--size is a standard size, not {text}")),
                };
            }
            "--workload" => {
                out.workload = match value()?.as_str() {
                    "square30" => Workload::Square30,
                    "square60" => Workload::Square60,
                    "scroll" => Workload::Scroll,
                    "scroll-texture" => Workload::ScrollTexture,
                    "scroll-noise" => Workload::ScrollNoise,
                    other => return Err(format!("unknown workload {other}")),
                };
            }
            "--seconds" => out.seconds = number(value()?)?,
            "--warmup" => out.warmup = number(value()?)?,
            "--quality" => {
                out.quality = match value()?.as_str() {
                    "1080" => Quality::UpTo1080,
                    "1440" => Quality::UpTo1440,
                    "2160" => Quality::UpTo2160,
                    other => return Err(format!("unknown quality {other}")),
                };
            }
            "--downlink-kbps" => out.downlink_kbps = Some(number(value()?)?),
            "--psnr-every" => out.psnr_every = number(value()?)? as u32,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if out.seconds == 0 {
        return Err("--seconds must be at least 1".into());
    }
    Ok(out)
}

struct Picture {
    at_micros: u64,
    stamp: Option<Stamp>,
    size: (u32, u32),
}

/// What the receive task is told up front, so it decides nothing per picture beyond a comparison.
struct Plan {
    measured_from_micros: u64,
    source: (u32, u32),
    /// The pictures kept for PSNR after the run: those of the source's size whose number divides
    /// by this, up to `keep`.
    psnr_every: u32,
    keep: usize,
    capacity: usize,
}

async fn receive(
    mut feed: VideoFeed,
    mut stop: oneshot::Receiver<()>,
    plan: Plan,
) -> (Vec<Picture>, Vec<(Stamp, VideoFrame)>) {
    let mut pictures = Vec::with_capacity(plan.capacity);
    let mut kept = Vec::with_capacity(plan.keep);
    loop {
        let frame = tokio::select! {
            frame = feed.frames.recv() => frame,
            _ = &mut stop => break,
        };
        let Some(frame) = frame else { break };
        let at_micros = unix_micros();
        let size = (frame.width, frame.height);
        let stamp = read_stamp(&frame);
        pictures.push(Picture {
            at_micros,
            stamp,
            size,
        });
        if let Some(stamp) = stamp
            && kept.len() < plan.keep
            && at_micros >= plan.measured_from_micros
            && stamp.frame.is_multiple_of(plan.psnr_every)
            && size == plan.source
        {
            kept.push((stamp, frame));
        }
    }
    (pictures, kept)
}

/// What had been sent by a moment: Alice's share video, frames and bytes as `share_video_sent`
/// counts them, and the bytes each end had sent by UDP.
struct Sent {
    at: Instant,
    frames: u64,
    bytes: u64,
    wire: Wire,
}

/// Bytes, or bits per second, that each end sent by UDP.
#[derive(Debug, Clone, Copy)]
struct Wire {
    alice: u64,
    sfu: u64,
    bob: u64,
}

async fn sent_now(a: &Member, b: &Member, server: &TestServer) -> Sent {
    let at = Instant::now();
    let (frames, bytes) = a.share_video_sent().await.unwrap_or_default();
    Sent {
        at,
        frames,
        bytes,
        wire: Wire {
            alice: a.udp_bytes_sent().await.unwrap_or_default(),
            sfu: server.server.sfu_udp_bytes_sent(),
            bob: b.udp_bytes_sent().await.unwrap_or_default(),
        },
    }
}

struct Second {
    bps: u64,
    fps: f64,
    size: Option<(u32, u32)>,
    wire_bps: Wire,
}

/// The mean and the highest of some per-second rates.
fn mean_max(rates: impl Iterator<Item = u64>) -> Value {
    let rates: Vec<u64> = rates.collect();
    json!({
        "mean": rates.iter().sum::<u64>() / rates.len().max(1) as u64,
        "max": rates.iter().max(),
    })
}

fn round(v: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (v * scale).round() / scale
}

/// Nearest-rank percentile of an ascending list.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    let n = sorted.len();
    (n > 0).then(|| sorted[((p / 100.0 * n as f64).ceil() as usize).clamp(1, n) - 1])
}

fn size_name((w, h): (u32, u32)) -> String {
    format!("{w}x{h}")
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let (report, summary) = run(&args).await;
    eprintln!("{summary}");
    println!("{report}");
}

async fn run(args: &Args) -> (Value, String) {
    let (server, relay) = match args.downlink_kbps {
        Some(_) => {
            let (server, relay) = TestServer::start_behind_relay(&["alice", "bob"], |_| {}).await;
            (server, Some(relay))
        }
        None => (
            TestServer::start_with_members(&["alice", "bob"], |_| {}).await,
            None,
        ),
    };
    let origin = origin(&server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));
    let mut a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    let mut b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();
    a.join_voice(MediaConfig::fake()).await.unwrap();
    b.join_voice(MediaConfig::fake()).await.unwrap();
    until(&mut a, &mut b, "both In Voice on both", |sa, sb| {
        ["alice", "bob"]
            .iter()
            .all(|n| active(sa, n) && active(sb, n))
    })
    .await;
    if let (Some(kbps), Some(relay)) = (args.downlink_kbps, &relay) {
        shape_downlink(&server, relay, &mut a, &mut b, kbps).await;
    }

    let source = ScreenSource::test_pattern_sized(args.workload.motion(), args.size);
    a.start_share(source, args.quality).await.unwrap();
    until(&mut a, &mut b, "alice streaming on both", |sa, sb| {
        [sa, sb].iter().all(|state| streaming(state, "alice"))
    })
    .await;
    let feed = b.watch("alice").await.unwrap();

    let started = Instant::now();
    let measured_from_micros = unix_micros() + args.warmup * 1_000_000;
    let (stop, stopped) = oneshot::channel();
    let picture_bytes = (args.size.0 * args.size.1 * 4) as usize;
    let keep = if args.workload.scrolls() && args.psnr_every > 0 {
        PSNR_KEPT_BYTES / picture_bytes
    } else {
        0
    };
    let receiver = tokio::spawn(receive(
        feed,
        stopped,
        Plan {
            measured_from_micros,
            source: args.size,
            psnr_every: args.psnr_every,
            keep,
            capacity: (args.warmup + args.seconds + 2) as usize * 70,
        },
    ));

    let total = args.warmup + args.seconds;
    let mut sent = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;
    let mut before = sent_now(&a, &b, &server).await;
    let mut encoder = None;
    while (sent.len() as u64) < total {
        tokio::select! {
            _ = tick.tick() => {
                let now = sent_now(&a, &b, &server).await;
                let secs = (now.at - before.at).as_secs_f64();
                let bps = |now: u64, before: u64| {
                    (now.saturating_sub(before) as f64 * 8.0 / secs) as u64
                };
                let stats = a.state().voice.stats.as_ref().and_then(|s| s.share_out.as_ref());
                encoder = stats.and_then(|s| s.encoder.clone()).or(encoder);
                sent.push(Second {
                    bps: bps(now.bytes, before.bytes),
                    fps: (now.frames - before.frames) as f64 / secs,
                    size: stats.and_then(|s| s.size),
                    wire_bps: Wire {
                        alice: bps(now.wire.alice, before.wire.alice),
                        sfu: bps(now.wire.sfu, before.wire.sfu),
                        bob: bps(now.wire.bob, before.wire.bob),
                    },
                });
                before = now;
            }
            more = a.step() => assert!(more, "alice's session ended"),
            more = b.step() => assert!(more, "bob's session ended"),
        }
    }
    let ran = started.elapsed();
    let _ = stop.send(());
    let (pictures, kept) = receiver.await.expect("receive task");
    let measured_from = started + Duration::from_secs(args.warmup);
    let loads: Vec<WatchLoad> = b
        .watch_loads()
        .await
        .into_iter()
        .filter(|(at, _)| *at >= measured_from)
        .map(|(_, load)| load)
        .collect();
    b.unwatch().await;
    a.stop_share().await;
    a.shutdown().await;
    b.shutdown().await;
    let clean = server.shutdown().await.is_clean();
    let region = args.workload.busy().then(|| busy_region(args.size));
    let psnr = (keep > 0).then(|| {
        let mut source = ScrollSource::new(args.workload.motion(), args.size);
        kept.iter()
            .map(|(stamp, frame)| {
                let picture = source.picture(*stamp);
                (
                    luma_psnr(picture, &frame.bgra, args.size),
                    region.map(|r| luma_psnr_in(picture, &frame.bgra, args.size, r)),
                )
            })
            .collect::<Vec<_>>()
    });

    let sent = &sent[args.warmup as usize..];
    let measured = Measured::of(args, &pictures, measured_from_micros);
    let report = json!({
        "size": size_name(args.size),
        "workload": args.workload.name(),
        "quality": format!("{:?}", args.quality),
        "downlink_kbps": args.downlink_kbps,
        "warmup_seconds": args.warmup,
        "measured_seconds": args.seconds,
        "ran_seconds": round(ran.as_secs_f64(), 1),
        "sharer": {
            "encoder": encoder.as_deref(),
            "video_bitrate_bps": mean_max(sent.iter().map(|s| s.bps)),
            "frames_per_second": {
                "mean": round(sent.iter().map(|s| s.fps).sum::<f64>() / sent.len().max(1) as f64, 1),
                "min": sent.iter().map(|s| s.fps).reduce(f64::min).map(|v| round(v, 1)),
            },
            "picture_sizes_sent": histogram(sent.iter().filter_map(|s| s.size)),
        },
        "viewer": measured.viewer(),
        "viewer_threads": threads_report(&loads),
        "udp_bps": {
            "alice": mean_max(sent.iter().map(|s| s.wire_bps.alice)),
            "sfu": mean_max(sent.iter().map(|s| s.wire_bps.sfu)),
            "bob": mean_max(sent.iter().map(|s| s.wire_bps.bob)),
        },
        "psnr_y": psnr_report(args, psnr.as_deref(), &measured),
        "server_shutdown_clean": clean,
    });
    let summary = format!(
        "{}\n{}",
        measured.summary(args, sent),
        threads_summary(&loads)
    );
    (report, summary)
}

fn threads_report(loads: &[WatchLoad]) -> Value {
    let percents = |of: fn(&WatchLoad) -> u32| mean_max(loads.iter().map(|l| u64::from(of(l))));
    json!({
        "seconds": loads.len(),
        "decode_busy_percent": percents(WatchLoad::decode_percent),
        "convert_busy_percent": percents(WatchLoad::convert_percent),
        "seconds_over_90_percent": loads
            .iter()
            .filter(|l| l.decode_percent() > 90 || l.convert_percent() > 90)
            .count(),
        "lost": loads.iter().map(|l| l.lost).sum::<u64>(),
        "ui_dropped": loads.iter().map(|l| l.ui_dropped).sum::<u64>(),
    })
}

fn threads_summary(loads: &[WatchLoad]) -> String {
    let n = loads.len().max(1) as u64;
    let mean = |of: fn(&WatchLoad) -> u32| loads.iter().map(|l| u64::from(of(l))).sum::<u64>() / n;
    let max = |of: fn(&WatchLoad) -> u32| loads.iter().map(of).max().unwrap_or(0);
    format!(
        "viewer threads, % busy mean/max: decode {}/{}, conversion {}/{}; lost {}",
        mean(WatchLoad::decode_percent),
        max(WatchLoad::decode_percent),
        mean(WatchLoad::convert_percent),
        max(WatchLoad::convert_percent),
        loads.iter().map(|l| l.lost).sum::<u64>(),
    )
}

async fn shape_downlink(
    server: &TestServer,
    relay: &MediaRelay,
    a: &mut Member,
    b: &mut Member,
    kbps: u64,
) {
    let user = server.user_id_of("alice").await;
    let on_udp = tokio::time::timeout(WAIT, async {
        while server.server.sfu_media_path(&user) != Some(MediaPath::Udp) {
            a.run_for(Duration::from_millis(10)).await;
            b.run_for(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(on_udp.is_ok(), "alice's media over UDP");
    relay.narrow_client(
        b.voice_media_addr().await.expect("bob's media socket"),
        Direction::Down,
        Bottleneck {
            bps: kbps * 1000,
            queue: DOWNLINK_QUEUE,
        },
    );
}

fn histogram(sizes: impl Iterator<Item = (u32, u32)>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for size in sizes {
        *counts.entry(size_name(size)).or_default() += 1;
    }
    counts
}

/// What Bob's pictures in the measured window say.
struct Measured {
    pictures: usize,
    unstamped: usize,
    /// Distinct pictures in each measured second.
    per_second: Vec<u32>,
    /// The longest time without a new picture, from the window's start to its end.
    longest_gap_ms: f64,
    /// Receive time minus capture time of each distinct picture, ascending, in ms.
    latency_ms: Vec<f64>,
    sizes: BTreeMap<String, usize>,
}

impl Measured {
    fn of(args: &Args, all: &[Picture], from_micros: u64) -> Measured {
        let to_micros = from_micros + args.seconds * 1_000_000;
        let window: Vec<&Picture> = all
            .iter()
            .filter(|p| (from_micros..to_micros).contains(&p.at_micros))
            .collect();
        let mut seen: HashSet<u32> = all
            .iter()
            .filter(|p| p.at_micros < from_micros)
            .filter_map(|p| p.stamp.map(|s| s.frame))
            .collect();
        let mut per_second = vec![0u32; args.seconds as usize];
        let (mut latency_ms, mut arrivals) = (Vec::new(), Vec::new());
        for p in &window {
            let Some(stamp) = p.stamp else { continue };
            if !seen.insert(stamp.frame) {
                continue;
            }
            per_second[((p.at_micros - from_micros) / 1_000_000) as usize] += 1;
            arrivals.push(p.at_micros);
            latency_ms.push((p.at_micros as f64 - stamp.captured_micros as f64) / 1000.0);
        }
        latency_ms.sort_by(f64::total_cmp);
        Measured {
            pictures: window.len(),
            unstamped: window.iter().filter(|p| p.stamp.is_none()).count(),
            per_second,
            longest_gap_ms: std::iter::once(from_micros)
                .chain(arrivals)
                .chain(std::iter::once(to_micros))
                .collect::<Vec<u64>>()
                .windows(2)
                .map(|w| (w[1] - w[0]) as f64 / 1000.0)
                .fold(0.0, f64::max),
            latency_ms,
            sizes: histogram(window.iter().map(|p| p.size)),
        }
    }

    fn worst_fps(&self) -> Option<u32> {
        self.per_second.iter().copied().min()
    }

    fn mean_fps(&self) -> f64 {
        self.per_second.iter().sum::<u32>() as f64 / self.per_second.len().max(1) as f64
    }

    fn viewer(&self) -> Value {
        let at = |p| percentile(&self.latency_ms, p).map(|v| round(v, 1));
        json!({
            "pictures": self.pictures,
            "pictures_without_a_readable_stamp": self.unstamped,
            "unique_pictures_per_second": {
                "worst": self.worst_fps(),
                "mean": round(self.mean_fps(), 1),
                "each_second": self.per_second,
            },
            "longest_gap_ms": round(self.longest_gap_ms, 1),
            "latency_ms": {
                "p50": at(50.0),
                "p95": at(95.0),
                "p99": at(99.0),
                "max": self.latency_ms.last().map(|v| round(*v, 1)),
            },
            "picture_sizes": self.sizes,
        })
    }

    fn summary(&self, args: &Args, sent: &[Second]) -> String {
        let at = |p| percentile(&self.latency_ms, p).unwrap_or(f64::NAN);
        let kbps = |bps: u64| bps / 1000;
        let wire = |end: fn(&Wire) -> u64| {
            let mean =
                sent.iter().map(|s| end(&s.wire_bps)).sum::<u64>() / sent.len().max(1) as u64;
            let max = sent.iter().map(|s| end(&s.wire_bps)).max().unwrap_or(0);
            format!("{}/{}", kbps(mean), kbps(max))
        };
        format!(
            "{} {} {:?} for {} s after {} s warm-up\n\
             viewer: {} pictures/s worst, {:.1} mean; longest gap {:.0} ms; sizes {:?}\n\
             latency ms: p50 {:.0}, p95 {:.0}, p99 {:.0}, max {:.0}\n\
             sharer: {} kbps mean, {} kbps max\n\
             udp kbps mean/max: alice {}, sfu {}, bob {}",
            size_name(args.size),
            args.workload.name(),
            args.quality,
            args.seconds,
            args.warmup,
            self.worst_fps().unwrap_or(0),
            self.mean_fps(),
            self.longest_gap_ms,
            self.sizes,
            at(50.0),
            at(95.0),
            at(99.0),
            self.latency_ms.last().copied().unwrap_or(f64::NAN),
            kbps(sent.iter().map(|s| s.bps).sum::<u64>() / sent.len().max(1) as u64),
            kbps(sent.iter().map(|s| s.bps).max().unwrap_or(0)),
            wire(|w| w.alice),
            wire(|w| w.sfu),
            wire(|w| w.bob),
        )
    }
}

/// `psnr` holds each compared picture's PSNR, and that of its busy region if it has one.
fn psnr_report(args: &Args, psnr: Option<&[(f64, Option<f64>)]>, measured: &Measured) -> Value {
    let Some(each) = psnr else {
        return json!({ "skipped": if args.workload.scrolls() {
            "switched off by --psnr-every 0"
        } else {
            "only the scroll workloads have a source picture to regenerate"
        } });
    };
    if each.is_empty() {
        let seen: Vec<&String> = measured.sizes.keys().collect();
        return json!({
            "skipped": format!(
                "the Viewer's pictures ({seen:?}) are not the source's {}",
                size_name(args.size)
            ),
        });
    }
    // An identical picture is infinitely close; a mean of it would be no number.
    let finite =
        |values: &[f64]| -> Vec<f64> { values.iter().copied().filter(|v| v.is_finite()).collect() };
    let mean = |values: &[f64]| {
        let finite = finite(values);
        round(finite.iter().sum::<f64>() / finite.len().max(1) as f64, 2)
    };
    let min = |values: &[f64]| values.iter().copied().reduce(f64::min).map(|v| round(v, 2));
    let full: Vec<f64> = each.iter().map(|p| p.0).collect();
    let busy: Vec<f64> = each.iter().filter_map(|p| p.1).collect();
    let mut report = json!({
        "pictures_compared": full.len(),
        "pictures_identical": full.len() - finite(&full).len(),
        "mean_db": mean(&full),
        "min_db": min(&full),
    });
    if !busy.is_empty() {
        report["busy_region_mean_db"] = mean(&busy).into();
        report["busy_region_min_db"] = min(&busy).into();
    }
    report
}

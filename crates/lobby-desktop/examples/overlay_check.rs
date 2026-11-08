//! The Sharer's ink overlay on the real screen. It captures the primary monitor with Windows
//! Graphics Capture as a share does, and checks three things:
//!
//! - A monitor share: a magenta marker window near the right edge, the overlay over the monitor
//!   with ink across the marker and beside it. Left out of captures, the picture shows the marker
//!   whole with no ink and no black patch; let back in, it shows the ink. A click on the ink
//!   lands on the marker.
//! - A watched share's picture: the overlay paints a stroke over a spot of the marker, as it
//!   paints over the picture in a Lobby window on the shared monitor. The ink must land on the
//!   spot's pixels.
//! - A window share: a second process opens a magenta window; the overlay covers it, sits just
//!   above it and follows it as it moves, changes size, is minimised and restored, and is
//!   raised. Left out of captures, neither the monitor's nor the window's own picture shows ink.
//!
//! The focus must never move. The pictures hold whatever else is on the screen: keep them local.
//! It shows windows on the person's screen for several seconds and never takes the focus, nor
//! moves the pointer. Run it only with the person's leave:
//!
//! ```sh
//! cargo run -p lobby-desktop --example overlay_check -- <out dir>
//! ```

use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use gpui_kit::{
    AppContext as _, AsyncApp, Bounds, IntoElement, Render, Styled as _, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point,
    px, rgb, size,
};
use lobby_client::{
    SourceId,
    ink::{InkBoard, point_at},
    list_sources,
    media::{grab_monitor, grab_window},
};
use lobby_desktop::{
    assets::LobbyAssets,
    components::{
        ink::{InkLayer, ink_authors},
        ink_overlay::{InkOverlay, OverlayPane, PaneAt},
    },
    host::{DriveWindow, NativeWindow, OverlayTarget},
};
use lobby_protocol::{DrawingEpoch, ShareId, StrokeId, UserId};

const SETTLE: Duration = Duration::from_millis(700);
const GRAB_TIMEOUT: Duration = Duration::from_secs(5);
const MAGENTA: [u8; 3] = [255, 0, 255];
/// Past a window's edge, where Windows draws its border and rounds its corners.
const EDGE: i32 = 14;
/// How far an edge of the ink may lie from where it should: half a stroke and a dot's spill.
const INK_SLACK: i32 = 14;

static PASSED: AtomicBool = AtomicBool::new(false);

struct Marker;

impl Render for Marker {
    fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0xff00ff))
    }
}

/// x, y, width and height in physical pixels of the virtual screen.
type Rect = (i32, i32, i32, i32);

/// A picture of the monitor or a window, BGRA rows.
struct Shot {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

impl Shot {
    fn rgb(&self, x: i32, y: i32) -> [u8; 3] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]]
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut rgba = self.bgra.clone();
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        image::RgbaImage::from_raw(self.width, self.height, rgba)
            .context("picture size")?
            .save(path)?;
        Ok(())
    }

    fn black(&self) -> usize {
        self.bgra
            .chunks_exact(4)
            .filter(|px| px[..3] == [0, 0, 0])
            .count()
    }

    /// The pixels inside `rect` that are not magenta, and the box around them.
    fn ink_in(&self, (x, y, w, h): Rect) -> (usize, Option<Rect>) {
        let (mut count, mut bounds) = (0, None::<(i32, i32, i32, i32)>);
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = (
            (x + w).min(self.width as i32),
            (y + h).min(self.height as i32),
        );
        for py in y0..y1 {
            for px in x0..x1 {
                if near(self.rgb(px, py), MAGENTA, 40) {
                    continue;
                }
                count += 1;
                bounds = Some(match bounds {
                    None => (px, py, px, py),
                    Some((l, t, r, b)) => (l.min(px), t.min(py), r.max(px), b.max(py)),
                });
            }
        }
        (
            count,
            bounds.map(|(l, t, r, b)| (l, t, r - l + 1, b - t + 1)),
        )
    }
}

fn near(a: [u8; 3], b: [u8; 3], by: u8) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= by)
}

fn inset((x, y, w, h): Rect, by: i32) -> Rect {
    (x + by, y + by, w - 2 * by, h - 2 * by)
}

/// The part of `rect` from fraction `from` to `to` of its size, both ways.
fn part((x, y, w, h): Rect, from: f32, to: f32) -> Rect {
    let at = |o: i32, len: i32, f: f32| o + (len as f32 * f).round() as i32;
    let (l, t) = (at(x, w, from), at(y, h, from));
    (l, t, at(x, w, to) - l, at(y, h, to) - t)
}

/// Whether the ink's box has its edges where the stroke runs.
fn ink_on(found: Option<Rect>, stroke: Rect) -> bool {
    let Some((x, y, w, h)) = found else {
        return false;
    };
    let (sx, sy, sw, sh) = stroke;
    [(x, sx), (y, sy), (x + w, sx + sw), (y + h, sy + sh)]
        .iter()
        .all(|(a, b)| a.abs_diff(*b) as i32 <= INK_SLACK)
}

struct Report {
    lines: Vec<String>,
    passed: bool,
}

impl Report {
    fn check(&mut self, ok: bool, line: String) {
        self.passed &= ok;
        let line = format!("{} {line}", if ok { "ok  " } else { "FAIL" });
        println!("{line}");
        self.lines.push(line);
    }

    fn note(&mut self, line: String) {
        println!("     {line}");
        self.lines.push(format!("     {line}"));
    }
}

async fn grab(hmonitor: isize, cx: &AsyncApp) -> anyhow::Result<Shot> {
    let (width, height, bgra) = cx
        .background_executor()
        .spawn(async move { grab_monitor(hmonitor, GRAB_TIMEOUT) })
        .await?;
    Ok(Shot {
        width,
        height,
        bgra,
    })
}

async fn grab_of(hwnd: isize, cx: &AsyncApp) -> anyhow::Result<Shot> {
    let (width, height, bgra) = cx
        .background_executor()
        .spawn(async move { grab_window(hwnd, GRAB_TIMEOUT) })
        .await?;
    Ok(Shot {
        width,
        height,
        bgra,
    })
}

async fn settle(cx: &AsyncApp) {
    cx.background_executor().timer(SETTLE).await;
}

fn user(n: u8) -> UserId {
    format!("usr_{n:024x}")
        .parse()
        .expect("a well-formed user id")
}

/// A zigzag from `from` to `to` in fractions of the monitor.
fn zigzag(from: (f32, f32), to: (f32, f32), teeth: usize, depth: f32) -> Vec<(f32, f32)> {
    (0..=teeth * 2)
        .map(|i| {
            let t = i as f32 / (teeth * 2) as f32;
            let lift = if i % 2 == 0 { -depth } else { depth };
            (
                from.0 + (to.0 - from.0) * t,
                from.1 + (to.1 - from.1) * t + lift,
            )
        })
        .collect()
}

/// A closed square stroke from fraction `from` to `to` of a pane, both ways.
fn square(from: f32, to: f32) -> Vec<(f32, f32)> {
    vec![(from, from), (to, from), (to, to), (from, to), (from, from)]
}

/// One pane whose layer has bob's `stroke` on it, lifted.
fn pane(stroke: &[(f32, f32)], at: PaneAt) -> OverlayPane {
    let mut board = InkBoard::new(ShareId::new(1).expect("not zero"), DrawingEpoch(1));
    let now = Instant::now();
    let bob = user(2);
    for (x, y) in stroke {
        board.echo(&bob, StrokeId(1), point_at(*x, *y), now);
    }
    board.lift(&bob, StrokeId(1), now);
    OverlayPane {
        layer: InkLayer {
            board: Some(board),
            authors: ink_authors([(&bob, "bob")]),
            me: Some(user(1)),
            pen: None,
            at: Some(now),
            spot: None,
        },
        at,
    }
}

/// A pop-up shows at once; a normal window, which a share can capture, opens hidden and is shown
/// without activating it ([`DriveWindow::Restore`]).
fn open_marker(
    bounds: Bounds<gpui_kit::Pixels>,
    kind: WindowKind,
    cx: &mut gpui_kit::App,
) -> WindowHandle<Marker> {
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Lobby overlay check".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            focus: false,
            show: kind == WindowKind::PopUp,
            kind,
            is_movable: false,
            is_resizable: true,
            is_minimizable: true,
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        },
        |_, cx| cx.new(|_| Marker),
    )
    .expect("the marker window opens")
}

async fn run(out: PathBuf, cx: &mut AsyncApp) -> anyhow::Result<()> {
    std::fs::create_dir_all(&out)?;
    let hmonitor = list_sources(false)?
        .into_iter()
        .find_map(|s| match s.id {
            SourceId::Monitor { hmonitor } if s.detail.contains("Primary") => Some(hmonitor),
            _ => None,
        })
        .context("no primary monitor")?;
    let focus_before = NativeWindow::foreground();
    let mut report = Report {
        lines: Vec::new(),
        passed: true,
    };

    // GPUI quits once its last window closes: the first part's stay open until the end.
    let (marker, overlay) = monitor_share(hmonitor, &out, &mut report, cx).await?;
    window_share(hmonitor, &out, &mut report, cx).await?;
    overlay.update(cx, |_, window, _| window.remove_window())?;
    marker.update(cx, |_, window, _| window.remove_window())?;

    report.check(
        focus_before == NativeWindow::foreground(),
        "the focus never moved".into(),
    );
    std::fs::write(out.join("report.txt"), report.lines.join("\n"))?;
    PASSED.store(report.passed, Ordering::SeqCst);
    // A quit in the same turn as the windows' removal leaves GPUI's event loop running.
    settle(cx).await;
    Ok(())
}

async fn monitor_share(
    hmonitor: isize,
    out: &Path,
    report: &mut Report,
    cx: &mut AsyncApp,
) -> anyhow::Result<(WindowHandle<Marker>, WindowHandle<InkOverlay>)> {
    let display = cx
        .update(|cx| cx.primary_display().map(|d| d.bounds()))
        .context("no display")?;
    let marker_bounds = Bounds::new(
        point(display.right() - px(480.), display.origin.y + px(180.)),
        size(px(400.), px(260.)),
    );
    let marker = cx.update(|cx| open_marker(marker_bounds, WindowKind::PopUp, cx));
    let (marker_native, marker_size, scale) = marker.update(cx, |_, window, _| {
        (
            NativeWindow::of(window),
            window.viewport_size(),
            window.scale_factor(),
        )
    })?;
    let marker_native = marker_native.context("the marker has no native window")?;
    settle(cx).await;
    let before = grab(hmonitor, cx).await?;

    let overlay = cx.update(|cx| InkOverlay::open(OverlayTarget::Monitor(hmonitor), cx))?;
    let native = InkOverlay::make(overlay, cx)
        .await
        .context("making the overlay")?;
    let (ox, oy) = native.client_origin().context("overlay origin")?;
    let (mx, my) = marker_native.client_origin().context("marker origin")?;
    let (mw, mh) = (
        (f32::from(marker_size.width) * scale) as i32,
        (f32::from(marker_size.height) * scale) as i32,
    );
    let (sw, sh) = (before.width as f32, before.height as f32);
    let frac = |x: i32, y: i32| ((x - ox) as f32 / sw, (y - oy) as f32 / sh);
    // Across the marker's middle and on past its left edge onto whatever is on the screen.
    let (left, mid) = frac(mx - mw, my + mh / 2);
    let (right, _) = frac(mx + mw - mw / 8, my);
    let (_, depth_px) = frac(0, oy + mh / 5);
    let stroke = zigzag((left, mid), (right, mid), 4, depth_px);
    overlay.update(cx, |o, _, cx| {
        o.set_panes(vec![pane(&stroke, PaneAt::Whole)], cx)
    })?;
    settle(cx).await;
    let overlay_size = overlay.update(cx, |_, window, _| {
        let (size, scale) = (window.viewport_size(), window.scale_factor());
        (
            (f32::from(size.width) * scale).round() as u32,
            (f32::from(size.height) * scale).round() as u32,
        )
    })?;
    // The primary monitor's top left is the screen's origin.
    report.check(
        (ox, oy) == (0, 0) && overlay_size == (before.width, before.height),
        format!(
            "monitor: the overlay ({}x{} at {ox},{oy}) covers the monitor ({}x{})",
            overlay_size.0, overlay_size.1, before.width, before.height
        ),
    );

    // Where a click on the ink over the marker would land.
    let on_ink = stroke
        .iter()
        .map(|(x, y)| (ox + (x * sw) as i32, oy + (y * sh) as i32))
        .find(|(x, y)| (mx..mx + mw).contains(x) && (my..my + mh).contains(y))
        .context("no ink over the marker")?;
    let landed = NativeWindow::at(on_ink.0, on_ink.1);
    let excluded = grab(hmonitor, cx).await?;
    if !native.exclude_from_capture(false) {
        bail!("Windows did not let the overlay back into captures");
    }
    settle(cx).await;
    let included = grab(hmonitor, cx).await?;
    before.save(&out.join("0-marker-only.png"))?;
    excluded.save(&out.join("1-overlay-excluded.png"))?;
    included.save(&out.join("2-overlay-included.png"))?;

    // The marker's pixels that are not as they were before the overlay opened.
    let inside = |shot: &Shot| {
        let (x0, y0) = (mx - ox, my - oy);
        let mut off = 0;
        for y in y0..y0 + mh {
            for x in x0..x0 + mw {
                if !near(shot.rgb(x, y), before.rgb(x, y), 2) {
                    off += 1;
                }
            }
        }
        off
    };
    let (off_excluded, off_included) = (inside(&excluded), inside(&included));
    let ink_points: Vec<(i32, i32)> = stroke
        .iter()
        .map(|(x, y)| ((x * sw) as i32, (y * sh) as i32))
        .collect();
    let changed = |a: &Shot, b: &Shot| {
        ink_points
            .iter()
            .filter(|(x, y)| !near(a.rgb(*x, *y), b.rgb(*x, *y), 24))
            .count()
    };
    report.note(format!("marker {mw}x{mh} at {mx},{my}"));
    report.check(
        off_excluded == 0 && off_included > 0,
        format!(
            "monitor: marker pixels the ink changed: left out {off_excluded}, let in \
             {off_included}"
        ),
    );
    let black_grew = excluded.black() > before.black() + before.black() / 100 + 16;
    report.check(
        !black_grew,
        format!(
            "monitor: black pixels: before {}, left out {}, let in {}",
            before.black(),
            excluded.black(),
            included.black()
        ),
    );
    report.check(
        changed(&before, &included) > ink_points.len() / 2,
        format!(
            "monitor: ink points that changed: left out {} of {n}, let in {} of {n}",
            changed(&before, &excluded),
            changed(&before, &included),
            n = ink_points.len()
        ),
    );
    report.check(
        landed == Some(marker_native),
        format!(
            "monitor: a click on the ink at {on_ink:?} lands on {}",
            match landed {
                Some(w) if w == marker_native => "the marker",
                Some(w) if w == native => "the overlay",
                Some(_) => "another window",
                None => "nothing",
            }
        ),
    );

    // The marker stands in for a Lobby window with a watched share's canvas at `spot`.
    let spot = Bounds::new(point(px(40.), px(30.)), size(px(240.), px(150.)));
    let picture = PaneAt::Picture {
        window: marker_native,
        spot: Rc::new(std::cell::Cell::new(Some(spot))),
    };
    overlay.update(cx, |o, _, cx| {
        o.set_panes(vec![pane(&square(0.1, 0.9), picture)], cx)
    })?;
    settle(cx).await;
    let on_picture = grab(hmonitor, cx).await?;
    on_picture.save(&out.join("3-picture-included.png"))?;
    let _ = native.exclude_from_capture(true);
    let spot_px = (
        mx + (f32::from(spot.origin.x) * scale).round() as i32,
        my + (f32::from(spot.origin.y) * scale).round() as i32,
        (f32::from(spot.size.width) * scale).round() as i32,
        (f32::from(spot.size.height) * scale).round() as i32,
    );
    let (count, found) = on_picture.ink_in(inset((mx, my, mw, mh), EDGE));
    report.check(
        ink_on(found, part(spot_px, 0.1, 0.9)),
        format!(
            "picture: {count} ink pixels in a box {found:?}; the stroke runs at {:?}",
            part(spot_px, 0.1, 0.9)
        ),
    );

    overlay.update(cx, |o, _, cx| o.set_panes(Vec::new(), cx))?;
    Ok((marker, overlay))
}

/// The other process, with its magenta window: its `HWND` on its first line out, gone when its
/// input closes.
struct Shared {
    child: Child,
    window: NativeWindow,
}

impl Shared {
    fn open() -> anyhow::Result<Shared> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--marker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take().context("the marker's output")?).read_line(&mut line)?;
        let hwnd: isize = line.trim().parse().context("the marker's HWND")?;
        Ok(Shared {
            child,
            window: NativeWindow::from_raw(hwnd),
        })
    }

    fn frame(&self) -> Option<Rect> {
        Some(self.window.state()?.frame)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let until = Instant::now() + GRAB_TIMEOUT;
        while Instant::now() < until {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
    }
}

async fn window_share(
    hmonitor: isize,
    out: &Path,
    report: &mut Report,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let screen = grab(hmonitor, cx).await?;
    let (w, h) = (screen.width as f32, screen.height as f32);
    let at = |x: f32, y: f32, fw: f32, fh: f32| {
        DriveWindow::Place(
            (w * x) as i32,
            (h * y) as i32,
            (w * fw) as i32,
            (h * fh) as i32,
        )
    };
    let shared = Shared::open()?;
    let window = shared.window;
    window.drive(DriveWindow::Restore);
    window.drive(at(0.3, 0.15, 0.3, 0.3));
    settle(cx).await;

    let overlay = cx.update(|cx| InkOverlay::open(OverlayTarget::Window(window.raw()), cx))?;
    let native = InkOverlay::make(overlay, cx)
        .await
        .context("making the window's overlay")?;
    overlay.update(cx, |o, _, cx| {
        o.set_panes(vec![pane(&square(0.1, 0.9), PaneAt::Whole)], cx)
    })?;
    settle(cx).await;

    let frame = shared.frame().context("the shared window's frame")?;
    let own = grab_of(window.raw(), cx).await?;
    own.save(&out.join("4-window-own-capture.png"))?;
    let (own_ink, _) = own.ink_in(inset((0, 0, own.width as i32, own.height as i32), EDGE));
    report.check(
        (own.width as i32, own.height as i32) == (frame.2, frame.3) && own_ink == 0,
        format!(
            "window: its own capture is {}x{} as its frame {frame:?}, with {own_ink} ink pixels",
            own.width, own.height
        ),
    );
    let excluded = grab(hmonitor, cx).await?;
    excluded.save(&out.join("5-window-excluded.png"))?;
    let (excluded_ink, _) = excluded.ink_in(inset(frame, EDGE));
    report.check(
        excluded_ink == 0,
        format!("window: left out, the monitor's capture has {excluded_ink} ink pixels on it"),
    );
    if !native.exclude_from_capture(false) {
        bail!("Windows did not let the window's overlay back into captures");
    }

    let steps = [
        ("placed", None),
        ("moved", Some(at(0.15, 0.45, 0.3, 0.3))),
        ("resized", Some(at(0.15, 0.45, 0.42, 0.38))),
        ("minimised", Some(DriveWindow::Minimise)),
        ("restored", Some(DriveWindow::Restore)),
        ("raised", Some(DriveWindow::Raise)),
    ];
    for (n, (name, drive)) in steps.into_iter().enumerate() {
        if let Some(drive) = drive {
            window.drive(drive);
            settle(cx).await;
        }
        let shot = grab(hmonitor, cx).await?;
        shot.save(&out.join(format!("{}-window-{name}.png", 6 + n)))?;
        let target = window.state().context("the shared window")?;
        let over = native.state().context("the overlay")?;
        if name == "minimised" {
            report.check(
                !over.shown,
                format!("window {name}: the overlay is hidden ({over:?})"),
            );
            continue;
        }
        let (_, found) = shot.ink_in(inset(target.frame, EDGE));
        report.check(
            over.shown
                && over.frame == target.frame
                && target.above == Some(native.raw())
                && !over.topmost,
            format!(
                "window {name}: the overlay {:?} covers the window {:?}, just above it ({:?}), \
                 not topmost",
                over.frame, target.frame, target.above
            ),
        );
        report.check(
            ink_on(found, part(target.frame, 0.1, 0.9)),
            format!(
                "window {name}: ink in a box {found:?}; the stroke runs at {:?}",
                part(target.frame, 0.1, 0.9)
            ),
        );
    }
    let _ = native.exclude_from_capture(true);
    overlay.update(cx, |_, window, _| window.remove_window())?;
    drop(shared);
    Ok(())
}

/// The second process: one magenta window, never focused, hidden until the check shows it.
fn marker_process() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::new(point(px(200.), px(200.)), size(px(320.), px(200.)));
        let marker = open_marker(bounds, WindowKind::Normal, cx);
        let hwnd = marker
            .update(cx, |_, window, _| {
                NativeWindow::of(window).map(NativeWindow::raw)
            })
            .ok()
            .flatten()
            .unwrap_or(0);
        println!("{hwnd}");
        let _ = std::io::stdout().flush();
        std::thread::spawn(|| {
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            std::process::exit(0);
        });
    });
}

fn main() -> anyhow::Result<()> {
    let first = std::env::args().nth(1);
    if first.as_deref() == Some("--marker") {
        marker_process();
        return Ok(());
    }
    let out = first.map_or_else(|| PathBuf::from("target/verify/overlay"), PathBuf::from);
    gpui_kit::application()
        .with_assets(LobbyAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            lobby_desktop::fonts::install(cx);
            cx.spawn(async move |cx| {
                if let Err(e) = run(out, cx).await {
                    // Its windows go with the process.
                    eprintln!("overlay check: {e:#}");
                    std::process::exit(1);
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    if PASSED.load(Ordering::SeqCst) {
        println!("overlay check: pass");
        Ok(())
    } else {
        bail!("overlay check: FAIL")
    }
}

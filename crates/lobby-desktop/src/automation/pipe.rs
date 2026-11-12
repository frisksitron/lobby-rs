//! The automation channel: with the `automation` feature and `LOBBY_AUTOMATION=1`, the app listens
//! on `\\.\pipe\lobby-automation-<LOBBY_INSTANCE_ID>` for scenario steps, one JSON object per line,
//! and answers each with one line. `cargo xtask drive` speaks it to the real binaries, so a
//! scenario runs against DirectX and DirectWrite without SendInput, focus changes or the mouse.
//! Input is synthetic GPUI input and pictures come from `Window::render_to_image`.
//!
//! Every observation draws a fresh frame first. `render_to_image` rasterizes the last frame drawn,
//! so an off-screen window that has not redrawn would show a picture or bounds older than the model
//! a `wait` just read. A shot also takes the pointer out of the window (unless `"hover": true`) and
//! is taken once two frames [`SETTLE_FRAME`] apart agree, or after [`SETTLE_WITHIN`] for what never
//! holds still (a spinner, live video). A click waits the same way for its target to hold still.
//!
//! Requests are the `scenario` steps that act on one window, plus `{"hello": true}`. `shot` and
//! `dump` take a `"path"` to write to. Answers are `{"ok": true, "ms": ...}` with `dump` or `said`
//! where there is something to say, or `{"ok": false, "error": "..."}`.
//!
//! The pipe is served on the app's Tokio runtime. Each request crosses to the UI over a channel and
//! is done there, so nothing reactor-bound runs on the GPUI side. [`Channel`] owns both tasks.

use std::time::{Duration, Instant};

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Entity, Global, test::TestWindowExt as _,
};
use image::RgbaImage;
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::{mpsc, oneshot},
};
use tokio_util::task::AbortOnDropHandle;
use tracing::{info, warn};

use super::{
    StepError, WindowStep, crop, downscale, dump, element_bounds, perform, resize_target,
    rest_pointer,
    scenario::{self, Step},
    script_host,
};
use crate::{TokioRuntime, host::NativeWindow, shell::Shell};

const POLL: Duration = Duration::from_millis(50);
const SETTLE_FRAME: Duration = Duration::from_millis(50);
const SETTLE_WITHIN: Duration = Duration::from_secs(1);
/// Longer than a dialog's exit transition (180 ms, set in `appearance::apply`).
const COVERED_WITHIN: Duration = Duration::from_secs(2);

pub fn pipe_name(instance: Option<&str>) -> String {
    format!(
        r"\\.\pipe\lobby-automation-{}",
        instance.unwrap_or("default")
    )
}

type Request = (String, oneshot::Sender<String>);

pub struct Channel {
    _serve: AbortOnDropHandle<()>,
    _steps: gpui_kit::Task<()>,
}

impl Global for Channel {}

pub fn start(
    instance: Option<&str>,
    park: bool,
    shell: Entity<Shell>,
    window: AnyWindowHandle,
    cx: &mut App,
) -> Channel {
    let name = pipe_name(instance);
    let (tx, mut rx) = mpsc::channel::<Request>(8);
    info!(pipe = %name, "automation channel listening");
    let serve = cx.global::<TokioRuntime>().handle().spawn(serve(name, tx));
    let instance = instance.map(str::to_owned);
    let steps = cx.spawn(async move |cx| {
        // Here, not in the update that opened the window: the move sends window messages that GPUI
        // must take while the app is not borrowed.
        if park {
            let native = cx
                .update_window(window, |_, window, _| NativeWindow::of(window))
                .ok()
                .flatten();
            if !native.is_some_and(NativeWindow::park_offscreen) {
                warn!("the driven window could not be placed off screen");
            }
        }
        let mut marks = Map::new();
        while let Some((line, reply)) = rx.recv().await {
            let started = Instant::now();
            let mut answer = match handle(&line, &instance, &shell, window, &mut marks, cx).await {
                Ok(mut ok) => {
                    ok["ok"] = Value::Bool(true);
                    ok
                }
                Err(e) => json!({"ok": false, "error": e}),
            };
            answer["ms"] = json!(started.elapsed().as_millis() as u64);
            let _ = reply.send(answer.to_string());
        }
    });
    Channel {
        _serve: AbortOnDropHandle::new(serve),
        _steps: steps,
    }
}

/// The next instance is created before a connection is served, so the name never lapses for another
/// process to take. Only the first may create it, so a clash fails loudly.
async fn serve(name: String, tx: mpsc::Sender<Request>) {
    let create = |first: bool| {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(&name)
    };
    let mut server = match create(true) {
        Ok(server) => server,
        Err(e) => {
            warn!(pipe = %name, error = %e, "the automation pipe cannot be created");
            return;
        }
    };
    loop {
        let connected = server.connect().await;
        let current = server;
        server = match create(false) {
            Ok(next) => next,
            Err(e) => {
                warn!(pipe = %name, error = %e, "the automation pipe cannot be created again");
                return;
            }
        };
        if let Err(e) = connected {
            warn!(error = %e, "an automation driver failed to connect");
            continue;
        }
        info!("an automation driver connected");
        if !connection(current, &tx).await {
            return;
        }
    }
}

async fn connection(pipe: NamedPipeServer, tx: &mpsc::Sender<Request>) -> bool {
    let (read, mut write) = tokio::io::split(pipe);
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let (reply, answer) = oneshot::channel();
        if tx.send((line, reply)).await.is_err() {
            return false;
        }
        let Ok(answer) = answer.await else {
            return false;
        };
        let sent = async {
            write.write_all(answer.as_bytes()).await?;
            write.write_all(b"\n").await?;
            write.flush().await
        };
        if sent.await.is_err() {
            break;
        }
    }
    info!("the automation driver left");
    true
}

async fn handle(
    line: &str,
    instance: &Option<String>,
    shell: &Entity<Shell>,
    window: AnyWindowHandle,
    marks: &mut Map<String, Value>,
    cx: &mut AsyncApp,
) -> Result<Value, String> {
    let mut value: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let obj = value.as_object_mut().ok_or("a request is a JSON object")?;
    if obj.contains_key("hello") {
        return Ok(json!({"instance": instance, "pid": std::process::id()}));
    }
    let path = match obj.remove("path") {
        Some(Value::String(p)) => Some(p),
        Some(_) => return Err("path is a string".into()),
        None => None,
    };
    let step = scenario::parse_step_value(value).map_err(|e| e.message)?;
    let window_step = async |step: WindowStep, cx: &mut AsyncApp| -> Result<Value, String> {
        // A person waits for a dialog to finish leaving before they click what it covered.
        let until = Instant::now() + COVERED_WITHIN;
        loop {
            let done = cx
                .update_window(window, |_, window, cx| {
                    let done = perform(shell, &step, window, cx);
                    // The steps draw frames of their own; the platform draws and presents the next.
                    window.refresh();
                    done
                })
                .map_err(|e| e.to_string())?;
            match done {
                Err(StepError::Covered(_)) if Instant::now() < until => {
                    cx.background_executor().timer(POLL).await;
                }
                done => return done.map(|()| json!({})).map_err(|e| e.to_string()),
            }
        }
    };
    match step {
        Step::Action(name) if resize_target(&name).is_some() => resize(&name, window, cx),
        Step::Click(id) => {
            until_still(&id, window, cx).await?;
            window_step(WindowStep::Click(id), cx).await
        }
        Step::MoveTo(id) => window_step(WindowStep::MoveTo(id), cx).await,
        Step::RightClick(id) => {
            until_still(&id, window, cx).await?;
            window_step(WindowStep::RightClick(id), cx).await
        }
        Step::Type { text, into } => window_step(WindowStep::Type { text, into }, cx).await,
        Step::Press(key) => window_step(WindowStep::Press(key), cx).await,
        Step::Scroll { list, to } => window_step(WindowStep::Scroll { list, to }, cx).await,
        Step::Wheel { id, notches } => window_step(WindowStep::Wheel { id, notches }, cx).await,
        Step::Action(name) => window_step(WindowStep::Action(name), cx).await,
        Step::Drag { id, by } => window_step(WindowStep::Drag { id, by }, cx).await,
        Step::Pen(pen) => window_step(WindowStep::Pen(pen), cx).await,
        Step::Expect(pred) => {
            let now = snapshot(shell, window, marks, cx)?;
            if scenario::holds(&pred, &now) {
                Ok(json!({}))
            } else {
                Err(scenario::explain(&pred, &now))
            }
        }
        Step::Wait { pred, timeout } => {
            let until = Instant::now() + timeout;
            loop {
                let now = snapshot(shell, window, marks, cx)?;
                if scenario::holds(&pred, &now) {
                    return Ok(json!({}));
                }
                if Instant::now() >= until {
                    return Err(format!(
                        "not within {timeout:?}: {}",
                        scenario::explain(&pred, &now)
                    ));
                }
                cx.background_executor().timer(POLL).await;
            }
        }
        Step::Dump(_) => {
            let now = snapshot(shell, window, marks, cx)?;
            if let Some(path) = &path {
                std::fs::write(
                    path,
                    serde_json::to_string_pretty(&now).expect("a JSON Value serializes"),
                )
                .map_err(|e| format!("cannot write {path}: {e}"))?;
            }
            Ok(json!({"dump": now}))
        }
        Step::Mark(name) => {
            let mut now = snapshot(shell, window, marks, cx)?;
            now.as_object_mut()
                .expect("the dump is an object")
                .remove("marks");
            marks.insert(name, now);
            Ok(json!({}))
        }
        Step::Shot { id, hover, .. } => {
            let path = path.ok_or("shot needs a path")?;
            if !hover {
                cx.update_window(window, |_, window, cx| rest_pointer(window, cx))
                    .map_err(|e| e.to_string())?;
            }
            let until = Instant::now() + SETTLE_WITHIN;
            let mut last = capture(window, id.as_deref(), cx)?;
            let settled = loop {
                if Instant::now() >= until {
                    break false;
                }
                cx.background_executor().timer(SETTLE_FRAME).await;
                let next = capture(window, id.as_deref(), cx)?;
                if next == last {
                    break true;
                }
                last = next;
            };
            last.save(&path)
                .map_err(|e| format!("cannot write {path}: {e}"))?;
            Ok(json!({"said": path, "settled": settled}))
        }
        Step::Note(_) => Ok(json!({})),
        Step::Server(_) | Step::Open(_) | Step::Close(_) | Step::Corrupt(_) => {
            Err("the driver does this step, not the app".into())
        }
        Step::Network(_) | Step::Advance(_) => {
            Err("Layer 3 only: the driver has no proxy and the app's clock is real".into())
        }
        Step::Host { picks, save_dir } => cx
            .update(|cx| {
                let picks = picks
                    .iter()
                    .map(|list| list.iter().map(|p| scenario::fixture(p)).collect())
                    .collect();
                script_host(picks, save_dir.as_deref().map(scenario::fixture), cx)
            })
            .map(|()| json!({})),
        Step::Drop { id, paths } => {
            window_step(
                WindowStep::Drop {
                    id,
                    paths: paths.iter().map(|p| scenario::fixture(p)).collect(),
                },
                cx,
            )
            .await
        }
        Step::Paste { .. } => Err(
            "paste is Layer 3 only: it would write the person's own clipboard, and the app has \
             no clipboard of its own to paste from"
                .into(),
        ),
    }
}

/// A person clicks what holds still. A target still sliding in (the members sheet, a toast) moves
/// between the press and the release, and its middle can still be outside the window.
async fn until_still(id: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<(), String> {
    let bounds = |cx: &mut AsyncApp| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            element_bounds(window, id)
        })
        .map_err(|e| e.to_string())
    };
    let until = Instant::now() + SETTLE_WITHIN;
    let mut last = bounds(cx)?;
    while last.is_some() && Instant::now() < until {
        cx.background_executor().timer(SETTLE_FRAME).await;
        let next = bounds(cx)?;
        if next == last {
            break;
        }
        last = next;
    }
    Ok(())
}

fn capture(
    window: AnyWindowHandle,
    id: Option<&str>,
    cx: &mut AsyncApp,
) -> Result<RgbaImage, String> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let scale = window.scale_factor();
        let frame = window
            .render_to_image()
            .map_err(|e| format!("render_to_image: {e}"))?;
        match id {
            Some(id) => element_bounds(window, id)
                .map(|b| crop(&frame, b, scale))
                .ok_or_else(|| format!("{id:?} is not drawn")),
            None => Ok(downscale(&frame, scale)),
        }
    })
    .map_err(|e| e.to_string())?
}

/// Done from this task, not inside an update, so the `WM_SIZE` it sends synchronously reaches GPUI
/// while the app is not borrowed.
fn resize(name: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<Value, String> {
    let size = resize_target(name).ok_or_else(|| format!("{name:?} is not a size"))?;
    let (native, scale) = cx
        .update_window(window, |_, window, _| {
            (NativeWindow::of(window), window.scale_factor())
        })
        .map_err(|e| e.to_string())?;
    let native = native.ok_or("this window cannot be resized")?;
    let px = |v: gpui_kit::Pixels| (f32::from(v) * scale).round() as i32;
    if !native.resize_client(px(size.width), px(size.height)) {
        return Err("the window could not be resized".into());
    }
    Ok(json!({}))
}

fn snapshot(
    shell: &Entity<Shell>,
    window: AnyWindowHandle,
    marks: &Map<String, Value>,
    cx: &mut AsyncApp,
) -> Result<Value, String> {
    let mut now = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            dump(shell, window, cx)
        })
        .map_err(|e| e.to_string())?;
    now["marks"] = Value::Object(marks.clone());
    Ok(now)
}

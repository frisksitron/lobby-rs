//! The Linux half of `cargo xtask drive`. Windows opens the driven windows off screen; here they
//! open in a headless sway of the run's own (software renderer, a private `XDG_RUNTIME_DIR`), so
//! nothing appears on the person's desktop and `grim` can photograph what GPUI drew, which its
//! Wayland windows cannot render to an image themselves. `--visible` skips the compositor: the
//! windows open on the real desktop, where `shot` is refused (GNOME lets no shell capture it).
//!
//! The saved sessions of a driven client live in the Secret Service, under the service
//! `lobby-rs/tV-drive-<driver pid>-<client>`. The driver deletes them through the same `keyring`
//! crate the app stores them with, and lists them in `target/drive/.tokens/<driver pid>` so the
//! next run can remove what a driver that died left (the Secret Service cannot be searched here).
//! The same next run ends the sway of a driver that died, and removes its runtime directory. It
//! ends the `lobby-server`s too, which `target/drive/.servers/<driver pid>` lists.

use std::{
    fs,
    io::Read,
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::{Client, TOKEN_SERVICE};

/// What `apt` installs for the tools the driver runs.
const PACKAGES: &str = "sway grim";
const COMPOSITOR_WITHIN: Duration = Duration::from_secs(10);
const WINDOW_WITHIN: Duration = Duration::from_secs(10);
const EXIT_WITHIN: Duration = Duration::from_secs(3);
/// A `swaymsg`, `grim` or `kill` that takes longer is wedged: it is killed and reaped.
const HELPER_WITHIN: Duration = Duration::from_secs(5);
/// The app has drawn at the size the compositor set within this.
const RESIZE_WITHIN: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
const EXIT_POLL: Duration = Duration::from_millis(20);
const HELPER_POLL: Duration = Duration::from_millis(5);
/// How much of `sway.log` an error that ends the run carries.
const LOG_TAIL_LINES: usize = 20;
/// A runtime directory is `<temp dir>/lobby-drive-<driver pid>`.
const DIR_PREFIX: &str = "lobby-drive-";
/// In the runtime directory: the pid of the sway the driver started.
const SWAY_PID: &str = "sway.pid";
/// Two pictures this far apart that agree are a settled window; one that never agrees is taken
/// as it is after `SETTLE_WITHIN`, as the app's own `shot` does.
const SETTLE_FRAME: Duration = Duration::from_millis(50);
const SETTLE_WITHIN: Duration = Duration::from_secs(1);
/// One output wide enough to set a few windows side by side, so each client is photographed
/// without another window over it.
const OUTPUT: &str = "3840x1080";
const SLOT_WIDTH: i64 = 760;
/// `XTASK_DRIVE_SCALE=1.5` runs the output at that display scale, to look at a window the way a
/// scaled desktop shows it. The slots are then laid out in the output's logical pixels. Not
/// `LOBBY_*`: the server reads those as its own settings and refuses one it does not know.
const SCALE_VAR: &str = "XTASK_DRIVE_SCALE";

fn output_scale() -> Result<f64, String> {
    let Some(text) = std::env::var_os(SCALE_VAR) else {
        return Ok(1.0);
    };
    text.to_str()
        .and_then(|text| text.parse::<f64>().ok())
        .filter(|scale| (1.0..=4.0).contains(scale))
        .ok_or_else(|| format!("{SCALE_VAR} is a number from 1 to 4"))
}

/// Fails early, naming the package, when a tool the run needs is not installed.
pub fn require_tools(compositor: bool) -> Result<(), String> {
    if !compositor {
        return Ok(());
    }
    let have = |tool: &str| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
    };
    for (tool, package) in [("sway", "sway"), ("swaymsg", "sway"), ("grim", "grim")] {
        if !have(tool) {
            return Err(format!(
                "`{tool}` is not installed: sudo apt install {PACKAGES} (missing package: {package})"
            ));
        }
    }
    Ok(())
}

/// The run's private `XDG_RUNTIME_DIR`: owned from the moment it is made, removed with it.
struct RuntimeDir(PathBuf);

impl RuntimeDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A headless sway and the way to talk to it.
pub struct Desktop {
    dir: RuntimeDir,
    wayland: String,
    ipc: PathBuf,
    sway: Child,
}

/// Runs `cmd` to its end, or kills and reaps it once `within` has passed.
fn run_within(cmd: &mut Command, within: Duration) -> Result<Output, String> {
    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    }
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let until = Instant::now() + within;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < until => std::thread::sleep(HELPER_POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} did not finish within {within:?}, so it was killed"
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program}: {e}"));
            }
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

fn runtime_dir_of(run: u32) -> PathBuf {
    std::env::temp_dir().join(format!("{DIR_PREFIX}{run}"))
}

/// The tail of a log, for an error that is about to take the log's directory with it.
fn log_tail(log: &Path) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n");
    if tail.is_empty() {
        format!("{} is empty", log.display())
    } else {
        format!("the last lines of {}:\n{tail}", log.display())
    }
}

/// Whether process `pid` was started with `file` as an argument: a recycled pid was not. A sway
/// runs `--config <dir>/sway.conf` and a `lobby-server` `--config <dir>/lobby.toml`, both paths
/// of one run.
fn runs_with(pid: u32, file: &Path) -> bool {
    fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmdline| {
        cmdline
            .split(|&b| b == 0)
            .any(|arg| arg == file.as_os_str().as_encoded_bytes())
    })
}

/// Ends process `pid` while `runs_with(pid, file)`: TERM, then KILL, each given [`EXIT_WITHIN`].
/// Whether it is gone.
fn end_process(pid: u32, file: &Path) -> bool {
    for signal in ["-TERM", "-KILL"] {
        if !runs_with(pid, file) {
            break;
        }
        let _ = run_within(
            Command::new("kill").arg(signal).arg(pid.to_string()),
            HELPER_WITHIN,
        );
        let until = Instant::now() + EXIT_WITHIN;
        while runs_with(pid, file) && Instant::now() < until {
            std::thread::sleep(EXIT_POLL);
        }
    }
    !runs_with(pid, file)
}

/// Ends the sway left in `dir` by a driver that is gone and removes the directory. A sway that
/// will not end keeps its directory, so the next run tries again.
fn reclaim(dir: &Path) {
    if let Some(pid) = fs::read_to_string(dir.join(SWAY_PID))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .filter(|&pid| runs_with(pid, &dir.join("sway.conf")))
        && !end_process(pid, &dir.join("sway.conf"))
    {
        eprintln!(
            "drive: sway {pid} left by a driver that died would not end; {} stays",
            dir.display()
        );
        return;
    }
    let _ = fs::remove_dir_all(dir);
}

fn servers_path(run: u32) -> PathBuf {
    crate::workspace_root()
        .join("target/drive/.servers")
        .join(run.to_string())
}

/// Writes down a `lobby-server` this run started (its pid and its config file), so the next run
/// can end it if the driver is killed before it can. The servers start before the sway, and
/// `--visible` has no sway directory, so the record lives beside the tokens'.
pub fn record_server(run: u32, pid: u32, config: &Path) -> Result<(), String> {
    use std::io::Write as _;
    let path = servers_path(run);
    let written = fs::create_dir_all(path.parent().expect("a file has a folder"))
        .and_then(|()| fs::OpenOptions::new().create(true).append(true).open(&path))
        .and_then(|mut file| writeln!(file, "{pid} {}", config.display()));
    written.map_err(|e| format!("cannot record the server in {}: {e}", path.display()))
}

/// Ends the servers of drivers that are gone. A server is signalled only if its command line
/// still names the config file recorded with its pid. A record whose server will not end stays.
pub fn sweep_stale_servers() {
    let Ok(entries) = fs::read_dir(servers_path(0).parent().expect("a file has a folder")) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(run) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if process_alive(run) {
            continue;
        }
        let text = fs::read_to_string(entry.path()).unwrap_or_default();
        let mut left = Vec::new();
        for line in text.lines() {
            let Some((pid, config)) = line.split_once(' ') else {
                continue;
            };
            let (Ok(pid), config) = (pid.parse::<u32>(), Path::new(config)) else {
                continue;
            };
            if !runs_with(pid, config) {
                continue;
            }
            if end_process(pid, config) {
                eprintln!("drive: ended lobby-server {pid} left by a driver that died");
            } else {
                eprintln!("drive: lobby-server {pid} left by a driver that died would not end");
                left.push(line);
            }
        }
        if left.is_empty() {
            let _ = fs::remove_file(entry.path());
        } else {
            let _ = fs::write(entry.path(), left.join("\n"));
        }
    }
}

/// Reclaims the runtime directories of drivers that are gone.
fn sweep_stale_desktops() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(run) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix(DIR_PREFIX))
            .and_then(|run| run.parse::<u32>().ok())
        else {
            continue;
        };
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) && !process_alive(run) {
            reclaim(&entry.path());
        }
    }
}

/// A driven client's place on its run's [`Desktop`].
#[derive(Clone)]
pub struct Stage {
    pub desktop: Arc<Desktop>,
    pub slot: usize,
}

impl Desktop {
    pub fn start() -> Result<Desktop, String> {
        sweep_stale_desktops();
        let path = runtime_dir_of(std::process::id());
        // A driver that died with this pid left it, if anything did.
        reclaim(&path);
        // Short, and private: a Unix socket path is limited to about 100 bytes, and the
        // automation sockets of the clients live here.
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let dir = RuntimeDir(path);
        let conf = dir.path().join("sway.conf");
        let scale = output_scale()?;
        fs::write(
            &conf,
            format!(
                "# Written by `cargo xtask drive`.\ndefault_border none\n\
                 default_floating_border none\nfocus_follows_mouse no\n\
                 output * resolution {OUTPUT} position 0 0 scale {scale}\n\
                 for_window [shell=\"xdg_shell\"] floating enable\n"
            ),
        )
        .map_err(|e| e.to_string())?;
        let log_path = dir.path().join("sway.log");
        let log = fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let sway = Command::new("sway")
            .arg("--config")
            .arg(&conf)
            .env("XDG_RUNTIME_DIR", dir.path())
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env_remove("SWAYSOCK")
            .env_remove("I3SOCK")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("cannot start sway: {e} (sudo apt install {PACKAGES})"))?;
        let mut desktop = Desktop {
            dir,
            wayland: String::new(),
            ipc: PathBuf::new(),
            sway,
        };
        // The next run ends this sway if the driver is killed before it can.
        fs::write(
            desktop.dir.path().join(SWAY_PID),
            desktop.sway.id().to_string(),
        )
        .map_err(|e| format!("cannot record the sway pid: {e}"))?;
        let until = Instant::now() + COMPOSITOR_WITHIN;
        loop {
            if let Ok(Some(status)) = desktop.sway.try_wait() {
                return Err(format!(
                    "sway exited at start: {status}; {}",
                    log_tail(&log_path)
                ));
            }
            desktop.find_sockets();
            let left = until.saturating_duration_since(Instant::now());
            if !desktop.wayland.is_empty()
                && !desktop.ipc.as_os_str().is_empty()
                && desktop
                    .msg_within(&["-t", "get_outputs"], left.min(HELPER_WITHIN))
                    .is_ok()
            {
                return Ok(desktop);
            }
            if Instant::now() >= until {
                return Err(format!(
                    "sway did not come up within {COMPOSITOR_WITHIN:?}; {}",
                    log_tail(&log_path)
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    fn find_sockets(&mut self) {
        let Ok(entries) = fs::read_dir(self.dir.path()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("wayland-") && !name.ends_with(".lock") {
                self.wayland = name;
            } else if name.starts_with("sway-ipc.") && name.ends_with(".sock") {
                self.ipc = entry.path();
            }
        }
    }

    /// The directory the clients' automation sockets are in.
    pub fn runtime_dir(&self) -> &Path {
        self.dir.path()
    }

    /// What a client needs to open its window here and not on the person's desktop.
    pub fn env(&self, cmd: &mut Command) {
        cmd.env("XDG_RUNTIME_DIR", self.dir.path())
            .env("WAYLAND_DISPLAY", &self.wayland)
            .env_remove("DISPLAY")
            // Nobody parks a window: there is no desktop to keep it off.
            .env("LOBBY_AUTOMATION_VISIBLE", "1");
    }

    fn msg(&self, args: &[&str]) -> Result<String, String> {
        self.msg_within(args, HELPER_WITHIN)
    }

    fn msg_within(&self, args: &[&str], within: Duration) -> Result<String, String> {
        let out = run_within(
            Command::new("swaymsg")
                .args(args)
                .env("XDG_RUNTIME_DIR", self.dir.path())
                .env("SWAYSOCK", &self.ipc),
            within,
        )
        .map_err(|e| format!("swaymsg {}: {e}", args.join(" ")))?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            return Err(format!(
                "swaymsg {}: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim(),
                text.trim()
            ));
        }
        if let Ok(Value::Array(replies)) = serde_json::from_str::<Value>(&text)
            && let Some(failed) = replies.iter().find(|r| r["success"] == false)
        {
            return Err(format!("swaymsg {}: {}", args.join(" "), failed["error"]));
        }
        Ok(text)
    }

    /// Where the window of process `pid` is on the output, once it has one.
    fn window(&self, pid: u32) -> Result<[i64; 4], String> {
        let until = Instant::now() + WINDOW_WITHIN;
        loop {
            let tree: Value = serde_json::from_str(&self.msg(&["-t", "get_tree", "-r"])?)
                .map_err(|e| e.to_string())?;
            if let Some(rect) = find_window(&tree, pid) {
                return Ok(rect);
            }
            if Instant::now() >= until {
                return Err(format!(
                    "the window of process {pid} did not appear on the driver's compositor \
                     within {WINDOW_WITHIN:?}"
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    fn place(&self, pid: u32, slot: usize) -> Result<(), String> {
        self.window(pid)?;
        let x = (slot as i64 * SLOT_WIDTH).to_string();
        self.msg(&[
            &format!("[pid={pid}]"),
            "move",
            "absolute",
            "position",
            &x,
            "0",
        ])
        .map(drop)
    }

    fn resize(&self, pid: u32, width: f64, height: f64) -> Result<(), String> {
        self.window(pid)?;
        self.msg(&[
            &format!("[pid={pid}]"),
            "resize",
            "set",
            "width",
            &format!("{}", width.round() as i64),
            "px",
            "height",
            &format!("{}", height.round() as i64),
            "px",
        ])
        .map(drop)
    }

    /// A PNG of the window of `pid`, or of the part of it at `crop` (x, y, width, height in the
    /// window's own pixels), once two pictures a frame apart agree.
    fn picture(&self, pid: u32, crop: Option<[f64; 4]>) -> Result<(Vec<u8>, bool), String> {
        let [wx, wy, ww, wh] = self.window(pid)?;
        let (x, y, w, h) = match crop {
            None => (wx, wy, ww, wh),
            Some([cx, cy, cw, ch]) => {
                let (left, top) = (cx.floor().max(0.) as i64, cy.floor().max(0.) as i64);
                let right = ((cx + cw).ceil() as i64).min(ww);
                let bottom = ((cy + ch).ceil() as i64).min(wh);
                (wx + left, wy + top, right - left, bottom - top)
            }
        };
        if w <= 0 || h <= 0 {
            return Err("the element lies outside the window".into());
        }
        let region = format!("{x},{y} {w}x{h}");
        let take = || -> Result<Vec<u8>, String> {
            let out = run_within(
                Command::new("grim")
                    .args(["-g", &region, "-"])
                    .env("XDG_RUNTIME_DIR", self.dir.path())
                    .env("WAYLAND_DISPLAY", &self.wayland),
                HELPER_WITHIN,
            )?;
            if out.status.success() {
                Ok(out.stdout)
            } else {
                Err(format!(
                    "grim: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        };
        let until = Instant::now() + SETTLE_WITHIN;
        let mut last = take()?;
        loop {
            if Instant::now() >= until {
                return Ok((last, false));
            }
            std::thread::sleep(SETTLE_FRAME);
            let next = take()?;
            if next == last {
                return Ok((next, true));
            }
            last = next;
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // `exit` ends the compositor's own children (its background) with it. The directory is
        // removed after this, by its own `Drop`.
        let _ = self.msg_within(&["exit"], EXIT_WITHIN);
        let until = Instant::now() + EXIT_WITHIN;
        while !matches!(self.sway.try_wait(), Ok(Some(_))) && Instant::now() < until {
            std::thread::sleep(EXIT_POLL);
        }
        if !matches!(self.sway.try_wait(), Ok(Some(_))) {
            let _ = self.sway.kill();
            let _ = self.sway.wait();
        }
    }
}

/// The window of `pid` in a sway tree: its content's place on the output.
fn find_window(node: &Value, pid: u32) -> Option<[i64; 4]> {
    if node["pid"] == pid {
        let (rect, content) = (&node["rect"], &node["window_rect"]);
        let n = |v: &Value| v.as_i64().unwrap_or(0);
        let found = [
            n(&rect["x"]) + n(&content["x"]),
            n(&rect["y"]) + n(&content["y"]),
            n(&content["width"]),
            n(&content["height"]),
        ];
        if found[2] > 0 && found[3] > 0 {
            return Some(found);
        }
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|key| node[*key].as_array())
        .flatten()
        .find_map(|child| find_window(child, pid))
}

impl Client {
    /// Puts a just opened window in its slot on the compositor.
    pub(super) fn place(&self) -> Result<(), String> {
        match (&self.stage, &self.child) {
            (Some(stage), Some(child)) => stage.desktop.place(child.id(), stage.slot),
            _ => Ok(()),
        }
    }

    /// Sets the window's size and returns once the app has drawn at it: the compositor's answer
    /// comes before the app's next frame.
    pub(super) fn resize_window(&mut self, width: f64, height: f64) -> Result<(), String> {
        let stage = match (&self.stage, &self.child) {
            (Some(stage), Some(child)) => Some((stage.desktop.clone(), child.id())),
            _ => None,
        };
        let Some((desktop, pid)) = stage else {
            return Err("a window size needs the headless compositor, not --visible".into());
        };
        desktop.resize(pid, width, height)?;
        let until = Instant::now() + RESIZE_WITHIN;
        loop {
            let dump = self.expect_ok(&json!({"dump": "window"}))?;
            let size = |key: &str| dump["dump"]["window"][key].as_f64().unwrap_or(0.);
            let (w, h) = (size("width"), size("height"));
            if (w - width.round()).abs() < 1. && (h - height.round()).abs() < 1. {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(format!(
                    "{}: the window is {w}×{h} {RESIZE_WITHIN:?} after resizing it to {width}×{height}",
                    self.name
                ));
            }
            std::thread::sleep(POLL);
            // Seen with a third client: sway took the first request and the window stayed as
            // it was, so ask again until the app has drawn at the size.
            desktop.resize(pid, width, height)?;
        }
    }

    /// The steps the driver does itself here: the pictures (the app cannot render its window to
    /// an image under Wayland) and window sizes (the compositor sets them). `None`: send it on.
    pub(super) fn desktop_step(
        &mut self,
        step: &Value,
        out: &Path,
    ) -> Option<Result<Value, String>> {
        if let Some(name) = step.get("shot").and_then(Value::as_str) {
            let path = out.join(format!("{}-{name}.png", self.name));
            return Some(self.shot(step, &path));
        }
        let size = step
            .get("action")
            .and_then(Value::as_str)
            .and_then(|a| a.strip_prefix("resize:"))
            .and_then(|s| s.split_once('x'))
            .and_then(|(w, h)| Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?)));
        size.map(|(w, h)| self.resize_window(w, h).map(|()| json!({})))
    }

    fn shot(&mut self, step: &Value, path: &Path) -> Result<Value, String> {
        let Some(stage) = self.stage.clone() else {
            return Err(
                "a shot needs the headless compositor: --visible opens the window on the \
                 desktop, which GNOME will not let a shell capture"
                    .into(),
            );
        };
        let pid = self.child.as_ref().ok_or("not running")?.id();
        if step.get("hover").and_then(Value::as_bool) != Some(true) {
            self.expect_ok(&json!({"rest_pointer": true}))?;
        }
        let crop = match step.get("id").and_then(Value::as_str) {
            None => None,
            Some(id) => {
                let dump = self.expect_ok(&json!({"dump": "bounds"}))?;
                let element = &dump["dump"]["elements"][id];
                if element["visible"] != true {
                    return Err(format!("{id:?} is not drawn"));
                }
                let b = element["bounds"]
                    .as_array()
                    .filter(|b| b.len() == 4)
                    .ok_or_else(|| format!("{id:?} has no bounds"))?;
                let n = |i: usize| b[i].as_f64().unwrap_or(0.);
                // Bounds are measured in the app's surface, which a client-decorated window
                // draws larger than the window the compositor shows, by a margin for its shadow.
                let margin = &dump["dump"]["window"]["margin"];
                let m = |i: usize| margin[i].as_f64().unwrap_or(0.);
                Some([n(0) - m(0), n(1) - m(1), n(2), n(3)])
            }
        };
        let (png, settled) = stage.desktop.picture(pid, crop)?;
        fs::write(path, png).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(json!({"said": path.display().to_string(), "settled": settled}))
    }

    fn expect_ok(&mut self, step: &Value) -> Result<Value, String> {
        let answer = self.request(step)?;
        if answer["ok"] == true {
            Ok(answer)
        } else {
            Err(answer["error"].as_str().unwrap_or("failed").to_owned())
        }
    }
}

fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn record_path(run: u32) -> PathBuf {
    crate::workspace_root()
        .join("target/drive/.tokens")
        .join(run.to_string())
}

/// Writes down the saved sessions this run may make, before its first client starts. A run that
/// cannot write them down would leave items nobody removes, so it does not start.
pub fn record_tokens(run: u32, targets: &[String]) -> Result<(), String> {
    let path = record_path(run);
    let written = fs::create_dir_all(path.parent().expect("a file has a folder"))
        .and_then(|()| fs::write(&path, targets.join("\n")));
    written.map_err(|e| {
        format!(
            "cannot record the saved sessions in {}: {e}",
            path.display()
        )
    })
}

/// Deletes what the manifest of run `run` lists. The manifest goes once every delete succeeded or
/// found nothing to delete; otherwise it keeps what is left, for the next run to retry.
pub fn forget_recorded(run: u32, targets: &[String]) {
    let path = record_path(run);
    let left: Vec<&String> = targets
        .iter()
        .filter(|target| match forget(target) {
            Ok(()) => false,
            Err(e) => {
                eprintln!("drive: {e}");
                true
            }
        })
        .collect();
    if left.is_empty() {
        let _ = fs::remove_file(path);
        return;
    }
    let text = left
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if let Err(e) = fs::write(&path, text) {
        eprintln!("drive: cannot keep {}: {e}", path.display());
    } else {
        eprintln!(
            "drive: {} saved session(s) stay in {} for the next run to remove",
            left.len(),
            path.display()
        );
    }
}

/// Removes the saved sessions of drivers that are gone.
pub fn sweep_stale_tokens() {
    let Ok(entries) = fs::read_dir(record_path(0).parent().expect("a file has a folder")) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(run) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if process_alive(run) {
            continue;
        }
        let text = fs::read_to_string(entry.path()).unwrap_or_default();
        let targets: Vec<String> = text.lines().map(str::to_owned).collect();
        forget_recorded(run, &targets);
    }
}

/// Deletes the saved session `<origin>.lobby-rs/<profile>` from the Secret Service. An item that
/// is not there is as good as deleted.
fn forget(target: &str) -> Result<(), String> {
    let Some((origin, profile)) = target.split_once(&format!(".{TOKEN_SERVICE}")) else {
        return Ok(());
    };
    let service = format!("{TOKEN_SERVICE}{profile}");
    let deleted = keyring::Entry::new(&service, origin).and_then(|e| e.delete_credential());
    match deleted {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!(
            "could not remove the saved session {service} for {origin}: {e}"
        )),
    }
}

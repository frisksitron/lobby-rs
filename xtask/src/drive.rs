//! `cargo xtask drive <scenario>`: runs a scenario file of `crates/lobby-desktop/tests/scenarios/`
//! against the real binaries. A `lobby-server` runs on free ports with its own scratch data; each
//! client is a `lobby` with the `automation` feature, its own data directory and a
//! `LOBBY_INSTANCE_ID` unique per run, so parallel drives never share a pipe or tokens. Clients are
//! driven through their automation pipe. Nothing takes the focus, moves the mouse or raises a
//! Windows toast: windows open off screen without activation and get synthetic GPUI input.
//!
//! The driver does what a client cannot: the server (`stop` ends the process, `start` runs it again
//! on the same ports and database), `open`, `close` (the Quit flow, then the process must exit) and
//! `corrupt`. Every other step goes to the client's pipe as written, with `{lobby}` and `{lobby2}`
//! filled in, and `{code:<email>}` (or `{code2:…}`) as the newest Magic Code mailed there: the
//! servers mail into the driver's own SMTP sink. `freeze`/`thaw` are not supported (no proxy).
//! Shots, dumps and the transcript land in `target/drive/<scenario>/out/`.
//!
//! Clients keep their tokens in the real Credential Manager, one entry per client and Lobby origin,
//! so `close`/`open` restores the session. The driver removes its run's entries when it ends, and
//! at start those of earlier runs whose driver is gone (for example after Ctrl+C).

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead as _, BufReader, Write as _},
    net::{TcpListener, TcpStream, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use lobby_server::testing::SmtpSink;
use serde_json::{Value, json};

const PIPE_WITHIN: Duration = Duration::from_secs(30);
const SERVER_WITHIN: Duration = Duration::from_secs(20);
const QUIT_WITHIN: Duration = Duration::from_secs(6);
/// How long a client may take to answer a step that does not wait (a shot settles within 1 s);
/// a `wait` gets its own timeout plus this. Past it the app is stuck: it is ended, not waited
/// on forever.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
const TOKEN_SERVICE: &str = "lobby-rs/tV-drive-";
/// How long a `{code:…}` waits for its mail to arrive.
const MAIL_WITHIN: Duration = Duration::from_secs(5);

#[derive(Debug, Default, Clone)]
pub struct Options {
    pub scenario: String,
    pub instances: Option<Vec<String>>,
    pub build: bool,
    pub visible: bool,
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut opts = Options {
        build: true,
        ..Options::default()
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--instances" => {
                let list = args.next().ok_or("--instances needs a list")?;
                opts.instances = Some(list.split(',').map(str::to_owned).collect());
            }
            "--no-build" => opts.build = false,
            "--visible" => opts.visible = true,
            other if other.starts_with("--") => return Err(format!("unknown option {other:?}")),
            other if opts.scenario.is_empty() => opts.scenario = other.to_owned(),
            other => return Err(format!("one scenario at a time, not {other:?}")),
        }
    }
    if opts.scenario.is_empty() {
        return Err("drive needs a scenario".into());
    }
    Ok(opts)
}

fn fill(s: &str, lobby: &str, lobby2: Option<&str>) -> Result<String, String> {
    let host_of = |origin: &'_ str| {
        origin
            .split_once("://")
            .map_or(origin.to_owned(), |(_, h)| h.to_owned())
    };
    let mut s = s
        .replace("{lobby}", lobby)
        .replace("{host}", &host_of(lobby));
    if s.contains("{lobby2}") || s.contains("{host2}") {
        let second = lobby2.ok_or("{lobby2} and {host2} need a second Lobby")?;
        s = s
            .replace("{lobby2}", second)
            .replace("{host2}", &host_of(second));
    }
    Ok(s)
}

/// `{code:<email>}` and `{code2:<email>}` as the newest code mailed to that email since the last
/// one a step took. Both Lobbies mail into the one sink, so the newest is the one the Lobby just
/// asked for.
fn codes(s: &str, mail: &mut Mail) -> Result<String, String> {
    let mut s = s.to_owned();
    while let Some(start) = s.find("{code:").or_else(|| s.find("{code2:")) {
        let open = s[start..].find(':').map_or(0, |colon| colon + 1);
        let end = start + s[start..].find('}').ok_or("unclosed {code:…}")?;
        let email = s[start + open..end].to_owned();
        let seen = mail.seen.get(&email).copied().unwrap_or(0);
        let sink = &mail.sink;
        let (count, code) = mail
            .rt
            .block_on(async {
                tokio::time::timeout(MAIL_WITHIN, sink.code_after(&email, seen)).await
            })
            .map_err(|_| format!("no new code was mailed to {email}"))?;
        mail.seen.insert(email, count);
        s.replace_range(start..=end, &code);
    }
    Ok(s)
}

/// The servers' outbox: an SMTP sink on a runtime of the driver's own.
struct Mail {
    sink: SmtpSink,
    rt: tokio::runtime::Runtime,
    seen: BTreeMap<String, usize>,
}

impl Mail {
    fn start() -> Result<Mail, String> {
        let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let sink = rt.block_on(SmtpSink::start());
        Ok(Mail {
            sink,
            rt,
            seen: BTreeMap::new(),
        })
    }
}

fn scenario_path(name: &str) -> PathBuf {
    let direct = PathBuf::from(name);
    if direct.exists() {
        return direct;
    }
    crate::workspace_root()
        .join("crates/lobby-desktop/tests/scenarios")
        .join(format!("{}.jsonl", name.trim_end_matches(".jsonl")))
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Header {
    pub clients: Vec<String>,
    pub members: Vec<String>,
    pub second_lobby: bool,
    pub lobby_name: Option<String>,
    pub bootstrap_email: Option<String>,
    pub join_requests: Option<bool>,
    pub size: Option<(f64, f64)>,
    /// The header's `visible`: the windows open on screen, as `--visible` opens them.
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub number: usize,
    pub on: String,
    pub step: serde_json::Map<String, Value>,
}

/// Reads a scenario far enough to drive it; the app parses each step itself and says what is
/// wrong with it. Refuses what the driver cannot do.
pub fn read(text: &str) -> Result<(Header, Vec<Line>), String> {
    let mut header = Header::default();
    let mut lines = Vec::new();
    let mut on = None::<String>;
    for (ix, raw) in text.lines().enumerate() {
        let number = ix + 1;
        let t = raw.trim();
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        let v: Value = serde_json::from_str(t).map_err(|e| format!("line {number}: {e}"))?;
        let Value::Object(mut obj) = v else {
            return Err(format!("line {number}: a step is an object"));
        };
        if obj.contains_key("scenario") {
            if let Some(why) = obj.get("layer3_only").and_then(Value::as_str) {
                return Err(format!(
                    "line {number}: Layer 3 only, {why} (cargo nextest run -p lobby-desktop --test e2e_ui)"
                ));
            }
            for key in ["history", "sent_files", "own_paths"] {
                if obj.contains_key(key) {
                    return Err(format!(
                        "line {number}: Layer 3 only, the driver cannot set up `{key}` (cargo nextest run -p lobby-desktop --test e2e_ui)"
                    ));
                }
            }
            let strings = |key: &str| -> Vec<String> {
                obj.get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            header.clients = strings("clients");
            header.members = strings("members");
            header.second_lobby = obj.get("second_lobby").and_then(Value::as_bool) == Some(true);
            header.lobby_name = obj
                .get("lobby_name")
                .and_then(Value::as_str)
                .map(str::to_owned);
            header.bootstrap_email = obj
                .get("bootstrap_email")
                .and_then(Value::as_str)
                .map(str::to_owned);
            header.join_requests = obj.get("join_requests").and_then(Value::as_bool);
            header.visible = obj.get("visible").and_then(Value::as_bool) == Some(true);
            header.size = obj
                .get("size")
                .and_then(Value::as_array)
                .and_then(|wh| Some((wh.first()?.as_f64()?, wh.get(1)?.as_f64()?)));
            continue;
        }
        if let Some(target) = obj.remove("on") {
            on = target.as_str().map(str::to_owned);
        }
        if header.clients.is_empty() {
            header.clients.push("a".into());
        }
        let target = on.clone().unwrap_or_else(|| header.clients[0].clone());
        if matches!(
            obj.get("server").and_then(Value::as_str),
            Some("freeze" | "thaw")
        ) {
            return Err(format!(
                "line {number}: freeze/thaw needs Layer 3's proxy (cargo nextest run -p lobby-desktop --test e2e_ui)"
            ));
        }
        lines.push(Line {
            number,
            on: target,
            step: obj,
        });
    }
    if header.clients.is_empty() {
        header.clients.push("a".into());
    }
    Ok((header, lines))
}

fn free_tcp() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn free_udp() -> std::io::Result<u16> {
    Ok(UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

struct Server {
    name: String,
    dir: PathBuf,
    config: PathBuf,
    port: u16,
    child: Option<Child>,
}

impl Server {
    fn create(dir: PathBuf, name: &str, header: &Header, smtp: u16) -> Result<Server, String> {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let port = free_tcp().map_err(|e| e.to_string())?;
        let udp = free_udp().map_err(|e| e.to_string())?;
        // Each server has its own, or a second Lobby could not start beside the first.
        let sfu_tcp = free_tcp().map_err(|e| e.to_string())?;
        // Every client signs in from one address and one inbox, as in the in-process tests.
        let mut auth = String::from("magic_code_sends_per_10m = 1000\n");
        if let Some(email) = &header.bootstrap_email {
            auth.push_str(&format!("bootstrap_email = \"{email}\"\n"));
        }
        if let Some(on) = header.join_requests {
            auth.push_str(&format!("join_requests_enabled = {on}\n"));
        }
        let config = dir.join("lobby.toml");
        fs::write(
            &config,
            format!(
                "# Written by `cargo xtask drive`.\n[lobby]\nname = \"{name}\"\n\n\
                 [http]\nbind = \"127.0.0.1:{port}\"\nrate_limits = false\n\n\
                 [database]\npath = \"{db}\"\n\n[storage]\nblob_root = \"{blobs}\"\n\n\
                 [auth]\n{auth}\n[smtp]\nhost = \"127.0.0.1\"\nport = {smtp}\nfrom = \"Lobby <noreply@lobby.test>\"\n\n\
                 [sfu]\nudp_bind = \"127.0.0.1:{udp}\"\ntcp_bind = \"127.0.0.1:{sfu_tcp}\"\nadvertise_ips = [\"127.0.0.1\"]\n",
                db = slashes(&dir.join("lobby.db")),
                blobs = slashes(&dir.join("blobs")),
            ),
        )
        .map_err(|e| e.to_string())?;
        Ok(Server {
            name: name.to_owned(),
            dir,
            config,
            port,
            child: None,
        })
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn create_members(&self, names: &[String]) -> Result<(), String> {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        lobby_server::testing::seed_members_blocking(&self.dir.join("lobby.db"), &names)
    }

    fn start(&mut self, bin: &Path) -> Result<(), String> {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("server.log"))
            .map_err(|e| e.to_string())?;
        // The server refuses `LOBBY_*` variables it does not know; this one is the app's, set to
        // drive the updater against a local feed.
        let child = Command::new(bin)
            .arg("--config")
            .arg(&self.config)
            .env_remove("LOBBY_UPDATE_URL")
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", bin.display()))?;
        self.child = Some(child);
        let until = Instant::now() + SERVER_WITHIN;
        while TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            if Instant::now() >= until {
                return Err(format!("{} does not accept on {}", self.name, self.port));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One `lobby` process and its pipe. A worker thread owns the pipe and does each write and its
/// read (a synchronous pipe handle serializes I/O, so a read pending on one thread would block a
/// write from another); the driver waits for the answer with a deadline, so a stuck app costs a
/// timeout rather than the driver.
struct Client {
    name: String,
    run: u32,
    dir: PathBuf,
    child: Option<Child>,
    pipe: Option<(mpsc::Sender<String>, mpsc::Receiver<String>)>,
    /// The header's `size`: the window is resized to it each time it opens.
    size: Option<(f64, f64)>,
}

/// The instance id of `client` in run `run` (also its keychain profile).
fn instance_id(run: u32, client: &str) -> String {
    format!("tV-drive-{run}-{client}")
}

impl Client {
    fn instance(&self) -> String {
        instance_id(self.run, &self.name)
    }

    fn pipe_name(&self) -> String {
        format!(r"\\.\pipe\lobby-automation-{}", self.instance())
    }

    fn open(&mut self, bin: &Path, opts: &Options) -> Result<(), String> {
        if self.child.is_some() {
            return Err(format!("{} is already open", self.name));
        }
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let mut cmd = Command::new(bin);
        cmd.env("LOBBY_AUTOMATION", "1")
            .env("LOBBY_INSTANCE_ID", self.instance())
            .env("LOBBY_USER_DATA_DIR", &self.dir)
            .env("ALLOW_MULTIPLE_INSTANCES", "1")
            .env("LOBBY_FAKE_MEDIA", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if opts.visible {
            cmd.env("LOBBY_AUTOMATION_VISIBLE", "1");
        }
        self.child = Some(
            cmd.spawn()
                .map_err(|e| format!("cannot start {}: {e}", bin.display()))?,
        );
        let until = Instant::now() + PIPE_WITHIN;
        let file = loop {
            match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.pipe_name())
            {
                Ok(f) => break f,
                Err(_) if Instant::now() < until => {
                    if let Some(Ok(Some(status))) = self.child.as_mut().map(Child::try_wait) {
                        return Err(format!("{} exited at start: {status}", self.name));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("{}'s pipe did not open: {e}", self.name)),
            }
        };
        let mut reader = BufReader::new(file.try_clone().map_err(|e| e.to_string())?);
        let mut write = file;
        let (requests, todo) = mpsc::channel::<String>();
        let (tx, answers) = mpsc::channel();
        // Ends when the driver drops its sender or the pipe closes (the app quit or was ended).
        std::thread::spawn(move || {
            for request in todo {
                if writeln!(write, "{request}")
                    .and_then(|()| write.flush())
                    .is_err()
                {
                    return;
                }
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(n) if n > 0 && tx.send(line).is_ok() => {}
                    _ => return,
                }
            }
        });
        self.pipe = Some((requests, answers));
        let hello = self.request(&json!({"hello": true}))?;
        if hello["ok"] != true {
            return Err(format!("{}: {hello}", self.name));
        }
        if let Some((w, h)) = self.size {
            let resized = self.request(&json!({"action": format!("resize:{w}x{h}")}))?;
            if resized["ok"] != true {
                return Err(format!("{}: resize to {w}×{h}: {resized}", self.name));
            }
        }
        Ok(())
    }

    fn request(&mut self, step: &Value) -> Result<Value, String> {
        // A `wait` takes as long as it says; anything else answers promptly.
        let within = step.get("wait").map_or(ANSWER_WITHIN, |_| {
            let ms = step
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(10_000);
            Duration::from_millis(ms) + ANSWER_WITHIN
        });
        let (requests, answers) = self
            .pipe
            .as_mut()
            .ok_or_else(|| format!("{} is not open", self.name))?;
        if requests.send(step.to_string()).is_err() {
            return Err(format!("{} closed its pipe", self.name));
        }
        let line = match answers.recv_timeout(within) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("{} closed its pipe", self.name));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.end();
                return Err(format!(
                    "{} did not answer within {within:?}: the app is stuck, so it was ended",
                    self.name
                ));
            }
        };
        serde_json::from_str(&line).map_err(|e| format!("{}: bad answer {line:?}: {e}", self.name))
    }

    fn end(&mut self) {
        self.pipe = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn close(&mut self) -> Result<Duration, String> {
        let started = Instant::now();
        let answer = self.request(&json!({"action": "lobby::Quit"}))?;
        self.pipe = None;
        if answer["ok"] != true {
            return Err(format!("{}: Quit: {answer}", self.name));
        }
        let mut child = self.child.take().ok_or("not running")?;
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if !status.success() {
                    return Err(format!("{} exited with {status}", self.name));
                }
                return Ok(started.elapsed());
            }
            if started.elapsed() > QUIT_WITHIN {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} did not exit within {QUIT_WITHIN:?}", self.name));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.end();
    }
}

/// The Credential Manager entries of a driven client: `<origin>.<service>`, as the keyring
/// crate names them on Windows.
struct Tokens {
    targets: Vec<String>,
}

impl Tokens {
    /// Removes what earlier runs left behind: entries of a driver process that is gone.
    fn sweep_stale() {
        for target in token_targets() {
            let Some((_, profile)) = target.split_once(TOKEN_SERVICE) else {
                continue;
            };
            let run = profile
                .split_once('-')
                .and_then(|(run, _)| run.parse::<u32>().ok());
            if run.is_none_or(|pid| !process_alive(pid)) {
                forget(&target);
            }
        }
    }
}

impl Drop for Tokens {
    fn drop(&mut self) {
        for target in &self.targets {
            forget(target);
        }
    }
}

fn token_targets() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Ok(out) = Command::new("cmdkey").arg("/list").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once("target=").map(|(_, t)| t.trim().to_owned()))
        .filter(|t| t.contains(TOKEN_SERVICE))
        .collect()
}

fn forget(target: &str) {
    if cfg!(windows) {
        let _ = Command::new("cmdkey")
            .arg(format!("/delete:{target}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn process_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
}

fn build() -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let server = &["build", "-p", "lobby-server", "--bin", "lobby-server"][..];
    let client = &[
        "build",
        "-p",
        "lobby-desktop",
        "--features",
        "automation",
        "--bin",
        "lobby",
    ][..];
    for args in [server, client] {
        let status = crate::workspace_cargo(&cargo)
            .args(args)
            .current_dir(crate::workspace_root())
            .status()
            .map_err(|e| format!("cannot run cargo: {e}"))?;
        if !status.success() {
            return Err(format!("cargo {} failed", args.join(" ")));
        }
    }
    Ok(())
}

pub fn run(opts: &Options) -> Result<String, String> {
    let path = scenario_path(&opts.scenario);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_stem()
        .map_or("scenario".into(), |s| s.to_string_lossy().into_owned());
    let (header, lines) = read(&text)?;
    let opts = &Options {
        visible: opts.visible || header.visible,
        ..opts.clone()
    };
    if let Some(wanted) = &opts.instances
        && *wanted != header.clients
    {
        return Err(format!(
            "--instances {} but {name} drives {}",
            wanted.join(","),
            header.clients.join(",")
        ));
    }
    if opts.build {
        build()?;
    }
    // A worktree builds into the main checkout's target through CARGO_TARGET_DIR.
    let bin = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| crate::workspace_root().join("target"), PathBuf::from)
        .join("debug");
    let (server_bin, client_bin) = (bin.join("lobby-server.exe"), bin.join("lobby.exe"));
    let run_dir = crate::workspace_root().join("target/drive").join(&name);
    let _ = fs::remove_dir_all(&run_dir);
    let out = run_dir.join("out");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut transcript =
        fs::File::create(out.join("transcript.jsonl")).map_err(|e| e.to_string())?;

    let mut mail = Mail::start()?;
    let smtp = mail.sink.port();
    let mut servers = vec![Server::create(
        run_dir.join("server"),
        header.lobby_name.as_deref().unwrap_or("Test Lobby"),
        &header,
        smtp,
    )?];
    if header.second_lobby {
        servers.push(Server::create(
            run_dir.join("server2"),
            "Second Lobby",
            &header,
            smtp,
        )?);
    }
    for server in &mut servers {
        server.create_members(&header.members)?;
        server.start(&server_bin)?;
    }
    let origins: Vec<String> = servers.iter().map(Server::origin).collect();
    let lobby = origins[0].clone();
    let lobby2 = origins.get(1).cloned();
    let run = std::process::id();
    Tokens::sweep_stale();
    // Declared before the clients, so it is dropped after they have quit.
    let _tokens = Tokens {
        targets: header
            .clients
            .iter()
            .flat_map(|c| {
                origins
                    .iter()
                    .map(move |o| format!("{o}.{TOKEN_SERVICE}{run}-{c}"))
            })
            .collect(),
    };
    let mut clients: BTreeMap<String, Client> = header
        .clients
        .iter()
        .map(|c| {
            (
                c.clone(),
                Client {
                    name: c.clone(),
                    run,
                    dir: run_dir.join("clients").join(c),
                    child: None,
                    pipe: None,
                    size: header.size.filter(|&wh| wh != (680., 480.)),
                },
            )
        })
        .collect();
    let started = Instant::now();
    for c in clients.values_mut() {
        c.open(&client_bin, opts)?;
    }
    println!(
        "drive {name}: server {lobby}, clients {} (pipes tV-drive-{run}-*)",
        header.clients.join(", ")
    );
    let mut failure = None;
    for line in &lines {
        let t = Instant::now();
        let step = Value::Object(line.step.clone());
        let verb = line.step.keys().next().cloned().unwrap_or_default();
        let result: Result<Value, String> = (|| {
            let arg = |key: &str| {
                line.step
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            match verb.as_str() {
                "server" => {
                    let server = &mut servers[0];
                    match arg("server") {
                        "stop" => server.stop(),
                        "start" => server.start(&server_bin)?,
                        "restart" => {
                            server.stop();
                            server.start(&server_bin)?;
                        }
                        other => return Err(format!("server {other:?} is not driven")),
                    }
                    Ok(json!({"ok": true}))
                }
                "open" => {
                    let c = clients.get_mut(arg("open")).ok_or("unknown client")?;
                    c.open(&client_bin, opts)?;
                    Ok(json!({"ok": true}))
                }
                "close" => {
                    let c = clients.get_mut(arg("close")).ok_or("unknown client")?;
                    let took = c.close()?;
                    Ok(json!({"ok": true, "quit_ms": took.as_millis() as u64}))
                }
                "corrupt" => {
                    let c = clients.get_mut(arg("corrupt")).ok_or("unknown client")?;
                    if c.child.is_some() {
                        return Err("close it first".into());
                    }
                    fs::write(c.dir.join("state.json"), "{ not json").map_err(|e| e.to_string())?;
                    Ok(json!({"ok": true}))
                }
                _ => {
                    let filled = fill(&step.to_string(), &lobby, lobby2.as_deref())?;
                    let mut step: Value = serde_json::from_str(&codes(&filled, &mut mail)?)
                        .map_err(|e| e.to_string())?;
                    if let Some(shot) = line.step.get("shot").and_then(Value::as_str) {
                        step["path"] = json!(slashes(&out.join(format!("{}-{shot}.png", line.on))));
                    }
                    if let Some(dump) = line.step.get("dump").and_then(Value::as_str) {
                        step["path"] =
                            json!(slashes(&out.join(format!("{}-{dump}.json", line.on))));
                    }
                    let c = clients.get_mut(&line.on).ok_or("unknown client")?;
                    let answer = c.request(&step)?;
                    if answer["ok"] == true {
                        Ok(answer)
                    } else {
                        Err(answer["error"].as_str().unwrap_or("failed").to_owned())
                    }
                }
            }
        })();
        let ms = t.elapsed().as_millis() as u64;
        let at = started.elapsed().as_millis() as u64;
        let entry = match &result {
            Ok(answer) => {
                let mut a = answer.clone();
                if let Some(o) = a.as_object_mut() {
                    o.remove("dump");
                }
                json!({"at_ms": at, "line": line.number, "on": line.on, "step": step, "ok": true, "ms": ms, "answer": a})
            }
            Err(e) => {
                json!({"at_ms": at, "line": line.number, "on": line.on, "step": step, "ok": false, "ms": ms, "error": e})
            }
        };
        let _ = writeln!(transcript, "{entry}");
        match result {
            Ok(_) => println!("  ok   {:>3} {:<2} {step} ({ms} ms)", line.number, line.on),
            Err(e) => {
                println!("  FAIL {:>3} {:<2} {step}: {e}", line.number, line.on);
                failure = Some(format!("{name}:{}: {e}", line.number));
                break;
            }
        }
    }
    for c in clients.values_mut() {
        if c.child.is_some()
            && let Err(e) = c.close()
            && failure.is_none()
        {
            failure = Some(format!("{name}: closing at the end: {e}"));
        }
    }
    drop(servers);
    let took = started.elapsed();
    match failure {
        None => Ok(format!(
            "drive {name}: {} steps passed in {took:.1?}; transcript and shots in {}",
            lines.len(),
            out.display()
        )),
        Some(e) => Err(format!("{e} (transcript in {})", out.display())),
    }
}

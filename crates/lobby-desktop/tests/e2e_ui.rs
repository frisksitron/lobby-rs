//! The scenario files in `tests/scenarios/*.jsonl`, each against a real in-process `lobby-server`
//! (behind a `ChaosProxy`, so a scenario can freeze the path) with real `Shell`s as the clients:
//! real `Client`s, real sockets, real Voice with fake media. The files are read at run time, so a
//! new scenario needs no rebuild; `LOBBY_SCENARIO=<name>` runs only the ones whose file name
//! contains it.
//!
//! Each client is a `HeadlessAppContext` (with frames only under `LOBBY_RENDER_TESTS=1`). Its clock
//! follows real time while a step waits, at most `MAX_TICK` a pump, so GPUI timers fire as they
//! would in the app and a slow pump cannot jump past a state that lasts longer. Transcripts, dumps
//! and shots land in `target/ui-shots/scenarios/<scenario>/`.

mod common;

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use common::render::{self, Opening, Rendered};
use lobby_client::{MemoryTokenStore, ReconnectPolicy, testing::ChaosProxy};
use lobby_desktop::automation::{
    self, WindowStep,
    scenario::{self, PasteContent, Predicate, Scenario, ServerOp, Step},
};
use lobby_server::testing::{StoppedServer, TestServer};
use serde_json::{Value, json};

/// Short waits so a lost connection is noticed, retried and given up on within seconds.
fn reconnects() -> ReconnectPolicy {
    ReconnectPolicy {
        tiers: [150, 300, 450, 600].map(Duration::from_millis),
        max_attempts: 10,
        jitter: 0.,
    }
}

const POLL: Duration = Duration::from_millis(10);
/// The most a client's GPUI clock moves in one pump. Polls come far quicker on an idle machine;
/// under load a slow pump (every client settles and renders in turn) slows the app's clock
/// instead of jumping past a state the next step looks for, such as the 2.5 s Esc hint.
const MAX_TICK: Duration = Duration::from_millis(100);
const CLOSE_WITHIN: Duration = Duration::from_secs(5);
const CODE_WITHIN: Duration = Duration::from_secs(5);

fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios")
}

fn load_all() -> Vec<(PathBuf, String)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(scenarios_dir())
        .expect("tests/scenarios exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            (p, text)
        })
        .collect()
}

fn name_of(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

enum Server {
    Running(Box<TestServer>),
    Stopped(Box<StoppedServer>),
    Gone,
}

struct Client {
    name: String,
    dir: Rc<tempfile::TempDir>,
    store: MemoryTokenStore,
    app: Option<Rendered>,
    synced: Instant,
    marks: serde_json::Map<String, Value>,
}

struct World {
    name: String,
    rt: tokio::runtime::Runtime,
    server: Server,
    second: Option<TestServer>,
    proxy: ChaosProxy,
    paths: std::collections::HashMap<String, ChaosProxy>,
    clients: Vec<Client>,
    scratch: tempfile::TempDir,
    out: PathBuf,
    transcript: std::fs::File,
    started: Instant,
    render: bool,
    size: gpui_kit::Size<gpui_kit::Pixels>,
    focused: bool,
    /// How many mails to each address a `{code:…}` has already looked through, by whether the
    /// inbox is the second Lobby's.
    codes_taken: std::collections::HashMap<(bool, String), usize>,
}

impl World {
    fn start(s: &Scenario) -> World {
        let rt = common::tokio_rt();
        let members: Vec<&str> = s.header.members.iter().map(String::as_str).collect();
        let heartbeat = s.header.heartbeat_ms.map(Duration::from_millis);
        let (bootstrap, join_requests) = (s.header.bootstrap_email.clone(), s.header.join_requests);
        let name = s.header.lobby_name.clone();
        let tweak = |c: &mut lobby_server::Config| {
            if let Some(every) = heartbeat {
                c.ws.ping_interval = every;
                c.ws.pong_timeout = every * 10;
            }
            if let Some(email) = &bootstrap {
                c.auth.bootstrap_email = email.clone();
            }
            if let Some(on) = join_requests {
                c.auth.join_requests_enabled = on;
            }
        };
        let history = s
            .header
            .history
            .as_ref()
            .map(|(author, count)| (author.as_str(), *count));
        let server = rt.block_on(TestServer::start_with_history(&members, history, |c| {
            tweak(c);
            if let Some(name) = &name {
                c.lobby.name = name.clone();
            }
        }));
        let server = if s.header.sent_files.is_empty() {
            server
        } else {
            rt.block_on(send_files(server, &s.header.sent_files))
        };
        let second = s.header.second_lobby.then(|| {
            rt.block_on(TestServer::start_with_members(&members, |c| {
                c.lobby.name = "Second Lobby".into();
                tweak(c);
            }))
        });
        let proxy = rt.block_on(ChaosProxy::start(server.addr));
        let paths = s
            .header
            .own_paths
            .iter()
            .map(|c| (c.clone(), rt.block_on(ChaosProxy::start(server.addr))))
            .collect();
        let out = render::shots_dir("scenarios").join(&s.name);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let transcript = std::fs::File::create(out.join("transcript.jsonl")).unwrap();
        let clients = s
            .header
            .clients
            .iter()
            .map(|name| Client {
                name: name.clone(),
                dir: Rc::new(tempfile::tempdir().unwrap()),
                store: MemoryTokenStore::new(),
                app: None,
                synced: Instant::now(),
                marks: serde_json::Map::new(),
            })
            .collect();
        let mut world = World {
            name: s.name.clone(),
            rt,
            server: Server::Running(Box::new(server)),
            second,
            proxy,
            paths,
            clients,
            scratch: tempfile::tempdir().unwrap(),
            out,
            transcript,
            started: Instant::now(),
            render: render::enabled(),
            size: s
                .header
                .size
                .map_or(render::DEFAULT, |(w, h)| render::wh(w, h)),
            focused: s.header.focused,
            codes_taken: std::collections::HashMap::new(),
        };
        for name in s.header.clients.clone() {
            world.open(&name);
        }
        world
    }

    fn fill(&self, text: &str) -> String {
        let mut text = text.replace("{lobby}", self.proxy.origin().as_str());
        for (client, path) in &self.paths {
            text = text.replace(&format!("{{lobby:{client}}}"), path.origin().as_str());
        }
        if let Some(second) = &self.second {
            text = text.replace("{lobby2}", &second.origin());
        }
        // Forward slashes: the path goes into JSON strings as it is.
        let tmp = self.scratch.path().display().to_string().replace('\\', "/");
        while let Some(start) = text.find("{big:") {
            let Some(len) = text[start..].find('}') else {
                break;
            };
            let mib: u64 = text[start + "{big:".len()..start + len]
                .parse()
                .expect("{big:<MiB>}");
            let name = format!("big-{mib}-mib.bin");
            common::fixtures::big_file(self.scratch.path(), &name, mib);
            text.replace_range(start..=start + len, &format!("{tmp}/{name}"));
        }
        text.replace("{tmp}", &tmp)
    }

    fn client(&mut self, name: &str) -> &mut Client {
        self.clients
            .iter_mut()
            .find(|c| c.name == name)
            .expect("parsed scenarios name known clients")
    }

    fn open(&mut self, name: &str) {
        let (render, out, size, focused) = (self.render, self.out.clone(), self.size, self.focused);
        let rt = &self.rt;
        let c = self.clients.iter_mut().find(|c| c.name == name).unwrap();
        assert!(c.app.is_none(), "{name} is already open");
        c.app = Some(render::open(
            rt,
            Opening {
                size,
                dir: c.dir.clone(),
                store: c.store.clone(),
                reconnect: Some(reconnects()),
                render,
                focused,
                out,
                prefix: format!("{name}-"),
            },
        ));
        c.synced = Instant::now();
    }

    fn pump(&mut self) {
        let now = Instant::now();
        for c in &mut self.clients {
            if let Some(app) = &mut c.app {
                app.cx.advance_clock((now - c.synced).min(MAX_TICK));
                app.settle();
            }
            c.synced = now;
        }
    }

    fn dump(&mut self, name: &str) -> Value {
        self.pump();
        let client = self.client(name);
        let marks = Value::Object(client.marks.clone());
        let app = client.app.as_mut().expect("open");
        let shell = app.shell.clone();
        let mut dump = app
            .cx
            .update_window(app.window, |_, window, cx| {
                automation::dump(&shell, window, cx)
            })
            .unwrap();
        dump["marks"] = marks;
        dump
    }

    /// Fills `{code:<email>}` (the Lobby's inbox) or `{code2:<email>}` (the second Lobby's) with
    /// the newest Magic Code mailed there since the last one a step took, waiting for it when it
    /// has not arrived.
    fn codes(&mut self, text: &str) -> Result<String, String> {
        let (start, open, second) = match (text.find("{code:"), text.find("{code2:")) {
            (Some(start), _) => (start, "{code:", false),
            (None, Some(start)) => (start, "{code2:", true),
            (None, None) => return Ok(text.to_owned()),
        };
        let end = start + text[start..].find('}').ok_or("unclosed {code:…}")?;
        let email = &text[start + open.len()..end];
        let mail = match (&self.server, &self.second) {
            (_, Some(lobby2)) if second => &lobby2.mail,
            (Server::Running(server), _) if !second => &server.mail,
            _ => return Err(format!("{open}…}} needs its Lobby running")),
        };
        let inbox = (second, email.to_owned());
        let seen = self.codes_taken.get(&inbox).copied().unwrap_or(0);
        let (count, code) = self
            .rt
            .block_on(async {
                tokio::time::timeout(CODE_WITHIN, mail.code_after(email, seen)).await
            })
            .map_err(|_| format!("no new code was mailed to {email}"))?;
        self.codes_taken.insert(inbox, count);
        Ok(format!("{}{code}{}", &text[..start], &text[end + 1..]))
    }

    fn wait(&mut self, on: &str, pred: &Predicate, timeout: Duration) -> Result<(), String> {
        let until = Instant::now() + timeout;
        loop {
            let dump = self.dump(on);
            if scenario::holds(pred, &dump) {
                return Ok(());
            }
            if Instant::now() >= until {
                self.save_dump(&format!("{on}-failed"), &dump);
                return Err(format!(
                    "not within {timeout:?}: {}",
                    scenario::explain(pred, &dump)
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    fn save_dump(&self, name: &str, dump: &Value) {
        let _ = std::fs::write(
            self.out.join(format!("{name}.json")),
            serde_json::to_string_pretty(dump).unwrap(),
        );
    }

    fn window_step(&mut self, on: &str, step: WindowStep) -> Result<(), String> {
        self.pump();
        let app = self
            .client(on)
            .app
            .as_mut()
            .ok_or_else(|| format!("{on} is closed"))?;
        let shell = app.shell.clone();
        let r = app
            .cx
            .update_window(app.window, |_, window, cx| {
                automation::perform(&shell, &step, window, cx)
            })
            .map_err(|e| e.to_string())?;
        app.settle();
        r.map_err(|e| e.to_string())
    }

    fn server(&mut self, op: ServerOp) -> Result<(), String> {
        let rt = &self.rt;
        let server = std::mem::replace(&mut self.server, Server::Gone);
        self.server = match (op, server) {
            (ServerOp::Stop, Server::Running(s)) => {
                // A closed port refused at once, not after Windows' 2 s.
                self.proxy.set_refusing(true);
                self.paths.values().for_each(|p| p.set_refusing(true));
                let (report, stopped) = rt.block_on(s.stop());
                if !report.is_clean() {
                    return Err(format!("unclean stop: {report:?}"));
                }
                Server::Stopped(Box::new(stopped))
            }
            (ServerOp::Start, Server::Stopped(s)) => {
                let s = rt.block_on(s.start());
                self.proxy.set_refusing(false);
                self.paths.values().for_each(|p| p.set_refusing(false));
                Server::Running(Box::new(s))
            }
            (ServerOp::Restart, Server::Running(s)) => {
                let (report, s) = rt.block_on(s.restart());
                if !report.is_clean() {
                    return Err(format!("unclean restart: {report:?}"));
                }
                Server::Running(Box::new(s))
            }
            (ServerOp::Freeze, s) => {
                self.proxy.freeze_existing();
                self.paths.values().for_each(ChaosProxy::freeze_existing);
                s
            }
            (ServerOp::Thaw, s) => {
                self.proxy.set_blackhole(false);
                self.paths.values().for_each(|p| p.set_blackhole(false));
                s
            }
            (op, s) => {
                self.server = s;
                return Err(format!("server {op:?} does not fit its state"));
            }
        };
        Ok(())
    }

    fn close(&mut self, name: &str) -> Result<(), String> {
        self.window_step(name, WindowStep::Action("lobby::Quit".into()))?;
        let until = Instant::now() + CLOSE_WITHIN;
        while self.client(name).app.as_ref().unwrap().exits.get() == 0 {
            if Instant::now() >= until {
                return Err(format!("{name} did not quit within {CLOSE_WITHIN:?}"));
            }
            std::thread::sleep(POLL);
            self.pump();
        }
        self.client(name).app = None;
        Ok(())
    }

    fn run_step(&mut self, on: &str, step: &Step) -> Result<Option<String>, String> {
        match step {
            Step::Click(id) => self.window_step(on, WindowStep::Click(id.clone()))?,
            Step::MoveTo(id) => self.window_step(on, WindowStep::MoveTo(id.clone()))?,
            Step::RightClick(id) => self.window_step(on, WindowStep::RightClick(id.clone()))?,
            Step::Type { text, into } => {
                let text = self.codes(text)?;
                self.window_step(
                    on,
                    WindowStep::Type {
                        text,
                        into: into.clone(),
                    },
                )?
            }
            Step::Press(key) => self.window_step(on, WindowStep::Press(key.clone()))?,
            Step::Scroll { list, to } => self.window_step(
                on,
                WindowStep::Scroll {
                    list: list.clone(),
                    to: to.clone(),
                },
            )?,
            Step::Wheel { id, notches } => self.window_step(
                on,
                WindowStep::Wheel {
                    id: id.clone(),
                    notches: *notches,
                },
            )?,
            Step::Action(name) => self.window_step(on, WindowStep::Action(name.clone()))?,
            Step::Drag { id, by } => self.window_step(
                on,
                WindowStep::Drag {
                    id: id.clone(),
                    by: *by,
                },
            )?,
            Step::Pen(pen) => self.window_step(on, WindowStep::Pen(*pen))?,
            Step::Wait { pred, timeout } => self.wait(on, pred, *timeout)?,
            Step::Expect(pred) => {
                let dump = self.dump(on);
                if !scenario::holds(pred, &dump) {
                    self.save_dump(&format!("{on}-failed"), &dump);
                    return Err(scenario::explain(pred, &dump));
                }
            }
            Step::Shot { name, id, hover } => {
                self.pump();
                let app = self.client(on).app.as_mut().ok_or("closed")?;
                if !hover {
                    app.cx
                        .update_window(app.window, |_, window, cx| {
                            automation::rest_pointer(window, cx)
                        })
                        .map_err(|e| e.to_string())?;
                }
                return match app.try_shot(name, id.as_deref()) {
                    Ok(path) => Ok(Some(path.display().to_string())),
                    Err(e) if e.starts_with("no renderer") => Ok(Some(format!("skipped: {e}"))),
                    Err(e) => Err(e),
                };
            }
            Step::Dump(name) => {
                let dump = self.dump(on);
                self.save_dump(&format!("{on}-{name}"), &dump);
            }
            Step::Server(op) => self.server(*op)?,
            Step::Open(name) => self.open(name),
            Step::Close(name) => self.close(name)?,
            Step::Corrupt(name) => {
                let c = self.client(name);
                if c.app.is_some() {
                    return Err(format!("close {name} before corrupting its state.json"));
                }
                std::fs::write(c.dir.path().join("state.json"), "{ not json")
                    .map_err(|e| e.to_string())?;
            }
            Step::Mark(mark) => {
                let mut dump = self.dump(on);
                dump.as_object_mut()
                    .expect("the dump is an object")
                    .remove("marks");
                self.client(on).marks.insert(mark.clone(), dump);
            }
            Step::Note(_) => {}
            Step::Network(up) => {
                let path = self
                    .paths
                    .get(on)
                    .ok_or_else(|| format!("{on} has no own path (header \"own_paths\")"))?;
                path.set_refusing(!*up);
                if !*up {
                    path.cut_all();
                }
            }
            Step::Host { picks, save_dir } => {
                let picks = picks
                    .iter()
                    .map(|list| list.iter().map(|p| scenario::fixture(p)).collect())
                    .collect();
                let save_dir = save_dir.as_deref().map(scenario::fixture);
                let app = self.client(on).app.as_mut().ok_or("closed")?;
                app.cx
                    .update(|cx| automation::script_host(picks, save_dir, cx))?;
            }
            Step::Drop { id, paths } => self.window_step(
                on,
                WindowStep::Drop {
                    id: id.clone(),
                    paths: paths.iter().map(|p| scenario::fixture(p)).collect(),
                },
            )?,
            Step::Paste { id, content } => {
                let item = match content {
                    PasteContent::Image(path) => {
                        automation::clipboard_image(&scenario::fixture(path))?
                    }
                    PasteContent::Files(paths) => automation::clipboard_files(
                        &paths
                            .iter()
                            .map(|p| scenario::fixture(p))
                            .collect::<Vec<_>>(),
                    ),
                };
                self.window_step(
                    on,
                    WindowStep::Paste {
                        id: id.clone(),
                        item,
                    },
                )?
            }
            Step::Advance(by) => {
                self.pump();
                let app = self.client(on).app.as_mut().ok_or("closed")?;
                app.cx.advance_clock(*by);
                app.settle();
            }
        }
        Ok(None)
    }

    fn log(&mut self, entry: Value) {
        let _ = writeln!(self.transcript, "{entry}");
    }

    fn finish(mut self) -> Result<(), String> {
        let open: Vec<String> = self
            .clients
            .iter()
            .filter(|c| c.app.is_some())
            .map(|c| c.name.clone())
            .collect();
        for name in open {
            self.close(&name)?;
        }
        let rt = &self.rt;
        if let Server::Running(s) = std::mem::replace(&mut self.server, Server::Gone) {
            let report = rt.block_on(s.shutdown());
            if !report.is_clean() {
                return Err(format!("{}: unclean shutdown {report:?}", self.name));
            }
        }
        if let Some(second) = self.second.take() {
            let report = rt.block_on(second.shutdown());
            if !report.is_clean() {
                return Err(format!("{}: second Lobby unclean {report:?}", self.name));
            }
        }
        Ok(())
    }
}

/// Each a Message claiming one file, stored as `MESSAGE_SEND` would; the restart lets the Lobby's
/// sequence see them.
async fn send_files(server: TestServer, files: &[scenario::SentFile]) -> TestServer {
    for (i, f) in files.iter().enumerate() {
        let author = server.user_id_of(&f.author).await;
        let bytes = std::fs::read(scenario::fixture(&f.file)).expect("a sent file exists");
        let nonce = format!("file-{i:06}-nonce-0000");
        server
            .seed_file_message(&author, &bytes, &f.name, "", &nonce)
            .await;
    }
    let (report, server) = server.restart().await;
    assert!(report.is_clean(), "{report:?}");
    server
}

fn run(path: &Path, text: &str) -> Result<Duration, String> {
    let name = name_of(path);
    let s = scenario::parse(&name, text).map_err(|e| format!("{name}: {e}"))?;
    if s.header.drive_only {
        eprintln!("scenario {name}: skipped here (drive_only: cargo xtask drive {name})");
        return Ok(Duration::ZERO);
    }
    let mut world = World::start(&s);
    let s = scenario::parse(&name, &world.fill(text)).map_err(|e| format!("{name}: {e}"))?;
    for line in &s.steps {
        let t = Instant::now();
        let result = world.run_step(&line.on, &line.step);
        let ms = t.elapsed().as_millis() as u64;
        let at = world.started.elapsed().as_millis() as u64;
        match result {
            Ok(said) => world.log(json!({
                "at_ms": at, "line": line.number, "on": line.on,
                "step": format!("{:?}", line.step), "ok": true, "ms": ms, "said": said,
            })),
            Err(e) => {
                world.log(json!({
                    "at_ms": at, "line": line.number, "on": line.on,
                    "step": format!("{:?}", line.step), "ok": false, "ms": ms, "error": e,
                }));
                let elapsed = world.started.elapsed();
                let _ = world.finish();
                return Err(format!(
                    "{name}:{}: {:?} on {} failed after {elapsed:?}: {e}",
                    line.number, line.step, line.on
                ));
            }
        }
    }
    let elapsed = world.started.elapsed();
    world.finish()?;
    Ok(elapsed)
}

/// A scenario that starts a Screen Share encodes on the GPU, so it is the only kind that takes
/// turns with the other GPU tests (`.config/nextest.toml`).
fn shares_screen(text: &str) -> bool {
    text.contains(r#""click": "share-screen""#)
}

#[test]
fn scenarios() {
    play_scenarios(false);
}

#[test]
fn share_scenarios() {
    play_scenarios(true);
}

fn play_scenarios(sharing: bool) {
    let filter = std::env::var("LOBBY_SCENARIO").ok();
    let matching: Vec<(PathBuf, String)> = load_all()
        .into_iter()
        .filter(|(p, _)| {
            filter
                .as_ref()
                .is_none_or(|f| name_of(p).contains(f.as_str()))
        })
        .collect();
    assert!(!matching.is_empty(), "no scenario matched");
    let all: Vec<(PathBuf, String)> = matching
        .into_iter()
        .filter(|(_, text)| shares_screen(text) == sharing)
        .collect();
    if all.is_empty() {
        return;
    }
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, 4);
    let queue = std::sync::Mutex::new(all.iter().collect::<Vec<_>>());
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let Some((path, text)) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(path, text)))
                            .unwrap_or_else(|p| {
                                Err(format!(
                                    "{}: panicked: {}",
                                    name_of(path),
                                    p.downcast_ref::<String>()
                                        .map(String::as_str)
                                        .or(p.downcast_ref::<&str>().copied())
                                        .unwrap_or("?")
                                ))
                            });
                    results.lock().unwrap().push((name_of(path), outcome));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    let mut failed = Vec::new();
    for (name, outcome) in &results {
        match outcome {
            Ok(took) => eprintln!("scenario {name}: ok in {took:.1?}"),
            Err(e) => {
                eprintln!("scenario {name}: FAILED: {e}");
                failed.push(e.clone());
            }
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} scenarios failed (transcripts in target/ui-shots/scenarios/):\n{}",
        failed.len(),
        results.len(),
        failed.join("\n")
    );
}

#[test]
fn every_scenario_parses() {
    for (path, text) in load_all() {
        scenario::parse(&name_of(&path), &text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

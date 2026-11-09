//! The scenario language: one JSON object per line, read by the UI tests
//! (`tests/e2e_ui.rs`) and by `cargo xtask drive` through the automation channel, so one file
//! proves a step in process and against the real binaries. Pure: parsing and predicates only.
//!
//! The first line may be a header: `{"scenario": "...", "clients": ["a", "b"], "members":
//! ["alice", "bob"]}`, and optionally `heartbeat_ms`, `second_lobby`, `lobby_name` (the first
//! Lobby's name in place of "Test Lobby"), `size`, `bootstrap_email`,
//! `join_requests`, `drive_only`, `visible` (the driver opens the windows on screen, never
//! focused), `layer3_only` (why `cargo xtask drive` refuses it), `focused` (the UI tests'
//! windows are active, as the person's is while they use it; the driver never focuses), `history`
//! (`{"author": "bob", "count": 120}`: Messages stored before the server starts), `sent_files`
//! (`[{"author": "bob", "file": "photo-640x480.png", "name": "..\\..\\x.png"}]`: Messages with
//! one attachment each, stored before the clients open under a name no client could give) and
//! `own_paths` (clients that reach the Lobby through their own proxy, so `network` can cut one of
//! them alone). Every other line is one step, optionally with `"on": "<client>"` (the client it
//! acts on; later steps keep it). Blank lines and lines that start with `//` are skipped.
//!
//! The steps:
//! - `{"click": "<id>"}`: a left click in the middle of the element. Drive first waits up to a
//!   second for an element that is moving (sliding in) to hold still.
//! - `{"move_to": "<id>"}`: the pointer moved to the middle of the element, and a frame drawn. It
//!   stays there until the next step moves it (a `shot` takes it out of the window unless it has
//!   `"hover": true`).
//! - `{"right_click": "<id>"}`: the right button pressed and released in the middle of the element.
//!   It opens the element's menu, whose items are elements to click (`menu-copy`, `menu-open`…).
//!   Copy in a menu is recorded in the model's `/clipboard` (the last text), never written.
//! - `{"type": "text", "into": "<id>"}`: `into` is clicked first; then the text goes to the focus.
//! - `{"press": "ctrl-a"}`: one keystroke, GPUI's syntax.
//! - `{"scroll": "<list>", "to": "<id>"}`: the wheel over `list` until `id` is visible.
//! - `{"wheel": "<id>", "notches": -20}`: the wheel turned over the middle of `id` that many
//!   notches of three lines, a frame drawn after each: positive up, negative down.
//! - `{"drag": "<id>", "by": [dx, dy]}`: the left button pressed in the middle of the element,
//!   moved by `dx`, `dy` and released.
//! - `{"pen": "down", "at": [x, y]}`, `{"pen": "move", "at": [x, y]}` and `{"pen": "up"}`: the
//!   left button on the share's ink canvas (`ink-canvas`, over the picture only), each its own
//!   step so a stroke can span steps and clock advances: pressed at a point, moved there held, and
//!   released where the pointer is. `x` and `y` run from 0 to 1 over the picture; from -0.5 to 0
//!   and 1 to 1.5 they fall off it, on the letterbox or past it. Down and move fail where no canvas
//!   is drawn; whether the press drew is for the model to say.
//! - `{"action": "<name>"}`: a GPUI action (`lobby::Quit`) or a host event: `idle`, `active`,
//!   `wake`, `tray:show`, `tray:quit`, `window:close` (the close button: hides to the tray),
//!   `open-link:<link>` (a `lobby://` link, as the OS hands it over: the Shell opens it as it
//!   does one from a browser).
//! - `{"wait": <predicate>, "timeout_ms": 5000}`: polls the model until it holds.
//! - `{"expect": <predicate>}`: holds now.
//! - `{"shot": "<name>", "id": "<id>"}`: a 1x crop of the element (the window without `id`),
//!   drawn fresh once it holds still, the pointer out of the window (`"hover": true` keeps it).
//! - `{"dump": "<name>"}`: the model and the element snapshots as JSON.
//! - `{"server": "stop"}`, or `"start"`, `"restart"`, `"freeze"`, `"thaw"`: the Lobby's server.
//! - `{"close": "<client>"}` / `{"open": "<client>"}`: Quit flow / start again from its data.
//! - `{"corrupt": "<client>"}`: garbage into a closed client's `state.json`.
//! - `{"mark": "<name>"}`: remembers the model and where every element is, for `unchanged` and
//!   `changed_since`.
//! - `{"note": "..."}`: said in the transcript.
//! - `{"network": "down"}` or `"up"`: in-process, a client in `own_paths`: its path refuses and
//!   is cut, or works again.
//! - `{"advance": 5100}`: in-process: the client's GPUI clock jumps ahead (a deadline passes
//!   without waiting for it).
//! - `{"host": {"pick": [["a.png", "b.pdf"], …], "save_dir": "<dir>"}}`: scripts the file host
//!   (`files::FileRecorder`): each `pick` list answers one picker in turn (none left: cancelled),
//!   a Save dialog answers `save_dir/<suggested name>` (no `save_dir`: cancelled); both keys are
//!   optional and replace what was scripted.
//! - `{"drop": "<id>", "paths": ["a.pdf", …]}`: files dragged over the middle of the element and
//!   dropped (a synthetic file drop, like a click).
//! - `{"paste": "<id>", "image": "shot.png"}` or `"files": ["a.pdf", …]`: in-process only: the
//!   element is clicked, the test platform's clipboard gets the picture's bytes (or the file
//!   list) and ctrl-v is pressed. The pipe refuses it: it never writes the person's clipboard.
//!
//! The paths of `host`, `drop` and `paste` are absolute or relative to `tests/scenarios/fixtures/`
//! (`fixture`). Open and Show in folder are recorded in the model's `/files` (`prompts`,
//! `opened`, `revealed`, `saved`), never done.
//!
//! Predicates read the dump (`model::dump`): `{"path": "/route", "eq": "Lobby"}` with `eq` (a
//! missing path is `null`), `ne` (a missing path fails it), `gt` (a number above it), `contains`
//! (substring; array element matching a partial object; object key; partial object), `exists`,
//! `len`, `count` + `where` (array elements matching a partial object), `changed_since` (a mark:
//! the value differs from the one at the mark);
//! `{"visible": "<id>"}`, `{"gone": "<id>"}`; `{"unchanged": "<id>", "since": "<mark>"}` (the
//! element has not moved or changed size since the mark); `{"inside": "<id>", "of": "<id>"}`
//! (the element lies wholly within the other); and `all`, `any`, `not`.
//!
//! Placeholders, filled before parsing: `{lobby}` and `{lobby2}` (the Lobbies' addresses),
//! `{host}` and `{host2}` (the same without `http://`, for a `lobby://` link),
//! `{lobby:<client>}` (an `own_paths` client's own address), `{tmp}` (a directory of the run's
//! own, for a Save dialog's `save_dir`) and `{big:<n>}` (a file of `n` MiB in it, made for the
//! run); `{code:<email>}` in a `type` is the newest Magic Code the Lobby mailed there, and
//! `{code2:<email>}` the second Lobby's. `cargo xtask drive` fills `{lobby}`, `{lobby2}`, `{host}`,
//! `{host2}` and the codes only. A Member `alice` of the header's `members` has the email
//! `alice@lobby.test`.

use std::time::Duration;

use serde_json::{Map, Value};

pub const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub header: Header,
    pub steps: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Header {
    pub clients: Vec<String>,
    pub members: Vec<String>,
    /// Tighter server timings: the WebSocket heartbeat (stale paths are found in 2.5 beats).
    pub heartbeat_ms: Option<u64>,
    pub second_lobby: bool,
    /// The first Lobby's name, in place of "Test Lobby".
    pub lobby_name: Option<String>,
    pub size: Option<(f32, f32)>,
    pub bootstrap_email: Option<String>,
    pub join_requests: Option<bool>,
    /// Only `cargo xtask drive` can run it (real windows: `resize:`); the UI tests skip it.
    pub drive_only: bool,
    /// `cargo xtask drive` opens its windows on screen, never focused, as `--visible` does: what
    /// it checks needs them there (a Lobby window on the monitor it shares).
    pub visible: bool,
    /// The UI tests activate each client's window. By default it is not, and the OS toast logic
    /// takes it as away; what depends on the Member being in front of the window needs this.
    pub focused: bool,
    /// Why only the UI tests can run it (it leans on the tests' short timings); the driver refuses
    /// it up front with this reason.
    pub layer3_only: Option<String>,
    pub history: Option<(String, usize)>,
    pub sent_files: Vec<SentFile>,
    pub own_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentFile {
    pub author: String,
    pub file: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub number: usize,
    pub on: String,
    pub step: Step,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Click(String),
    MoveTo(String),
    RightClick(String),
    Type {
        text: String,
        into: Option<String>,
    },
    Press(String),
    Scroll {
        list: String,
        to: String,
    },
    Wheel {
        id: String,
        notches: i32,
    },
    Action(String),
    Drag {
        id: String,
        by: (f32, f32),
    },
    Pen(PenStep),
    Wait {
        pred: Predicate,
        timeout: Duration,
    },
    Expect(Predicate),
    /// `hover` keeps the pointer where the last step left it. By default it leaves the window
    /// first, so hover styles never pass for state.
    Shot {
        name: String,
        id: Option<String>,
        hover: bool,
    },
    Dump(String),
    Server(ServerOp),
    Open(String),
    Close(String),
    Corrupt(String),
    Mark(String),
    Note(String),
    Network(bool),
    Advance(Duration),
    Host {
        picks: Vec<Vec<String>>,
        save_dir: Option<String>,
    },
    Drop {
        id: String,
        paths: Vec<String>,
    },
    Paste {
        id: String,
        content: PasteContent,
    },
}

/// Where on the share's picture the pen is, from 0 to 1 each way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenStep {
    Down(f32, f32),
    Move(f32, f32),
    Up,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PasteContent {
    Image(String),
    Files(Vec<String>),
}

pub fn fixture(path: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(path);
    if path.is_absolute() {
        return path.to_owned();
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/scenarios/fixtures")
        .join(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOp {
    Stop,
    Start,
    Restart,
    /// The clients' existing connections stop moving (a pulled cable); new ones pass.
    Freeze,
    Thaw,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Path { path: String, test: Test },
    Visible(String),
    Gone(String),
    Unchanged { id: String, mark: String },
    Inside { id: String, of: String },
    All(Vec<Predicate>),
    Any(Vec<Predicate>),
    Not(Box<Predicate>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Test {
    Eq(Value),
    Ne(Value),
    Gt(f64),
    Contains(Value),
    Exists(bool),
    Len(usize),
    Count { filter: Value, n: usize },
    ChangedSince(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

fn err(line: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        line,
        message: message.into(),
    }
}

pub fn parse(name: &str, text: &str) -> Result<Scenario, ParseError> {
    let mut header = None;
    let mut steps = Vec::new();
    let mut on = None::<String>;
    for (ix, raw) in text.lines().enumerate() {
        let number = ix + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let value: Value =
            serde_json::from_str(trimmed).map_err(|e| err(number, format!("not JSON: {e}")))?;
        let Value::Object(mut obj) = value else {
            return Err(err(number, "a step is a JSON object"));
        };
        if obj.contains_key("scenario") {
            if header.is_some() || !steps.is_empty() {
                return Err(err(number, "the header must be the first line"));
            }
            header = Some(parse_header(number, &obj)?);
            continue;
        }
        if let Some(target) = obj.remove("on") {
            let target = target
                .as_str()
                .ok_or_else(|| err(number, "\"on\" names a client"))?;
            on = Some(target.to_owned());
        }
        let step = parse_step(number, obj)?;
        let header = header.get_or_insert_with(Header::default);
        if header.clients.is_empty() {
            header.clients.push("a".into());
        }
        let target = on.clone().unwrap_or_else(|| header.clients[0].clone());
        if !header.clients.contains(&target) {
            return Err(err(number, format!("unknown client {target:?}")));
        }
        for named in step.clients() {
            if !header.clients.iter().any(|c| c == named) {
                return Err(err(number, format!("unknown client {named:?}")));
            }
        }
        steps.push(Line {
            number,
            on: target,
            step,
        });
    }
    let mut header = header.unwrap_or_default();
    if header.clients.is_empty() {
        header.clients.push("a".into());
    }
    Ok(Scenario {
        name: name.to_owned(),
        header,
        steps,
    })
}

fn strings(number: usize, value: Option<&Value>, key: &str) -> Result<Vec<String>, ParseError> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| err(number, format!("{key:?} lists strings")))
            })
            .collect(),
        Some(_) => Err(err(number, format!("{key:?} is an array"))),
    }
}

fn parse_header(number: usize, obj: &Map<String, Value>) -> Result<Header, ParseError> {
    for key in obj.keys() {
        if ![
            "scenario",
            "clients",
            "members",
            "heartbeat_ms",
            "second_lobby",
            "lobby_name",
            "size",
            "bootstrap_email",
            "join_requests",
            "drive_only",
            "visible",
            "focused",
            "layer3_only",
            "history",
            "sent_files",
            "own_paths",
        ]
        .contains(&key.as_str())
        {
            return Err(err(number, format!("unknown header key {key:?}")));
        }
    }
    let clients = strings(number, obj.get("clients"), "clients")?;
    let mut seen = std::collections::HashSet::new();
    for c in &clients {
        let ok = !c.is_empty()
            && c.len() <= 16
            && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ok || !seen.insert(c) {
            return Err(err(number, format!("bad or repeated client name {c:?}")));
        }
    }
    Ok(Header {
        members: strings(number, obj.get("members"), "members")?,
        heartbeat_ms: obj.get("heartbeat_ms").and_then(Value::as_u64),
        second_lobby: obj
            .get("second_lobby")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        lobby_name: obj
            .get("lobby_name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        size: match obj.get("size") {
            None => None,
            Some(Value::Array(wh)) if wh.len() == 2 => match (wh[0].as_f64(), wh[1].as_f64()) {
                (Some(w), Some(h)) if w >= 1. && h >= 1. => Some((w as f32, h as f32)),
                _ => return Err(err(number, "size is [width, height]")),
            },
            Some(_) => return Err(err(number, "size is [width, height]")),
        },
        bootstrap_email: obj
            .get("bootstrap_email")
            .and_then(Value::as_str)
            .map(str::to_owned),
        join_requests: obj.get("join_requests").and_then(Value::as_bool),
        drive_only: obj.get("drive_only").and_then(Value::as_bool) == Some(true),
        visible: obj.get("visible").and_then(Value::as_bool) == Some(true),
        focused: obj.get("focused").and_then(Value::as_bool) == Some(true),
        layer3_only: obj
            .get("layer3_only")
            .and_then(Value::as_str)
            .map(str::to_owned),
        history: match obj.get("history") {
            None => None,
            Some(h) => match (
                h.get("author").and_then(Value::as_str),
                h.get("count").and_then(Value::as_u64),
            ) {
                (Some(author), Some(count)) => Some((author.to_owned(), count as usize)),
                _ => return Err(err(number, "history is {\"author\": …, \"count\": …}")),
            },
        },
        sent_files: match obj.get("sent_files") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|f| {
                    let field = |k: &str| f.get(k).and_then(Value::as_str).map(str::to_owned);
                    match (field("author"), field("file"), field("name")) {
                        (Some(author), Some(file), Some(name)) => {
                            Ok(SentFile { author, file, name })
                        }
                        _ => Err(err(
                            number,
                            "sent_files lists {\"author\": …, \"file\": …, \"name\": …}",
                        )),
                    }
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(err(number, "sent_files is an array")),
        },
        own_paths: {
            let paths = strings(number, obj.get("own_paths"), "own_paths")?;
            if let Some(p) = paths.iter().find(|p| !clients.contains(p)) {
                return Err(err(
                    number,
                    format!("own_paths names an unknown client {p:?}"),
                ));
            }
            paths
        },
        clients,
    })
}

fn string_of(number: usize, v: &Value, what: &str) -> Result<String, ParseError> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| err(number, format!("{what} is a string")))
}

const VERBS: &[&str] = &[
    "click",
    "move_to",
    "right_click",
    "type",
    "press",
    "scroll",
    "wheel",
    "drag",
    "action",
    "wait",
    "expect",
    "shot",
    "dump",
    "server",
    "open",
    "close",
    "corrupt",
    "mark",
    "note",
    "network",
    "advance",
    "host",
    "drop",
    "paste",
    "pen",
];

pub fn parse_step_value(value: Value) -> Result<Step, ParseError> {
    match value {
        Value::Object(obj) => parse_step(1, obj),
        _ => Err(err(1, "a step is a JSON object")),
    }
}

fn parse_step(number: usize, mut obj: Map<String, Value>) -> Result<Step, ParseError> {
    let verbs: Vec<&str> = VERBS
        .iter()
        .copied()
        .filter(|v| obj.contains_key(*v))
        .collect();
    let [verb] = verbs[..] else {
        return Err(err(
            number,
            format!("a step has exactly one of {VERBS:?}, found {verbs:?}"),
        ));
    };
    let value = obj.remove(verb).expect("present");
    let allowed: &[&str] = match verb {
        "type" => &["into"],
        "scroll" => &["to"],
        "wheel" => &["notches"],
        "drag" => &["by"],
        "wait" => &["timeout_ms"],
        "shot" => &["id", "hover"],
        "drop" => &["paths"],
        "paste" => &["image", "files"],
        "pen" => &["at"],
        _ => &[],
    };
    if let Some(extra) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(err(number, format!("unknown key {extra:?} for {verb:?}")));
    }
    Ok(match verb {
        "click" => Step::Click(string_of(number, &value, "click")?),
        "move_to" => Step::MoveTo(string_of(number, &value, "move_to")?),
        "right_click" => Step::RightClick(string_of(number, &value, "right_click")?),
        "type" => Step::Type {
            text: string_of(number, &value, "type")?,
            into: obj
                .get("into")
                .map(|v| string_of(number, v, "into"))
                .transpose()?,
        },
        "press" => Step::Press(string_of(number, &value, "press")?),
        "scroll" => Step::Scroll {
            list: string_of(number, &value, "scroll")?,
            to: obj
                .get("to")
                .ok_or_else(|| {
                    err(
                        number,
                        "scroll needs \"to\": the element to bring into view",
                    )
                })
                .and_then(|v| string_of(number, v, "to"))?,
        },
        "wheel" => Step::Wheel {
            id: string_of(number, &value, "wheel")?,
            notches: obj
                .get("notches")
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| err(number, "wheel needs \"notches\": a whole number"))?,
        },
        "action" => Step::Action(string_of(number, &value, "action")?),
        "wait" => Step::Wait {
            pred: parse_predicate(number, &value)?,
            timeout: obj
                .get("timeout_ms")
                .map(|v| {
                    v.as_u64()
                        .map(Duration::from_millis)
                        .ok_or_else(|| err(number, "timeout_ms is a number"))
                })
                .transpose()?
                .unwrap_or(WAIT_TIMEOUT),
        },
        "expect" => Step::Expect(parse_predicate(number, &value)?),
        "shot" => Step::Shot {
            name: string_of(number, &value, "shot")?,
            id: obj
                .get("id")
                .map(|v| string_of(number, v, "id"))
                .transpose()?,
            hover: match obj.get("hover") {
                None => false,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| err(number, "hover is true or false"))?,
            },
        },
        "dump" => Step::Dump(string_of(number, &value, "dump")?),
        "server" => Step::Server(match value.as_str() {
            Some("stop") => ServerOp::Stop,
            Some("start") => ServerOp::Start,
            Some("restart") => ServerOp::Restart,
            Some("freeze") => ServerOp::Freeze,
            Some("thaw") => ServerOp::Thaw,
            _ => {
                return Err(err(
                    number,
                    "server is one of stop, start, restart, freeze, thaw",
                ));
            }
        }),
        "open" => Step::Open(string_of(number, &value, "open")?),
        "close" => Step::Close(string_of(number, &value, "close")?),
        "corrupt" => Step::Corrupt(string_of(number, &value, "corrupt")?),
        "mark" => Step::Mark(string_of(number, &value, "mark")?),
        "note" => Step::Note(string_of(number, &value, "note")?),
        "network" => Step::Network(match value.as_str() {
            Some("up") => true,
            Some("down") => false,
            _ => return Err(err(number, "network is up or down")),
        }),
        "drag" => Step::Drag {
            id: string_of(number, &value, "drag")?,
            by: match obj.get("by") {
                Some(Value::Array(d)) if d.len() == 2 => match (d[0].as_f64(), d[1].as_f64()) {
                    (Some(dx), Some(dy)) => (dx as f32, dy as f32),
                    _ => return Err(err(number, "by is [dx, dy]")),
                },
                _ => return Err(err(number, "drag needs \"by\": [dx, dy]")),
            },
        },
        "pen" => Step::Pen(pen_step(number, &value, obj.get("at"))?),
        "advance" => Step::Advance(Duration::from_millis(
            value
                .as_u64()
                .ok_or_else(|| err(number, "advance is a number of milliseconds"))?,
        )),
        "host" => {
            let Value::Object(host) = &value else {
                return Err(err(number, "host is {\"pick\": …, \"save_dir\": …}"));
            };
            if let Some(extra) = host
                .keys()
                .find(|k| !["pick", "save_dir"].contains(&k.as_str()))
            {
                return Err(err(number, format!("unknown key {extra:?} for \"host\"")));
            }
            let picks = match host.get("pick") {
                None => Vec::new(),
                Some(Value::Array(lists)) => lists
                    .iter()
                    .map(|list| match list {
                        Value::Array(_) => strings(number, Some(list), "pick"),
                        _ => Err(err(number, "pick is a list of path lists")),
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err(err(number, "pick is a list of path lists")),
            };
            Step::Host {
                picks,
                save_dir: host
                    .get("save_dir")
                    .map(|v| string_of(number, v, "save_dir"))
                    .transpose()?,
            }
        }
        "drop" => Step::Drop {
            id: string_of(number, &value, "drop")?,
            paths: match strings(number, obj.get("paths"), "paths")? {
                paths if paths.is_empty() => {
                    return Err(err(number, "drop needs \"paths\": the files dropped"));
                }
                paths => paths,
            },
        },
        "paste" => Step::Paste {
            id: string_of(number, &value, "paste")?,
            content: match (obj.get("image"), obj.contains_key("files")) {
                (Some(image), false) => PasteContent::Image(string_of(number, image, "image")?),
                (None, true) => PasteContent::Files(strings(number, obj.get("files"), "files")?),
                _ => return Err(err(number, "paste has exactly one of \"image\", \"files\"")),
            },
        },
        _ => unreachable!("listed in VERBS"),
    })
}

/// How far off the picture a pen step may point, as a share of the picture.
const PEN_REACH: std::ops::RangeInclusive<f64> = -0.5..=1.5;

fn pen_step(number: usize, verb: &Value, at: Option<&Value>) -> Result<PenStep, ParseError> {
    let point = || match at {
        Some(Value::Array(p)) if p.len() == 2 => match (p[0].as_f64(), p[1].as_f64()) {
            (Some(x), Some(y)) if PEN_REACH.contains(&x) && PEN_REACH.contains(&y) => {
                Ok((x as f32, y as f32))
            }
            _ => Err(err(number, "at is [x, y], each from -0.5 to 1.5")),
        },
        _ => Err(err(number, "pen down and move need \"at\": [x, y]")),
    };
    match verb.as_str() {
        Some("down") => point().map(|(x, y)| PenStep::Down(x, y)),
        Some("move") => point().map(|(x, y)| PenStep::Move(x, y)),
        Some("up") if at.is_none() => Ok(PenStep::Up),
        Some("up") => Err(err(number, "pen up takes no \"at\"")),
        _ => Err(err(number, "pen is one of down, move, up")),
    }
}

pub fn parse_predicate(number: usize, v: &Value) -> Result<Predicate, ParseError> {
    let Value::Object(obj) = v else {
        return Err(err(number, "a predicate is an object"));
    };
    let list = |key: &str| -> Result<Vec<Predicate>, ParseError> {
        match obj.get(key) {
            Some(Value::Array(items)) => items.iter().map(|p| parse_predicate(number, p)).collect(),
            _ => Err(err(number, format!("{key:?} lists predicates"))),
        }
    };
    if obj.contains_key("all") {
        return Ok(Predicate::All(list("all")?));
    }
    if obj.contains_key("any") {
        return Ok(Predicate::Any(list("any")?));
    }
    if let Some(inner) = obj.get("not") {
        return Ok(Predicate::Not(Box::new(parse_predicate(number, inner)?)));
    }
    if let Some(id) = obj.get("visible") {
        return Ok(Predicate::Visible(string_of(number, id, "visible")?));
    }
    if let Some(id) = obj.get("gone") {
        return Ok(Predicate::Gone(string_of(number, id, "gone")?));
    }
    if let Some(id) = obj.get("unchanged") {
        let mark = obj
            .get("since")
            .ok_or_else(|| err(number, "unchanged needs \"since\": a mark"))?;
        return Ok(Predicate::Unchanged {
            id: string_of(number, id, "unchanged")?,
            mark: string_of(number, mark, "since")?,
        });
    }
    if let Some(id) = obj.get("inside") {
        let of = obj
            .get("of")
            .ok_or_else(|| err(number, "inside needs \"of\": an element"))?;
        return Ok(Predicate::Inside {
            id: string_of(number, id, "inside")?,
            of: string_of(number, of, "of")?,
        });
    }
    let path = obj
        .get("path")
        .ok_or_else(|| {
            err(
                number,
                "a predicate has path, visible, gone, inside, all, any or not",
            )
        })
        .and_then(|p| string_of(number, p, "path"))?;
    if !path.is_empty() && !path.starts_with('/') {
        return Err(err(number, "a path is a JSON pointer: \"/lobby/members\""));
    }
    let test = if let Some(v) = obj.get("eq") {
        Test::Eq(v.clone())
    } else if let Some(v) = obj.get("ne") {
        Test::Ne(v.clone())
    } else if let Some(v) = obj.get("gt") {
        Test::Gt(v.as_f64().ok_or_else(|| err(number, "gt is a number"))?)
    } else if let Some(v) = obj.get("contains") {
        Test::Contains(v.clone())
    } else if let Some(v) = obj.get("exists") {
        Test::Exists(
            v.as_bool()
                .ok_or_else(|| err(number, "exists is true or false"))?,
        )
    } else if let Some(v) = obj.get("len") {
        Test::Len(v.as_u64().ok_or_else(|| err(number, "len is a number"))? as usize)
    } else if let Some(v) = obj.get("count") {
        Test::Count {
            filter: obj
                .get("where")
                .cloned()
                .unwrap_or(Value::Object(Map::new())),
            n: v.as_u64().ok_or_else(|| err(number, "count is a number"))? as usize,
        }
    } else if let Some(v) = obj.get("changed_since") {
        Test::ChangedSince(string_of(number, v, "changed_since")?)
    } else {
        return Err(err(
            number,
            "a path predicate has eq, ne, gt, contains, exists, len, count or changed_since",
        ));
    };
    Ok(Predicate::Path { path, test })
}

impl Step {
    fn clients(&self) -> Vec<&str> {
        match self {
            Step::Open(c) | Step::Close(c) | Step::Corrupt(c) => vec![c.as_str()],
            _ => Vec::new(),
        }
    }
}

/// `small` is a part of `big`: scalars and arrays are equal, and every key of `small` matches in
/// `big`. Strings must be equal.
pub fn matches_partial(big: &Value, small: &Value) -> bool {
    match (big, small) {
        (Value::Object(b), Value::Object(s)) => s
            .iter()
            .all(|(k, sv)| b.get(k).is_some_and(|bv| matches_partial(bv, sv))),
        (b, s) => b == s,
    }
}

pub fn holds(pred: &Predicate, dump: &Value) -> bool {
    match pred {
        Predicate::All(ps) => ps.iter().all(|p| holds(p, dump)),
        Predicate::Any(ps) => ps.iter().any(|p| holds(p, dump)),
        Predicate::Not(p) => !holds(p, dump),
        Predicate::Visible(id) => element_visible(dump, id),
        Predicate::Gone(id) => !element_visible(dump, id),
        Predicate::Unchanged { id, mark } => {
            let now = dump.pointer(&format!("/elements/{id}/bounds"));
            let then = dump.pointer(&format!("/marks/{mark}/elements/{id}/bounds"));
            match (
                now.and_then(Value::as_array),
                then.and_then(Value::as_array),
            ) {
                (Some(a), Some(b)) if a.len() == b.len() => {
                    a.iter()
                        .zip(b)
                        .all(|(x, y)| match (x.as_f64(), y.as_f64()) {
                            (Some(x), Some(y)) => (x - y).abs() < 0.5,
                            _ => false,
                        })
                }
                _ => false,
            }
        }
        Predicate::Inside { id, of } => match (bounds_of(dump, id), bounds_of(dump, of)) {
            (Some([x, y, w, h]), Some([ox, oy, ow, oh])) => {
                x >= ox - 0.5 && y >= oy - 0.5 && x + w <= ox + ow + 0.5 && y + h <= oy + oh + 0.5
            }
            _ => false,
        },
        Predicate::Path { path, test } => {
            let found = dump.pointer(path);
            match test {
                Test::Exists(want) => found.is_some_and(|v| !v.is_null()) == *want,
                Test::Eq(want) => found.unwrap_or(&Value::Null) == want,
                Test::Ne(want) => found.is_some_and(|v| v != want),
                Test::Gt(n) => found.and_then(Value::as_f64).is_some_and(|v| v > *n),
                Test::Len(n) => match found {
                    Some(Value::Array(a)) => a.len() == *n,
                    Some(Value::Object(o)) => o.len() == *n,
                    Some(Value::String(s)) => s.chars().count() == *n,
                    _ => false,
                },
                Test::Contains(want) => match (found, want) {
                    (Some(Value::String(s)), Value::String(w)) => s.contains(w.as_str()),
                    (Some(Value::Array(items)), w) => items.iter().any(|i| matches_partial(i, w)),
                    (Some(Value::Object(o)), Value::String(k)) => o.contains_key(k),
                    (Some(found @ Value::Object(_)), w @ Value::Object(_)) => {
                        matches_partial(found, w)
                    }
                    _ => false,
                },
                Test::Count { filter, n } => match found {
                    Some(Value::Array(items)) => {
                        items.iter().filter(|i| matches_partial(i, filter)).count() == *n
                    }
                    Some(Value::Object(o)) => {
                        o.values().filter(|i| matches_partial(i, filter)).count() == *n
                    }
                    _ => *n == 0,
                },
                Test::ChangedSince(mark) => dump
                    .pointer(&format!("/marks/{mark}"))
                    .is_some_and(|then| then.pointer(path) != found),
            }
        }
    }
}

/// An element's `[x, y, width, height]` in the dump.
fn bounds_of(dump: &Value, id: &str) -> Option<[f64; 4]> {
    let b = dump
        .pointer(&format!("/elements/{id}/bounds"))?
        .as_array()?;
    Some([
        b.first()?.as_f64()?,
        b.get(1)?.as_f64()?,
        b.get(2)?.as_f64()?,
        b.get(3)?.as_f64()?,
    ])
}

fn element_visible(dump: &Value, id: &str) -> bool {
    dump.pointer("/elements")
        .and_then(|e| e.get(id))
        .and_then(|e| e.get("visible"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn explain(pred: &Predicate, dump: &Value) -> String {
    match pred {
        Predicate::All(ps) | Predicate::Any(ps) => ps
            .iter()
            .filter(|p| !holds(p, dump))
            .map(|p| explain(p, dump))
            .collect::<Vec<_>>()
            .join("; "),
        Predicate::Not(p) => format!("not ({p:?}) holds"),
        Predicate::Unchanged { id, mark } => format!(
            "{id:?} is at {} now and was at {} at {mark:?}",
            dump.pointer(&format!("/elements/{id}/bounds"))
                .map_or("nothing".to_owned(), Value::to_string),
            dump.pointer(&format!("/marks/{mark}/elements/{id}/bounds"))
                .map_or("nothing".to_owned(), Value::to_string),
        ),
        Predicate::Inside { id, of } => format!(
            "{id:?} is at {} and {of:?} at {}",
            dump.pointer(&format!("/elements/{id}/bounds"))
                .map_or("nothing".to_owned(), Value::to_string),
            dump.pointer(&format!("/elements/{of}/bounds"))
                .map_or("nothing".to_owned(), Value::to_string),
        ),
        Predicate::Visible(id) | Predicate::Gone(id) => format!(
            "element {id:?} is {}",
            dump.pointer("/elements")
                .and_then(|e| e.get(id))
                .map_or("not drawn".to_owned(), |e| e.to_string())
        ),
        Predicate::Path { path, test } => {
            let found = dump.pointer(path).map_or("nothing".to_owned(), |v| {
                let s = v.to_string();
                if s.len() > 400 {
                    format!("{}…", &s[..s.floor_char_boundary(400)])
                } else {
                    s
                }
            });
            format!("{path} is {found}, wanted {test:?}")
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn mistakes_are_refused_with_their_line() {
        for (text, line) in [
            ("{\"click\": \"x\", \"press\": \"y\"}", 1),
            ("\n{\"wait\": {\"path\": \"route\", \"eq\": 1}}", 2),
            ("{\"click\": \"x\", \"into\": \"y\"}", 1),
            ("{\"on\": \"z\", \"click\": \"x\"}", 1),
            ("{\"click\": \"x\"}\n{\"scenario\": \"late\"}", 2),
            ("{\"server\": \"explode\"}", 1),
            ("[1]", 1),
            ("{\"scenario\": \"s\", \"clients\": [\"a\", \"a\"]}", 1),
            ("{\"pen\": \"down\"}", 1),
            ("{\"pen\": \"move\", \"at\": [0.5, 2]}", 1),
            ("{\"pen\": \"up\", \"at\": [0.5, 0.5]}", 1),
            ("{\"pen\": \"jump\"}", 1),
        ] {
            let e = parse("bad", text).unwrap_err();
            assert_eq!(e.line, line, "{text}: {e}");
        }
    }
}

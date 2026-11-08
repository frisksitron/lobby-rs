//! `state.json`: the Lobbies on this device and everything else the client keeps except tokens.
//! The types are plain data; [`StateFile`] does the blocking file IO, which
//! the desktop runs off the UI thread (and synchronously on quit).

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use lobby_protocol::{BlobId, LobbyInfo, LobbyInstanceId, MessageId};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{Issue, IssueKind, LobbyOrigin, Paths, Settings, atomic, copy};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Persisted {
    pub lobbies: Vec<Lobby>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active: Option<LobbyOrigin>,
    /// The member sidebar's width as the person left it, in whole design pixels (what they are
    /// at a UI scale of 100%); clamped to the window when shown. `None` is the default width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<u32>,
    /// The member sidebar was put away. It comes back at `sidebar_width`. Layout, not a setting:
    /// it belongs to this window.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidebar_collapsed: bool,
    /// Where the main window was left. `None` is the default, centered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<SavedWindow>,
    #[serde(default, skip_serializing_if = "Settings::is_default")]
    pub settings: Settings,
    /// What was typed in each Lobby's composer and not sent, so a switch or a restart keeps it.
    /// Never holds an empty text.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unsent: BTreeMap<LobbyOrigin, String>,
    /// The newest Message each Lobby's Member has had in front of them, so the feed can mark what
    /// arrived since on the next start or switch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: BTreeMap<LobbyOrigin, MessageId>,
}

/// The window's normal (not maximized) rectangle in logical pixels, as GPUI reports it, with
/// whether it was maximized and the display it was on. Nothing validates it here: the desktop
/// checks it against the displays it finds at the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub maximized: bool,
    /// The display's stable id, not the number GPUI hands out for one run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

/// A Lobby this device has reached, keyed by its origin: what is known before any Sign
/// In, so the switcher is drawn, the address prefilled and a restored Lobby named without the
/// network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lobby {
    pub origin: LobbyOrigin,
    pub instance_id: LobbyInstanceId,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<BlobId>,
    pub upload_max_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upserted {
    Same,
    Added,
    Updated,
    /// The origin now answers with another `instanceId`: the Lobby was reset.
    Reset,
}

impl Upserted {
    pub fn changed(self) -> bool {
        self != Upserted::Same
    }
}

impl Persisted {
    pub fn upsert(&mut self, origin: &LobbyOrigin, info: &LobbyInfo) -> Upserted {
        let entry = Lobby {
            origin: origin.clone(),
            instance_id: info.instance_id.clone(),
            name: info.name.clone(),
            icon: info.icon.clone(),
            upload_max_bytes: info.upload_max_bytes,
        };
        match self.lobbies.iter_mut().find(|l| &l.origin == origin) {
            Some(known) if *known == entry => Upserted::Same,
            Some(known) => {
                let reset = known.instance_id != entry.instance_id;
                *known = entry;
                if reset {
                    // Message numbers start again in a reset Lobby.
                    self.seen.remove(origin);
                    Upserted::Reset
                } else {
                    Upserted::Updated
                }
            }
            None => {
                self.lobbies.push(entry);
                Upserted::Added
            }
        }
    }

    pub fn lobby(&self, origin: &LobbyOrigin) -> Option<&Lobby> {
        self.lobbies.iter().find(|l| &l.origin == origin)
    }

    /// A Lobby the Member left: it is no longer on this device, and not where it restarts.
    /// `false` if it was not saved.
    pub fn remove(&mut self, origin: &LobbyOrigin) -> bool {
        let before = self.lobbies.len();
        self.lobbies.retain(|l| &l.origin != origin);
        self.unsent.remove(origin);
        self.seen.remove(origin);
        if self.last_active.as_ref() == Some(origin) {
            self.last_active = None;
        }
        self.lobbies.len() != before
    }

    pub fn restore_target(&self) -> Option<&Lobby> {
        self.last_active
            .as_ref()
            .and_then(|origin| self.lobby(origin))
            .or_else(|| self.lobbies.first())
    }

    /// A hand-edited file may list an origin twice; the first entry wins.
    fn normalize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.lobbies.retain(|l| seen.insert(l.origin.clone()));
        self.settings.normalize();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("state file was not valid and was moved to {}: {cause}", moved_to.display())]
    Corrupt {
        moved_to: PathBuf,
        #[source]
        cause: serde_json::Error,
    },
    #[error("cannot read the state file: {0}")]
    Read(#[source] io::Error),
    #[error("cannot write the state file: {0}")]
    Write(#[source] io::Error),
}

impl PersistError {
    /// The notification for the person. None of them expire by themselves: they explain why the
    /// Lobbies look the way they do.
    pub fn issue(&self) -> Issue {
        let (kind, message) = match self {
            PersistError::Corrupt { moved_to, .. } => {
                let name = moved_to.file_name().map_or_else(
                    || moved_to.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                (IssueKind::StateCorrupt, copy::state_corrupt(&name))
            }
            PersistError::Read(_) => (
                IssueKind::StateReadFailed,
                copy::STATE_NOT_LOADED.to_owned(),
            ),
            PersistError::Write(_) => (
                IssueKind::StateWriteFailed,
                copy::STATE_WRITE_FAILED.to_owned(),
            ),
        };
        Issue::new(kind, message)
    }
}

/// What [`StateFile::load`] found. A problem never stops the app: it starts from `state` (the
/// defaults when the file was unreadable) and shows the problem as an Issue.
#[derive(Debug)]
pub struct Loaded {
    pub state: Persisted,
    pub problem: Option<PersistError>,
}

/// `<data_dir>/state.json`. Cloning shares the write lock, so writes from the debounce task and
/// the synchronous one on quit never interleave, and an older snapshot never replaces a newer one.
#[derive(Debug, Clone)]
pub struct StateFile {
    path: Arc<Path>,
    written: Arc<Mutex<u64>>,
}

impl StateFile {
    pub fn new(paths: &Paths) -> Self {
        Self::at(paths.state_file())
    }

    pub fn at(path: PathBuf) -> Self {
        Self {
            path: path.into(),
            written: Arc::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Blocking. A missing file is a first start; an unparsable one is moved aside to
    /// `state.json.bad-<local time>` (`state.json.bad-2026-10-25T02-29-14`, a name a person can
    /// read in the Notification Center) so the next write cannot destroy what may still be
    /// recoverable.
    pub fn load(&self) -> Loaded {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Loaded {
                    state: Persisted::default(),
                    problem: None,
                };
            }
            Err(e) => {
                warn!(path = %self.path.display(), error = %e, "cannot read the state file");
                return Loaded {
                    state: Persisted::default(),
                    problem: Some(PersistError::Read(e)),
                };
            }
        };
        let cause = match serde_json::from_str::<Persisted>(&text) {
            Ok(mut state) => {
                state.normalize();
                info!(lobbies = state.lobbies.len(), "state loaded");
                return Loaded {
                    state,
                    problem: None,
                };
            }
            Err(cause) => cause,
        };
        let moved_to = self.bad_path(chrono::Local::now().naive_local());
        let problem = match fs::rename(&self.path, &moved_to) {
            Ok(()) => PersistError::Corrupt { moved_to, cause },
            Err(e) => PersistError::Read(e),
        };
        warn!(path = %self.path.display(), error = %problem, "state file is corrupt; starting from defaults");
        Loaded {
            state: Persisted::default(),
            problem: Some(problem),
        }
    }

    /// Blocking. Writes `state` as generation `generation` (a counter the owner bumps on every
    /// change) through a temp file and a rename, so a crash leaves the old or the new file,
    /// never half of one. Returns `false` without touching the disk when this generation or a
    /// newer one is already there.
    pub fn write(&self, generation: u64, state: &Persisted) -> Result<bool, PersistError> {
        let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        if generation <= *written {
            return Ok(false);
        }
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| PersistError::Write(io::Error::other(e)))?;
        atomic::replace_blocking(&self.path, |tmp| {
            let mut file = fs::File::create(tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .map_err(PersistError::Write)?;
        *written = generation;
        info!(generation, lobbies = state.lobbies.len(), "state saved");
        Ok(true)
    }

    /// Where a corrupt file goes: its name, `.bad-`, the local time (no `:`, Windows forbids
    /// it), and a counter when that name is taken (two corrupt starts in one second).
    fn bad_path(&self, now: chrono::NaiveDateTime) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map_or_else(|| "state.json".into(), |n| n.to_string_lossy());
        let stem = format!("{name}.bad-{}", now.format("%Y-%m-%dT%H-%M-%S"));
        let mut candidate = self.path.with_file_name(&stem);
        let mut n = 2;
        while candidate.exists() {
            candidate = self.path.with_file_name(format!("{stem}-{n}"));
            n += 1;
        }
        candidate
    }
}

#[cfg(test)]
mod tests {
    use lobby_protocol::PROTOCOL_VERSION;

    use super::*;
    use crate::{HttpPolicy, Source};

    fn origin(s: &str) -> LobbyOrigin {
        LobbyOrigin::parse(s, HttpPolicy::LoopbackOnly).unwrap()
    }

    fn info(name: &str, instance: &str) -> LobbyInfo {
        LobbyInfo {
            instance_id: instance.parse().unwrap(),
            name: name.into(),
            icon: Some("blb_0123456789abcdef01234567".parse().unwrap()),
            upload_max_bytes: 10 * 1024 * 1024,
            magic_code_sends: 3,
            join_requests_enabled: true,
            set_up: true,
            owner: None,
            protocol_version: PROTOCOL_VERSION,
            server_version: "0.1.0".into(),
        }
    }

    const A: &str = "lby_aaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn a_lobby_the_member_left_is_gone_and_the_app_restarts_in_the_next_one() {
        let (one, two) = (origin("https://one.example"), origin("https://two.example"));
        let mut state = Persisted::default();
        state.upsert(&one, &info("One", A));
        state.upsert(&two, &info("Two", "lby_bbbbbbbbbbbbbbbbbbbbbbbb"));
        state.last_active = Some(one.clone());
        assert!(state.remove(&one));
        assert!(
            !state.remove(&one),
            "a second time there is nothing to remove"
        );
        assert_eq!(state.last_active, None);
        assert_eq!(state.restore_target().map(|l| &l.origin), Some(&two));
        assert!(state.remove(&two));
        assert!(state.restore_target().is_none());
    }

    #[test]
    fn an_older_snapshot_never_replaces_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = StateFile::at(dir.path().join("state.json"));
        let mut newer = Persisted::default();
        newer.upsert(&origin("https://one.example"), &info("One", A));
        assert!(file.write(2, &newer).unwrap());
        assert!(
            !file.clone().write(1, &Persisted::default()).unwrap(),
            "superseded"
        );
        assert!(
            !file.write(2, &Persisted::default()).unwrap(),
            "already written"
        );
        assert_eq!(file.load().state, newer);
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"{\"lobbies\": [ {\"origin\": 12").unwrap();
        let file = StateFile::at(path.clone());
        let loaded = file.load();
        assert_eq!(loaded.state, Persisted::default());
        let Some(problem @ PersistError::Corrupt { moved_to, .. }) = &loaded.problem else {
            panic!("{:?}", loaded.problem)
        };
        assert!(!path.exists(), "the corrupt file is no longer in the way");
        assert_eq!(moved_to.parent(), Some(dir.path()));
        let name = moved_to.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("state.json.bad-"), "{name}");
        assert_eq!(
            fs::read(moved_to).unwrap(),
            b"{\"lobbies\": [ {\"origin\": 12",
            "kept byte for byte"
        );
        let issue = problem.issue();
        assert_eq!(issue.kind, IssueKind::StateCorrupt);
        assert_eq!(issue.source(), Source::Device);
        assert!(
            issue.message.ends_with(&format!("kept as {name}.")),
            "{}",
            issue.message
        );

        assert!(file.write(1, &Persisted::default()).unwrap());
        assert!(path.exists() && moved_to.exists());
    }

    #[test]
    fn the_window_is_optional_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, br#"{"lobbies": [], "sidebar_width": 240}"#).unwrap();
        let file = StateFile::at(path.clone());
        assert_eq!(
            file.load().state.window,
            None,
            "a file from before it loads"
        );

        let window = SavedWindow {
            x: -20,
            y: 40,
            width: 900,
            height: 600,
            maximized: true,
            display: Some("2b1e9d4c-0000-4000-8000-000000000001".into()),
        };
        let state = Persisted {
            window: Some(window),
            ..Persisted::default()
        };
        assert!(file.write(1, &state).unwrap());
        assert_eq!(file.load().state, state);

        let plain = Persisted {
            window: Some(SavedWindow {
                maximized: false,
                display: None,
                ..state.window.clone().unwrap()
            }),
            ..Persisted::default()
        };
        assert!(file.write(2, &plain).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("maximized") && !text.contains("display"),
            "defaults are not written: {text}"
        );
        assert_eq!(file.load().state, plain);
    }
    #[test]
    fn a_collapsed_sidebar_is_written_only_when_it_is_and_old_files_load_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let file = StateFile::at(path.clone());

        let plain = Persisted::default();
        assert!(file.write(1, &plain).unwrap());
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("sidebar_collapsed")
        );
        assert!(!file.load().state.sidebar_collapsed);

        let collapsed = Persisted {
            sidebar_width: Some(300),
            sidebar_collapsed: true,
            ..Persisted::default()
        };
        assert!(file.write(2, &collapsed).unwrap());
        assert_eq!(
            file.load().state,
            collapsed,
            "the width it comes back at is kept"
        );
    }
}

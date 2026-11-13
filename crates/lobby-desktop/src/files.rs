//! OS file prompts and shell actions for attachments. A driven or tested app answers prompts from a
//! script and records what it was asked to open or reveal (the `FileRecorder` global): the test
//! platform hangs on prompts and panics on open and reveal, and a driven real app must never put a
//! dialog on the person's desktop.

use std::{
    cell::RefCell,
    collections::VecDeque,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui_kit::{App, Global, PathPromptOptions};
use tokio::sync::oneshot;
use tracing::warn;

#[derive(Clone, Default)]
pub struct FileRecorder(pub Rc<RefCell<FileLog>>);

impl Global for FileRecorder {}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileLog {
    pub picks: VecDeque<Vec<PathBuf>>,
    pub save_dir: Option<PathBuf>,
    pub prompts: Vec<FilePrompt>,
    pub opened: Vec<PathBuf>,
    pub revealed: Vec<PathBuf>,
    pub saved: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePrompt {
    Pick,
    Save { suggested: String },
}

/// The desktop's file dialog could not be opened (no portal on the session bus), as opposed to
/// the Member closing it. Only Linux answers it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the file dialog could not open: {0}")]
pub struct DialogFailed(String);

/// `Ok(None)`: the Member closed the dialog.
pub type Answer<T> = Result<Option<T>, DialogFailed>;

pub fn pick_files(cx: &mut App) -> oneshot::Receiver<Answer<Vec<PathBuf>>> {
    pick(true, cx)
}

pub fn pick_one(cx: &mut App) -> impl Future<Output = Answer<PathBuf>> + use<> {
    let picked = pick(false, cx);
    async move {
        let answer = picked.await.unwrap_or(Ok(None))?;
        Ok(answer.and_then(|paths| paths.into_iter().next()))
    }
}

fn pick(multiple: bool, cx: &mut App) -> oneshot::Receiver<Answer<Vec<PathBuf>>> {
    let (tx, rx) = oneshot::channel();
    if let Some(recorder) = cx.try_global::<FileRecorder>() {
        let mut log = recorder.0.borrow_mut();
        log.prompts.push(FilePrompt::Pick);
        let _ = tx.send(Ok(log.picks.pop_front()));
        return rx;
    }
    let asked = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple,
        prompt: None,
    });
    cx.spawn(async move |_| {
        let _ = tx.send(settled(asked.await, "file picker"));
    })
    .detach();
    rx
}

pub fn save_path(suggested: &str, cx: &mut App) -> oneshot::Receiver<Answer<PathBuf>> {
    let (tx, rx) = oneshot::channel();
    if let Some(recorder) = cx.try_global::<FileRecorder>() {
        let mut log = recorder.0.borrow_mut();
        log.prompts.push(FilePrompt::Save {
            suggested: suggested.to_owned(),
        });
        let answer = log.save_dir.as_ref().map(|dir| dir.join(suggested));
        if let Some(path) = &answer {
            log.saved.push(path.clone());
        }
        let _ = tx.send(Ok(answer));
        return rx;
    }
    let asked = cx.prompt_for_new_path(&start_dir(), Some(suggested));
    cx.spawn(async move |_| {
        let _ = tx.send(settled(asked.await, "save dialog"));
    })
    .detach();
    rx
}

/// What a paste means when it is nothing but `file://` URIs, one per line, as a file manager's
/// copy writes them (`text/uri-list`: GPUI hands it over as text on Linux; Windows hands over
/// paths). `None` when any line is anything else, so such a paste stays text. A list's `#`
/// comment line counts as anything else: GPUI gives only the text, in which it cannot be told
/// from a Markdown heading.
#[cfg(target_os = "linux")]
pub fn pasted_paths(text: &str) -> Option<Vec<PathBuf>> {
    let paths = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let uri = url::Url::parse(line).ok()?;
            uri.to_file_path().ok()
        })
        .collect::<Option<Vec<_>>>()?;
    (!paths.is_empty()).then_some(paths)
}

pub fn open_path(path: &Path, cx: &mut App) {
    if let Some(recorder) = cx.try_global::<FileRecorder>() {
        recorder.0.borrow_mut().opened.push(path.to_owned());
        return;
    }
    cx.open_with_system(path);
}

pub fn reveal_path(path: &Path, cx: &mut App) {
    if let Some(recorder) = cx.try_global::<FileRecorder>() {
        recorder.0.borrow_mut().revealed.push(path.to_owned());
        return;
    }
    cx.reveal_path(path);
}

pub fn recorded(cx: &App) -> Option<FileLog> {
    cx.try_global::<FileRecorder>()
        .map(|r| r.0.borrow().clone())
}

fn settled<T, E: std::fmt::Display, C: std::fmt::Display>(
    answer: Result<Result<Option<T>, E>, C>,
    what: &str,
) -> Answer<T> {
    match answer {
        Ok(Ok(answer)) => Ok(answer),
        // A desktop with no file chooser portal is a Linux setup the Member can fix, so there
        // they are told. Elsewhere it ends as a closed dialog, as it always has.
        #[cfg(target_os = "linux")]
        Ok(Err(e)) => {
            warn!(error = %e, "the {what} could not open");
            Err(DialogFailed(e.to_string()))
        }
        #[cfg(not(target_os = "linux"))]
        Ok(Err(e)) => {
            warn!(error = %e, "the {what} could not open");
            Ok(None)
        }
        Err(e) => {
            warn!(error = %e, "the {what} went away");
            Ok(None)
        }
    }
}

fn start_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    #[cfg(target_os = "linux")]
    if let Some(dir) = xdg_download_dir(&home).filter(|dir| dir.is_dir()) {
        return dir;
    }
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

/// The desktop's download folder, which a localized desktop names its own way (`Nedlastinger`):
/// `XDG_DOWNLOAD_DIR` in `user-dirs.dirs`, as `xdg-user-dir DOWNLOAD` reads it.
#[cfg(target_os = "linux")]
fn xdg_download_dir(home: &Path) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    let value = dirs
        .lines()
        .find_map(|line| line.trim().strip_prefix("XDG_DOWNLOAD_DIR="))?
        .trim()
        .trim_matches('"');
    let dir = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    dir.is_absolute().then_some(dir)
}

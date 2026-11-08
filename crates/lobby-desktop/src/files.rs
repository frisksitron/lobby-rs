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

pub fn pick_files(cx: &mut App) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
    pick(true, cx)
}

pub fn pick_one(cx: &mut App) -> impl Future<Output = Option<PathBuf>> + use<> {
    let picked = pick(false, cx);
    async move {
        picked
            .await
            .ok()
            .flatten()
            .and_then(|paths| paths.into_iter().next())
    }
}

fn pick(multiple: bool, cx: &mut App) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
    let (tx, rx) = oneshot::channel();
    if let Some(recorder) = cx.try_global::<FileRecorder>() {
        let mut log = recorder.0.borrow_mut();
        log.prompts.push(FilePrompt::Pick);
        let _ = tx.send(log.picks.pop_front());
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

pub fn save_path(suggested: &str, cx: &mut App) -> oneshot::Receiver<Option<PathBuf>> {
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
        let _ = tx.send(answer);
        return rx;
    }
    let asked = cx.prompt_for_new_path(&start_dir(), Some(suggested));
    cx.spawn(async move |_| {
        let _ = tx.send(settled(asked.await, "save dialog"));
    })
    .detach();
    rx
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
) -> Option<T> {
    match answer {
        Ok(Ok(answer)) => answer,
        Ok(Err(e)) => {
            warn!(error = %e, "the {what} could not open");
            None
        }
        Err(e) => {
            warn!(error = %e, "the {what} went away");
            None
        }
    }
}

fn start_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

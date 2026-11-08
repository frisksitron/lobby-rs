//! Shared helpers for desktop tests. Tests that cross into real Tokio call `allow_parking()` first;
//! keep them few.
#![allow(dead_code)]

pub mod fixtures;
pub mod render;
pub mod scripted;

use std::{cell::Cell, path::Path, rc::Rc, sync::Arc};

use gpui_kit::{AnyWindowHandle, Entity, TestAppContext};
use lobby_client::{Client, HttpPolicy, MemoryTokenStore, StateFile, TokenStore as _};
use lobby_desktop::{
    Exit, Shell, ShellDeps, TokioRuntime, host::WindowHost, lobby_view::VoiceDevices,
    shell::WindowSetup,
};

pub fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

pub struct Opened {
    pub window: AnyWindowHandle,
    pub shell: Entity<Shell>,
    _dir: Rc<tempfile::TempDir>,
}

pub fn caught_up(shell: &Entity<Shell>, cx: &gpui_kit::App) -> bool {
    shell
        .read(cx)
        .session()
        .is_none_or(|s| s.read(cx).caught_up())
}

pub fn state_file(dir: &Path) -> StateFile {
    StateFile::at(dir.join("state.json"))
}

/// A first start: no Lobby saved, nothing in the token store.
pub fn open_shell(cx: &mut TestAppContext, rt: &tokio::runtime::Runtime) -> Opened {
    let dir = Rc::new(tempfile::tempdir().unwrap());
    let store = MemoryTokenStore::new();
    let client = Client::new(
        rt.handle().clone(),
        Arc::new(store.clone()),
        HttpPolicy::LoopbackOnly,
    )
    .unwrap();
    open_shell_with_client(cx, rt, client, dir, store)
}

pub fn open_shell_with_client(
    cx: &mut TestAppContext,
    rt: &tokio::runtime::Runtime,
    client: Client,
    dir: Rc<tempfile::TempDir>,
    store: MemoryTokenStore,
) -> Opened {
    let client = client.with_clock(lobby_desktop::automation::GpuiClock::of(cx.executor()));
    let deps = shell_deps(
        client,
        dir.path(),
        &store,
        WindowSetup::default(),
        Rc::new(|_, _, _| {}),
    );
    let (exit, _) = exit_counter();
    let (window, shell) = cx.update(|cx| {
        if !cx.has_global::<TokioRuntime>() {
            cx.set_global(TokioRuntime::borrowed(rt.handle().clone()));
            gpui_kit::init(cx);
            cx.set_global(lobby_desktop::links::LinkRecorder::default());
            // File prompts are scripted and Open and Show in folder recorded: the test platform
            // hangs on a prompt and panics on both.
            cx.set_global(lobby_desktop::files::FileRecorder::default());
            cx.set_global(lobby_desktop::clipboard::CopyRecorder::default());
        }
        Shell::open(deps, exit, cx).unwrap()
    });
    cx.update(|cx| shell.update(cx, |s, _| s.set_idle_source(|| None)));
    Opened {
        window,
        shell,
        _dir: dir,
    }
}

/// What `Shell::open` takes, over the state directory `dir`: the Lobbies in its `state.json`, and
/// which of them `store` holds tokens for.
pub fn shell_deps(
    client: Client,
    dir: &Path,
    store: &MemoryTokenStore,
    window: WindowSetup,
    window_host: WindowHost,
) -> ShellDeps {
    let file = state_file(dir);
    let loaded = file.load();
    let signed_in = loaded
        .state
        .lobbies
        .iter()
        .filter(|l| store.load(&l.origin).unwrap().is_some())
        .map(|l| l.origin.clone())
        .collect();
    ShellDeps {
        client,
        paths: lobby_client::Paths {
            data_dir: dir.to_owned(),
        },
        devices: VoiceDevices::Fake,
        state_file: file,
        loaded,
        signed_in,
        window,
        window_host,
        started: None,
    }
}

pub fn exit_counter() -> (Exit, Rc<Cell<u32>>) {
    let exits = Rc::new(Cell::new(0));
    let counter = exits.clone();
    (Rc::new(move |_| counter.set(counter.get() + 1)), exits)
}

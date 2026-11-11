//! `lobby`: the desktop client. The only place a Tokio runtime is built.

// Release builds are a windowed app with no console; logs go to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashSet,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{AnyWindowHandle, App, Entity, QuitMode};
use lobby_client::{
    Client, HttpPolicy, KeyringTokenStore, LobbyOrigin, MemoryTokenStore, Paths, Persisted,
    StateFile, TokenStore, update::ReleaseFeed,
};
use lobby_desktop::{
    HostEnv, Shell, ShellDeps, TokioRuntime,
    assets::LobbyAssets,
    host::{
        self, HostEvent, Tray,
        single_instance::{self, Instance},
    },
    host_env::APP_NAME,
    lobby_view::VoiceDevices,
    logging,
    shell::{StartedSession, WindowSetup},
    updates,
    window_bounds::{self, Screen},
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{error, info, warn};

const STARTUP_KEYCHAIN_BUDGET: Duration = Duration::from_secs(2);

fn main() -> anyhow::Result<()> {
    let mut clock = StartupClock::new();
    let env = HostEnv::from_env()?;
    let paths = Paths::resolve(env.instance_id.as_deref(), env.user_data_dir.as_deref())?;
    logging::init(&paths.logs_dir())?;
    info!(version = env!("CARGO_PKG_VERSION"), data_dir = %paths.data_dir.display(), "starting");

    // Built first: anything that registers with a reactor needs it. Handed to GPUI as a global.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .thread_name("lobby-tokio")
        .enable_all()
        .build()?;
    // A published build is its own setup and uninstaller. Never while driven: a drive must not
    // install anything.
    #[cfg(windows)]
    if updates::published_repo().is_some()
        && !env.automation
        && host::install::on_start(rt.handle(), updates::current_version())
            == host::install::Startup::Exit
    {
        logging::flush_and_drop();
        return Ok(());
    }
    let profile =
        KeyringTokenStore::profile(env.instance_id.as_deref(), env.user_data_dir.as_deref());
    // Before the keychain, the state file and any window: a second launch only asks the first to
    // show itself, and to open its link.
    let link = host::link_in(std::env::args_os().skip(1));
    let (host_events, host_incoming) = tokio::sync::mpsc::unbounded_channel();
    let instance = match single_instance::acquire(
        env.allow_multiple_instances,
        &single_instance::pipe_name(&profile),
        link.as_deref(),
        rt.handle(),
        host_events.clone(),
    ) {
        Instance::Signalled => {
            info!(
                link = link.is_some(),
                "another instance is running: asked it to show itself"
            );
            logging::flush_and_drop();
            return Ok(());
        }
        Instance::First(guard) => Some(guard),
        Instance::Unguarded => None,
    };
    // This launch's own link takes the same way in as a second launch's.
    if let Some(link) = link {
        let _ = host_events.send(HostEvent::OpenLink(link));
    }
    let store: Arc<dyn TokenStore> = if env.insecure_tokens {
        warn!("LOBBY_INSECURE_TOKENS=1: tokens are kept in memory only and lost on exit");
        Arc::new(MemoryTokenStore::new())
    } else {
        let store = KeyringTokenStore::new(&profile);
        info!(service = store.service(), "tokens in the OS keychain");
        Arc::new(store)
    };
    if env.allow_http {
        warn!("LOBBY_ALLOW_HTTP=1: plain http is accepted for any Lobby");
    }
    let client = Client::new(
        rt.handle().clone(),
        store,
        HttpPolicy::from_allow_http(env.allow_http),
    )?
    .with_network_watch();
    let devices = if env.fake_media {
        warn!("LOBBY_FAKE_MEDIA=1: Voice sends a 440 Hz tone and plays nothing");
        VoiceDevices::Fake
    } else {
        VoiceDevices::System
    };
    // Read synchronously so the first frame is right: the Lobbies (a corrupt file becomes defaults
    // plus an Issue) and whether the one to restore has tokens.
    let state_file = StateFile::new(&paths);
    let loaded = state_file.load();
    let signed_in = signed_in_lobbies(&rt, &client, &loaded.state);
    // Its connect runs while GPUI and the window come up, which takes a few hundred ms.
    let started = StartedSession::restore(&client, &loaded.state, &signed_in);
    let share_probe = client.probe_share();
    let mut deps = ShellDeps {
        client,
        paths: paths.clone(),
        devices,
        state_file,
        loaded,
        signed_in,
        window: WindowSetup::default(),
        window_host: host::os_window_host(),
        started,
    };

    let update_feed = updates::feed(&env);
    let update_client = deps.client.clone();
    let on_exit = updates::OnExit::default();
    let swap_on_exit = on_exit.clone();
    let driven = env.automation;

    clock.step("before GPUI");
    gpui_kit::application()
        .with_assets(LobbyAssets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx| {
            clock.step("GPUI");
            cx.set_global(TokioRuntime::owned(rt));
            cx.spawn(async move |cx| {
                let _ = share_probe.await;
                cx.update(|cx| cx.refresh_windows());
            })
            .detach();
            gpui_kit::init(cx);
            lobby_desktop::ui::init(cx);
            clock.step("gpui-kit");
            lobby_desktop::fonts::install(cx);
            clock.step("fonts");
            cx.set_app_identity(&env.aumid(), APP_NAME);
            deps.window = window_setup(&env, &deps.loaded.state, cx);
            let exit = Rc::new(|cx: &mut App| cx.quit());
            match Shell::open(deps, exit, cx) {
                Ok((window, shell)) => {
                    clock.step("main window");
                    clock.on_first_frame(window, cx);
                    host_events_for(
                        &env,
                        &shell,
                        window,
                        host_events,
                        host_incoming,
                        instance,
                        cx,
                    );
                    start_updates(update_feed, &update_client, &shell, on_exit, cx);
                    automation(&env, window, shell, cx);
                }
                Err(e) => {
                    error!(error = %e, "cannot open the main window");
                    cx.quit();
                }
            }
        });

    // The normal return path. On OS logoff `main` never gets here; Shell's `on_app_quit` flushes
    // instead.
    // A downloaded update goes in once the app has let go of everything, so the new one starts
    // clean. A driven app's exe is a dev build, which an update must not replace.
    #[cfg(windows)]
    if let Some((staged, relaunch)) = swap_on_exit.take()
        && !driven
    {
        host::install::apply_update(&staged, relaunch);
    }
    #[cfg(not(windows))]
    let _ = (swap_on_exit, driven);
    info!("exited");
    logging::flush_and_drop();
    Ok(())
}

/// Checks the release feed, when this build has one, and downloads into the exe's folder.
fn start_updates(
    feed: Option<ReleaseFeed>,
    client: &Client,
    shell: &Entity<Shell>,
    on_exit: updates::OnExit,
    cx: &mut App,
) {
    let (Some(feed), Some(dir)) = (
        feed.filter(|_| cfg!(windows)),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(ToOwned::to_owned)),
    ) else {
        return;
    };
    info!(feed = feed.manifest_url(), "checking for updates");
    let shell = shell.clone();
    let quit = Rc::new(move |cx: &mut App| shell.update(cx, |s, cx| s.quit(cx)));
    updates::AppUpdates::install(client, feed, dir, on_exit, quit, cx);
}

fn window_setup(env: &HostEnv, state: &Persisted, cx: &App) -> WindowSetup {
    if cfg!(feature = "automation") && env.automation {
        // Never the person's foreground window. GPUI places a window only on a display (and a
        // hidden one only when it is first activated), so it opens inactive where it would,
        // and `automation` moves it off screen at once unless asked (never minimized: GPUI
        // skips the frames of an iconic window). Where it ends up is not the person's to keep.
        return WindowSetup {
            focus: false,
            remember: false,
            ..WindowSetup::default()
        };
    }
    let displays = cx.displays();
    let screens: Vec<Screen> = displays
        .iter()
        .map(|d| Screen {
            area: d.visible_bounds(),
            uuid: d.uuid().ok().map(|u| u.to_string()),
        })
        .collect();
    let placement = window_bounds::place(state.window.as_ref(), &screens);
    WindowSetup {
        size: placement.size,
        origin: placement.origin,
        maximized: placement.maximized,
        display: placement.screen.map(|ix| displays[ix].id()),
        ..WindowSetup::default()
    }
}

/// Wires what the OS asks of the app from outside its window: the second launches and the
/// notification-area icon the close button hides the window to. Without an icon (no notification
/// area) the close button quits.
fn host_events_for(
    env: &HostEnv,
    shell: &Entity<Shell>,
    window: AnyWindowHandle,
    events: UnboundedSender<HostEvent>,
    incoming: UnboundedReceiver<HostEvent>,
    instance: Option<single_instance::Guard>,
    cx: &mut App,
) {
    // A driven client's icon is registered but hidden, so a drive leaves the person's tray alone.
    let shown = !(cfg!(feature = "automation") && env.automation);
    let tray = Tray::add(&env.tray_tooltip(), shown, events);
    if tray.is_none() {
        warn!("no tray icon: the close button quits");
    }
    shell.update(cx, |shell, cx| {
        if tray.is_some() {
            shell.keep_in_tray(tray);
        }
        if let Some(guard) = instance {
            shell.hold_instance(guard);
        }
        shell.listen(incoming, window, cx);
    });
}

#[cfg(all(feature = "automation", windows))]
fn automation(env: &HostEnv, window: AnyWindowHandle, shell: Entity<Shell>, cx: &mut App) {
    if !env.automation {
        return;
    }
    warn!("LOBBY_AUTOMATION=1: the app takes scenario steps from its automation pipe");
    shell.update(cx, |s, _| s.set_idle_source(|| None));
    cx.set_global(lobby_desktop::links::LinkRecorder::default());
    // OS toasts are recorded, never posted: a drive must not fill the person's Action Center.
    cx.set_global(lobby_desktop::os_toasts::OsToastRecorder::default());
    // Likewise file prompts are answered from the script and Open / Show in folder recorded.
    cx.set_global(lobby_desktop::files::FileRecorder::default());
    // Copy in a menu is recorded too: a drive must not overwrite the person's clipboard.
    cx.set_global(lobby_desktop::clipboard::CopyRecorder::default());
    let park = !env.automation_visible;
    let channel =
        lobby_desktop::automation::pipe::start(env.instance_id.as_deref(), park, shell, window, cx);
    cx.set_global(channel);
}

#[cfg(not(all(feature = "automation", windows)))]
fn automation(env: &HostEnv, _: AnyWindowHandle, _: Entity<Shell>, _: &mut App) {
    if env.automation {
        warn!("LOBBY_AUTOMATION=1 has no effect: this build has no `automation` feature");
    }
}

/// Logs each startup step's time, from process start to the first frame, so a slow launch in the
/// log says which step was slow.
struct StartupClock {
    started: Instant,
    last: Instant,
}

impl StartupClock {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last: now,
        }
    }

    fn step(&mut self, step: &'static str) {
        let now = Instant::now();
        info!(
            step,
            step_ms = (now - self.last).as_secs_f64() * 1e3,
            total_ms = (now - self.started).as_secs_f64() * 1e3,
            "startup"
        );
        self.last = now;
    }

    /// Next-frame callbacks run before that frame draws, so the second one runs once the first
    /// frame is drawn and presented.
    fn on_first_frame(mut self, window: AnyWindowHandle, cx: &mut App) {
        let _ = window.update(cx, |_, window, _| {
            window.on_next_frame(|window, _| {
                window.on_next_frame(move |_, _| self.step("first frame"));
            });
        });
    }
}

/// A keychain that hangs (a locked vault, a credential provider's prompt) must not keep the
/// window from ever appearing: past [`STARTUP_KEYCHAIN_BUDGET`] the app starts as if no Lobby had
/// tokens.
fn signed_in_lobbies(
    rt: &tokio::runtime::Runtime,
    client: &Client,
    state: &Persisted,
) -> HashSet<LobbyOrigin> {
    let checks: Vec<_> = state
        .lobbies
        .iter()
        .map(|l| (l.origin.clone(), client.has_session(&l.origin)))
        .collect();
    let all = async move {
        let mut found = HashSet::new();
        for (origin, check) in checks {
            match check.await {
                Ok(Ok(true)) => {
                    found.insert(origin);
                }
                Ok(Ok(false)) | Err(_) => {}
                Ok(Err(e)) => {
                    warn!(lobby = %origin, error = %e, "token store unavailable at startup")
                }
            }
        }
        found
    };
    // The timer is made inside the runtime: `timeout` needs its reactor when it is created.
    match rt.block_on(async { tokio::time::timeout(STARTUP_KEYCHAIN_BUDGET, all).await }) {
        Ok(found) => found,
        Err(_) => {
            warn!(budget = ?STARTUP_KEYCHAIN_BUDGET, "the token store did not answer at startup");
            HashSet::new()
        }
    }
}

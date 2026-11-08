//! `Shell`: the app-wide entity and composition root of the window.
//!
//! The routes: the Lobby URL step (its Entry reaches the Lobby, then straight into it when the
//! keychain holds tokens for it, else Sign In), Sign In (the `AuthView` over that Entry), and the
//! Lobby view, whose Member panel signs this device out. `Shell` holds the strong `SessionEntity`
//! and runs its UI effects through `subscribe_in`; the Lobby view turns Issues into notifications.
//! It turns host events into Session intents: an OS resume probes the connection, the OS idle time
//! (checked every 30 s) sets idle presence, and any input in the window ends idle at once. It also
//! owns the Quit flow and the window close hook: the close button hides the window to the tray icon
//! (where Show and Quit are), and without that icon it quits, so the process is never left with
//! no way to reach it.
//!
//! Every Lobby that answered at the URL step is saved in `Lobbies` and refreshed on READY; the last
//! active one is restored at startup. The title bar's switcher moves between them: the Lobby being
//! left keeps its tokens, so coming back needs no Sign In, and while In Voice the switch asks
//! first. "Add Lobby…" inside a Lobby is a dialog, so the person stays where they are until the new
//! Lobby answered.
//!
//! Settings is a page over whichever route the person is on (Ctrl+, or the title bar's cog): the
//! Lobby view draws it under its own title bar, the other routes under the Shell's. Esc, Back or
//! the cog returns.
//!
//! Each route change settles what the one before left running (the URL step's request, a switch's
//! token check, the Voice confirm), so a late answer never moves the person away from where they
//! went since.
//!
//! Each Lobby's own menu in the switcher acts on that Lobby wherever the window is: the open one
//! through the route's Session, any other through a short-lived errand (`lobby_menu`).

mod kept;
mod lobby_menu;
mod presence;
mod quit;
mod settings_page;
mod switch;

pub use presence::{IDLE_AFTER, IDLE_CHECK_EVERY};
pub use quit::QUIT_BUDGET;
pub use switch::SwitchTarget;

use std::{collections::HashSet, rc::Rc, time::Duration};

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, DisplayId, Entity,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Point, Render,
    SharedString, Size, Styled as _, Subscription, Task, Window, WindowBounds, WindowOptions,
    actions,
    component::{
        ActiveTheme as _, TitleBar, WindowExt as _,
        input::{InputEvent, InputState},
    },
    div,
};
use lobby_client::{
    Client, ClientTask, DeviceSetup, EntryIntent, EntryStep, EntryView, Intent, Issue, Loaded,
    LobbyOrigin, Paths, PersistError, Persisted, SessionHandle, SignedOutWhy, SoundPlayer,
    StateFile, UI_SCALE_DEFAULT, UiEffect, Updates, copy, step_ui_scale,
};
use lobby_protocol::LobbyInfo;
use tracing::{info, warn};

use crate::{
    add_lobby::AddLobbyView,
    appearance,
    auth_view::{AuthView, AuthViewEvent},
    components::{
        AddressForm, AppTitleBar, AuthScreen, BarButton, CONNECT_IDS, Feedback, LobbySwitcher,
        MediaImages, StepRail, SwitcherEntry, busy_screen,
    },
    entry_entity::{EntryChanged, EntryEntity},
    host::{self, single_instance},
    issues::IssueCenter,
    lobbies::{Lobbies, LobbiesEvent},
    lobby_icons::{LobbyIcons, icons_of},
    lobby_view::{LobbyView, LobbyViewEvent, Shared, VoiceDevices},
    session_entity::{SessionEntity, SessionUiEvent},
    settings_view::SettingsView,
    voice_shared, window_bounds,
};

actions!(lobby, [Quit, OpenSettings, ZoomIn, ZoomOut, ZoomReset]);

#[derive(Clone, Copy)]
enum Zoom {
    Bigger,
    Smaller,
    Reset,
}

/// The Wayland app id. It must match the name of
/// `packaging/linux/io.github.frisksitron.Lobby.desktop`, or the desktop shows no icon or name for
/// the window.
const APP_ID: &str = "io.github.frisksitron.Lobby";

/// Where the OS idle time comes from: the host in the app, a stand-in in tests.
pub type IdleSource = fn() -> Option<Duration>;

/// Where the window is. Each route owns its subscriptions and request, so replacing it ends them.
pub enum Route {
    LobbyUrl(UrlRoute),
    SignIn(SignInRoute),
    Lobby(LobbyRoute),
}

pub struct UrlRoute {
    /// The address step's Entry, until it reached its Lobby; dropping it aborts its request.
    entry: Option<(Entity<EntryEntity>, Subscription)>,
    /// Why the saved Lobbies could not be read: said until the person first leaves the URL step.
    pub notice: Option<SharedString>,
}

impl UrlRoute {
    pub fn busy(&self, cx: &App) -> bool {
        self.entry.as_ref().is_some_and(|(e, _)| e.read(cx).busy())
    }

    pub fn error(&self, cx: &App) -> Option<SharedString> {
        match self.entry.as_ref()?.0.read(cx).said()? {
            Feedback::Error(e) => Some(e),
            _ => None,
        }
    }

    fn view<'a>(&self, cx: &'a App) -> Option<&'a EntryView> {
        Some(self.entry.as_ref()?.0.read(cx).view())
    }
}

pub struct SignInRoute {
    pub origin: LobbyOrigin,
    pub info: LobbyInfo,
    pub auth: Entity<AuthView>,
    _auth_events: Subscription,
}

pub struct LobbyRoute {
    pub origin: LobbyOrigin,
    pub info: Option<LobbyInfo>,
    pub session: Entity<SessionEntity>,
    pub view: Entity<LobbyView>,
    /// Ended as the Quit flow starts: the closing session's effects must not reach the window.
    session_events: Option<Subscription>,
    _view_events: Subscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteName {
    LobbyUrl,
    SignIn,
    Lobby,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitState {
    Running,
    Quitting,
    /// `within_budget` is false if the session's shutdown was cut off by [`QUIT_BUDGET`].
    Done {
        within_budget: bool,
    },
}

/// Ends the GPUI run loop; the app passes `|cx| cx.quit()`. Injected because the test
/// platform's `quit()` does nothing, so a test could not otherwise see that it was called.
pub type Exit = Rc<dyn Fn(&mut App)>;

pub struct ShellDeps {
    pub client: Client,
    pub paths: Paths,
    pub devices: VoiceDevices,
    pub state_file: StateFile,
    pub loaded: Loaded,
    pub signed_in: HashSet<LobbyOrigin>,
    pub window: WindowSetup,
    /// The OS window's hide and show: [`host::os_window_host`] in the app.
    pub window_host: host::WindowHost,
    /// The restored Lobby's Session, when `main` started it before any window existed.
    pub started: Option<StartedSession>,
}

/// The Session of the Lobby a start restores. The app starts it before GPUI and the window, so
/// its connect runs while they come up.
pub struct StartedSession {
    origin: LobbyOrigin,
    session: SessionHandle,
    updates: Updates,
}

impl StartedSession {
    /// The last active Lobby connects at once at a start when it has tokens.
    pub fn restore(
        client: &Client,
        state: &Persisted,
        signed_in: &HashSet<LobbyOrigin>,
    ) -> Option<StartedSession> {
        let lobby = state.restore_target()?;
        if !lobby.origin.is_allowed(client.policy()) || !signed_in.contains(&lobby.origin) {
            return None;
        }
        let (session, updates) = client.connect(&lobby.origin);
        Some(StartedSession {
            origin: lobby.origin.clone(),
            session,
            updates,
        })
    }
}

/// The sounds are a nicety: a machine without an output thread to spare plays none.
fn sound_player(devices: VoiceDevices) -> Option<SoundPlayer> {
    if devices != VoiceDevices::System {
        return None;
    }
    SoundPlayer::start()
        .inspect_err(|e| warn!(error = %e, "UI sounds are off"))
        .ok()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowSetup {
    pub size: Size<Pixels>,
    pub origin: Option<Point<Pixels>>,
    /// Whether opening it takes the keyboard focus. The automation channel opens its windows
    /// without, so a driven client never steals the person's foreground window.
    pub focus: bool,
    pub show: bool,
    pub maximized: bool,
    /// Where `origin` is, and what centering is relative to; `None` is the primary display.
    pub display: Option<DisplayId>,
    /// Whether moving and resizing the window is kept for the next start. The automation channel
    /// parks its windows off screen and must not leave that behind.
    pub remember: bool,
}

impl Default for WindowSetup {
    fn default() -> Self {
        Self {
            size: window_bounds::DEFAULT_SIZE,
            origin: None,
            focus: true,
            show: true,
            maximized: false,
            display: None,
            remember: true,
        }
    }
}

pub struct Shell {
    client: Client,
    paths: Paths,
    exit: Exit,
    devices: VoiceDevices,
    /// The URL step's address field. The Shell's, not the route's: what it says stays for the
    /// next visit to the URL step.
    pub url: Entity<InputState>,
    pub route: Route,
    pub quit: QuitState,
    window_host: host::WindowHost,
    native: Option<host::NativeWindow>,
    /// Whether the window's close button hides it to the notification-area icon instead of
    /// quitting. Only while the icon exists: a hidden window with no icon could not be reached.
    has_tray: bool,
    hidden_to_tray: bool,
    /// Whether bringing the window back takes the focus. A driven client's never does.
    show_takes_focus: bool,
    tray: Option<host::Tray>,
    host_events: Option<Task<()>>,
    /// Answers second launches; let go as the Quit flow starts, so a launch right after Quit is
    /// the first instance and not swallowed by one on its way out.
    instance: Option<single_instance::Guard>,
    pub lobbies: Entity<Lobbies>,
    /// The saved Lobbies' icons, which the switcher draws.
    pub icons: Entity<LobbyIcons>,
    /// The Settings page, over whatever route the person is on while it is open.
    pub settings: Option<Entity<SettingsView>>,
    settings_events: Option<Subscription>,
    /// What Sign In says once the URL step reached the Lobby (a session that ended before its
    /// READY, so its `LobbyInfo` had to be fetched again).
    auth_next: Option<(LobbyOrigin, Feedback)>,
    /// A Sign Out on its way (local first, then the Lobby is told); the Quit flow awaits it.
    signing_out: Option<Task<()>>,
    switching: Option<Task<()>>,
    pub pending_switch: Option<SwitchTarget>,
    /// The Lobby a switch left, while the person is on another Lobby's Sign In: its Back goes
    /// there (it still has its tokens).
    pub back_to: Option<LobbyOrigin>,
    pub add_lobby: Option<Entity<AddLobbyView>>,
    add_events: Option<Subscription>,
    /// Another Lobby's action from its menu in the switcher, on a Session of its own.
    errand: Option<lobby_menu::Errand>,
    /// The OS file picker for a Lobby's new icon, open.
    icon_pick: Option<Task<()>>,
    pub issues: Entity<IssueCenter>,
    idle_source: IdleSource,
    /// Plays the UI sounds; none with `LOBBY_FAKE_MEDIA`, which has no speakers.
    sounds: Option<SoundPlayer>,
    _idle_check: Task<()>,
    /// Shutdowns of sessions the Shell left (NeedsAuth, Change Lobby, a switch), awaited off the
    /// UI. One task, chained, so it never grows; the Quit flow awaits it too.
    leaving: Option<Task<()>>,
    kept: kept::Kept,
    quit_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn open(
        deps: ShellDeps,
        exit: Exit,
        cx: &mut App,
    ) -> gpui_kit::Result<(AnyWindowHandle, Entity<Shell>)> {
        cx.bind_keys([
            gpui_kit::KeyBinding::new("ctrl-q", Quit, None),
            gpui_kit::KeyBinding::new("ctrl-,", OpenSettings, None),
            // Browser-style zoom. `ctrl-+` is Shift with the key that also types `=`.
            gpui_kit::KeyBinding::new("ctrl-=", ZoomIn, None),
            gpui_kit::KeyBinding::new("ctrl-+", ZoomIn, None),
            gpui_kit::KeyBinding::new("ctrl-shift-=", ZoomIn, None),
            gpui_kit::KeyBinding::new("ctrl--", ZoomOut, None),
            gpui_kit::KeyBinding::new("ctrl-0", ZoomReset, None),
            // The member sidebar's edge, which sits on the right: Left widens it.
            gpui_kit::KeyBinding::new("ctrl-alt-left", crate::lobby_view::WidenSidebar, None),
            gpui_kit::KeyBinding::new("ctrl-alt-right", crate::lobby_view::NarrowSidebar, None),
        ]);
        appearance::apply(&deps.loaded.state.settings, cx);
        voice_shared::follow_settings(&deps.loaded.state.settings, cx);
        let setup = deps.window;
        let bounds = match setup.origin {
            Some(origin) => Bounds::new(origin, setup.size),
            None => Bounds::centered(setup.display, setup.size, cx),
        };
        let options = WindowOptions {
            window_bounds: Some(if setup.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }),
            window_min_size: Some(window_bounds::MIN_SIZE),
            display_id: setup.display,
            focus: setup.focus,
            show: setup.show,
            app_id: Some(APP_ID.into()),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, move |window, cx| {
            let shell = cx.new(|cx| Shell::new(deps, exit, window, cx));
            // Without this GPUI destroys the window and, with QuitMode::Explicit, leaves a
            // windowless process holding the session.
            let weak = shell.downgrade();
            window.on_window_should_close(cx, move |_window, cx| {
                let _ = weak.update(cx, |shell, cx| shell.on_close_requested(cx));
                false
            });
            let weak = shell.downgrade();
            cx.on_action(move |_: &Quit, cx| {
                let _ = weak.update(cx, |shell, cx| shell.quit(cx));
            });
            // At the app, so the size keys work on every route and over a dialog.
            let weak = shell.downgrade();
            cx.on_action(move |_: &ZoomIn, cx| {
                let _ = weak.update(cx, |shell, cx| shell.zoom(Zoom::Bigger, cx));
            });
            let weak = shell.downgrade();
            cx.on_action(move |_: &ZoomOut, cx| {
                let _ = weak.update(cx, |shell, cx| shell.zoom(Zoom::Smaller, cx));
            });
            let weak = shell.downgrade();
            cx.on_action(move |_: &ZoomReset, cx| {
                let _ = weak.update(cx, |shell, cx| shell.zoom(Zoom::Reset, cx));
            });
            shell
        })
    }

    fn new(deps: ShellDeps, exit: Exit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ShellDeps {
            client,
            paths,
            devices,
            state_file,
            loaded: Loaded { state, problem },
            signed_in,
            window: setup,
            window_host,
            started,
        } = deps;
        let policy = client.policy();
        // Startup restore: the last active Lobby, connected at once with tokens; without, its
        // address waits in the field.
        let restore = state.restore_target().cloned();
        let initial_url = restore
            .as_ref()
            .map_or_else(String::new, |lobby| lobby.origin.to_string());
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(copy::ADDRESS_PLACEHOLDER)
                .default_value(initial_url)
        });
        url.update(cx, |input, cx| input.focus(window, cx));
        let kept = match &problem {
            Some(PersistError::Corrupt { moved_to, .. }) => Some(moved_to.clone()),
            _ => None,
        };
        let started = started.or_else(|| StartedSession::restore(&client, &state, &signed_in));
        let lobbies = cx.new(|_| Lobbies::new(state_file, state).with_startup(signed_in, kept));
        let icons = cx.new(|_| LobbyIcons::new(client.clone(), paths.clone()));
        let mut subscriptions = vec![
            cx.observe(&icons, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &url,
                window,
                |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { shift: false, .. } => this.connect(window, cx),
                    InputEvent::Change => this.address_edited(cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &lobbies,
                window,
                |this, _, ev: &LobbiesEvent, window, cx| {
                    this.on_lobbies_event(ev, window, cx);
                },
            ),
            cx.observe(&lobbies, |_, _, cx| cx.notify()),
            cx.on_app_quit(Self::on_app_quit),
            // Windows reports PBT_APMRESUMEAUTOMATIC; a socket from before the sleep is likely
            // dead, and waiting 25 s to find out would fail sends into it meanwhile.
            cx.on_system_wake({
                let shell = cx.weak_entity();
                move |cx| {
                    let _ = shell.update(cx, |shell, cx| shell.on_system_wake(cx));
                }
            }),
            // Coming back to the window is input too: idle ends now, not at the next check.
            cx.observe_window_activation(window, |shell, window, cx| {
                if window.is_window_active() {
                    shell.on_input(cx);
                }
            }),
        ];
        if setup.remember {
            subscriptions.push(cx.observe_window_bounds(window, |shell, window, cx| {
                shell.remember_window(window, cx);
            }));
        }
        let idle_check = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(IDLE_CHECK_EVERY).await;
                if this.update(cx, |shell, cx| shell.check_idle(cx)).is_err() {
                    break;
                }
            }
        });
        // The saved Lobbies could not be read: the Center keeps it, and the URL step (where the
        // person necessarily starts) says it in its reserved line.
        let issue = problem.as_ref().map(|p| p.issue());
        let startup_notice = problem.as_ref().map(|p| {
            match p {
                // Short enough for the reserved two lines; the Center's entry names the file.
                PersistError::Corrupt { .. } => copy::STATE_CORRUPT_NOTICE,
                PersistError::Read(_) | PersistError::Write(_) => copy::STATE_READ_FAILED,
            }
            .into()
        });
        let issues = cx.new(|cx| {
            let mut center = IssueCenter::new();
            if let Some(issue) = issue {
                center.raise(issue, cx);
            }
            center
        });
        let url_entry = Self::url_entry(&client, window, cx);
        let mut shell = Self {
            client,
            paths,
            exit,
            devices,
            url,
            route: Route::LobbyUrl(UrlRoute {
                entry: Some(url_entry),
                notice: startup_notice,
            }),
            quit: QuitState::Running,
            window_host,
            native: host::NativeWindow::of(window),
            has_tray: false,
            hidden_to_tray: false,
            show_takes_focus: setup.focus,
            tray: None,
            host_events: None,
            instance: None,
            lobbies,
            icons,
            settings: None,
            settings_events: None,
            auth_next: None,
            signing_out: None,
            switching: None,
            pending_switch: None,
            back_to: None,
            add_lobby: None,
            add_events: None,
            errand: None,
            icon_pick: None,
            issues,
            idle_source: host::idle_time,
            sounds: sound_player(devices),
            _idle_check: idle_check,
            leaving: None,
            kept: kept::Kept::default(),
            quit_task: None,
            _subscriptions: subscriptions,
        };
        match (restore, started) {
            (Some(lobby), Some(started)) if started.origin == lobby.origin => {
                info!(lobby = %lobby.origin, name = %lobby.name, "restoring the last active Lobby");
                shell.enter_lobby(lobby.origin, None, Some(started), window, cx);
            }
            // `state.json` is only checked structurally (a file from a `LOBBY_ALLOW_HTTP` run, or
            // edited by hand): plain http to a remote host never carries tokens. The URL step,
            // prefilled, says why when the person presses Connect.
            (Some(lobby), _) if !lobby.origin.is_allowed(policy) => {
                warn!(lobby = %lobby.origin, "the saved Lobby needs https: not restored");
            }
            (Some(lobby), _) => info!(lobby = %lobby.origin, "no tokens: the address is prefilled"),
            (None, _) => {}
        }
        shell
    }

    pub fn route_name(&self) -> RouteName {
        match self.route {
            Route::LobbyUrl(_) => RouteName::LobbyUrl,
            Route::SignIn(_) => RouteName::SignIn,
            Route::Lobby(_) => RouteName::Lobby,
        }
    }

    pub fn url_step(&self) -> Option<&UrlRoute> {
        match &self.route {
            Route::LobbyUrl(step) => Some(step),
            _ => None,
        }
    }

    pub fn session(&self) -> Option<&Entity<SessionEntity>> {
        match &self.route {
            Route::Lobby(lobby) => Some(&lobby.session),
            _ => None,
        }
    }

    pub fn lobby_view(&self) -> Option<&Entity<LobbyView>> {
        match &self.route {
            Route::Lobby(lobby) => Some(&lobby.view),
            _ => None,
        }
    }

    pub fn auth_view(&self) -> Option<&Entity<AuthView>> {
        match &self.route {
            Route::SignIn(sign_in) => Some(&sign_in.auth),
            _ => None,
        }
    }

    pub fn current_origin(&self) -> Option<&LobbyOrigin> {
        match &self.route {
            Route::LobbyUrl(_) => None,
            Route::SignIn(SignInRoute { origin, .. }) | Route::Lobby(LobbyRoute { origin, .. }) => {
                Some(origin)
            }
        }
    }

    /// Sign In for this Lobby; `notice` says why the person is here again.
    fn show_auth(
        &mut self,
        origin: LobbyOrigin,
        info: LobbyInfo,
        notice: Option<Feedback>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entering = self.client.entry_at(&origin, info);
        let entry = cx.new(|cx| EntryEntity::new(entering, cx));
        self.sign_in_with(entry, notice, window, cx);
    }

    /// Sign In with the Entry that reached its Lobby.
    pub fn sign_in_with(
        &mut self,
        entry: Entity<EntryEntity>,
        notice: Option<Feedback>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reached = entry.read(cx).view();
        let (Some(origin), Some(info)) = (reached.origin.clone(), reached.info.clone()) else {
            warn!("Sign In for an Entry that has not reached its Lobby");
            return;
        };
        self.drop_switch(window, cx);
        let back_name = {
            let lobbies = self.lobbies.read(cx);
            self.back_to
                .as_ref()
                .filter(|b| **b != origin && lobbies.is_signed_in(b))
                .map(|b| lobbies.display_name(b, None))
        };
        self.lobbies
            .update(cx, |l, cx| l.set_signed_in(&origin, false, cx));
        let mark = self.lobbies.read(cx).color_of(&origin);
        let auth = cx.new(|cx| {
            let mut auth = AuthView::new(entry, origin.clone(), info.clone(), notice, window, cx);
            auth.back_to = back_name;
            auth.mark = mark;
            auth
        });
        let auth_events = cx.subscribe_in(
            &auth,
            window,
            |shell, _, ev: &AuthViewEvent, window, cx| match ev {
                AuthViewEvent::SignedIn => {
                    info!("signed in");
                    if let Route::SignIn(sign_in) = &shell.route {
                        let (origin, info) = (sign_in.origin.clone(), sign_in.info.clone());
                        shell.enter_lobby(origin, Some(info), None, window, cx);
                    }
                }
                AuthViewEvent::ChangeLobby => shell.back_from_sign_in(window, cx),
            },
        );
        let to = Route::SignIn(SignInRoute {
            origin,
            info,
            auth,
            _auth_events: auth_events,
        });
        self.navigate(to, cx);
    }

    pub fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let address = self.url.read(cx).value().to_string();
        let Route::LobbyUrl(step) = &mut self.route else {
            return;
        };
        if self.quit != QuitState::Running || step.busy(cx) {
            return;
        }
        let entry = match &step.entry {
            Some((entry, _)) => entry.clone(),
            None => {
                let (entry, events) = Self::url_entry(&self.client, window, cx);
                step.entry = Some((entry.clone(), events));
                entry
            }
        };
        entry.update(cx, |e, cx| e.act(EntryIntent::Connect(address), cx));
        cx.notify();
    }

    /// The URL step's Entry, at the address, followed by the Shell.
    fn url_entry(
        client: &Client,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<EntryEntity>, Subscription) {
        let entry = cx.new(|cx| EntryEntity::new(client.entry(None), cx));
        let events = cx.subscribe_in(
            &entry,
            window,
            |shell, entry, _: &EntryChanged, window, cx| {
                shell.on_url_entry(entry.clone(), window, cx);
            },
        );
        (entry, events)
    }

    /// The address changed: what was said about the old one goes.
    fn address_edited(&mut self, cx: &mut Context<Self>) {
        let Route::LobbyUrl(step) = &self.route else {
            return;
        };
        let Some((entry, _)) = &step.entry else {
            return;
        };
        let view = entry.read(cx).view();
        if view.said.is_some() {
            entry.update(cx, |e, cx| e.act(EntryIntent::Edited, cx));
        }
    }

    /// The URL step's Entry moved. A Lobby that answered with our protocol is saved now, tokens
    /// or not, and becomes the one to restore.
    fn on_url_entry(
        &mut self,
        entry: Entity<EntryEntity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = entry.read(cx).view().clone();
        cx.notify();
        let (Some(origin), Some(info)) = (view.origin, view.info) else {
            return;
        };
        self.lobbies.update(cx, |lobbies, cx| {
            lobbies.upsert(&origin, &info, cx);
            lobbies.set_last_active(&origin, cx);
        });
        match view.step {
            // The tokens could not be read: the step says so.
            EntryStep::Address => {}
            EntryStep::SignedIn => {
                info!(lobby = %origin, name = %info.name, "tokens found: skipping Sign In");
                self.cancel_connect();
                self.auth_next = None;
                self.enter_lobby(origin, Some(info), None, window, cx);
            }
            _ => {
                info!(lobby = %origin, name = %info.name, "lobby info received");
                self.cancel_connect();
                // Said only for the Lobby it was about.
                let notice = match self.auth_next.take() {
                    Some((for_origin, notice)) if for_origin == origin => Some(notice),
                    _ => None,
                };
                self.sign_in_with(entry, notice, window, cx);
            }
        }
    }

    pub fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running || self.signing_out.is_some() {
            return;
        }
        let Route::Lobby(LobbyRoute {
            origin,
            info,
            session,
            ..
        }) = &self.route
        else {
            return;
        };
        let (origin, info) = (origin.clone(), info.clone());
        info!(lobby = %origin, "signing out of this device");
        let task = session.update(cx, |s, cx| s.sign_out(cx));
        self.signing_out = Some(cx.spawn_in(window, async move |this, cx| {
            let _ = task.await;
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.signing_out = None;
                shell.lobbies.update(cx, |l, cx| {
                    l.set_signed_in(&origin, false, cx);
                    // The next person to sign in on this device does not inherit the text or
                    // what the last one had seen.
                    l.set_unsent(&origin, "", cx);
                    l.forget_seen(&origin, cx);
                });
                if shell.quit == QuitState::Running {
                    let notice = Feedback::Info(copy::SIGNED_OUT.into());
                    shell.sign_in_again(origin, info, notice, window, cx);
                }
            });
        }));
        cx.notify();
    }

    /// Sign In for the Lobby the window was in. Without its `LobbyInfo` (the session ended
    /// before READY), the URL step fetches it again first and then says `notice`.
    fn sign_in_again(
        &mut self,
        origin: LobbyOrigin,
        info: Option<LobbyInfo>,
        notice: Feedback,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match info {
            Some(info) => self.show_auth(origin, info, Some(notice), window, cx),
            None => {
                self.auth_next = Some((origin.clone(), notice));
                self.show_url_step(Some(origin.as_str()), window, cx);
                self.connect(window, cx);
            }
        }
    }

    pub fn change_lobby(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.back_to = None;
        self.show_url_step(None, window, cx);
    }

    /// Sign In's first step, Back: to the Lobby the person came from when it still has its
    /// tokens (a switch or Add Lobby led here), else to the URL step with this Lobby's address,
    /// so it is clear what was left and Connect brings it back.
    pub fn back_from_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_origin().cloned();
        if let Some(previous) = self.back_to.take()
            && Some(&previous) != current.as_ref()
            && self.lobbies.read(cx).is_signed_in(&previous)
        {
            info!(lobby = %previous, "back to the Lobby this Sign In was reached from");
            self.switch_to(previous, window, cx);
            return;
        }
        let value = current.map(|o| o.to_string());
        self.show_url_step(value.as_deref(), window, cx);
    }

    /// The URL step's request is overtaken: its answer must not move the person.
    fn cancel_connect(&mut self) {
        if let Route::LobbyUrl(step) = &mut self.route {
            step.entry = None;
        }
    }

    /// A switch still waiting (its token check, or the Voice confirm) is overtaken by where the
    /// person is now: its answer must not move them again.
    fn drop_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switching = None;
        if self.pending_switch.take().is_some() {
            window.close_dialog(cx);
        }
    }

    fn show_url_step(&mut self, value: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_switch(window, cx);
        let notice = match &mut self.route {
            Route::LobbyUrl(step) => step.notice.take(),
            Route::SignIn(_) | Route::Lobby(_) => None,
        };
        let to = Route::LobbyUrl(UrlRoute {
            entry: Some(Self::url_entry(&self.client, window, cx)),
            notice,
        });
        self.navigate(to, cx);
        self.url.update(cx, |u, cx| {
            if let Some(value) = value {
                u.set_value(value.to_owned(), window, cx);
            }
            u.focus(window, cx);
        });
    }

    fn enter_lobby(
        &mut self,
        origin: LobbyOrigin,
        info: Option<LobbyInfo>,
        started: Option<StartedSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drop_switch(window, cx);
        self.back_to = None;
        self.lobbies.update(cx, |lobbies, cx| {
            lobbies.set_last_active(&origin, cx);
            lobbies.set_signed_in(&origin, true, cx);
        });
        let session = match started {
            Some(started) => cx.new(|cx| SessionEntity::new(started.session, started.updates, cx)),
            None => self.errand_session_for(&origin, cx).unwrap_or_else(|| {
                let (session, updates) = self.client.connect(&origin);
                cx.new(|cx| SessionEntity::new(session, updates, cx))
            }),
        };
        let session_events = cx.subscribe_in(
            &session,
            window,
            |shell, _, ev: &SessionUiEvent, window, cx| shell.run_ui(ev, window, cx),
        );
        let images = match self.kept.take(&origin) {
            Some(kept) => {
                kept.images.update(cx, |m, _| m.retry_failed());
                kept.images
            }
            None => {
                let media = self.client.media(&origin, &self.paths);
                cx.new(|cx| MediaImages::new(media, cx))
            }
        };
        let shared = Shared {
            issues: self.issues.clone(),
            lobbies: self.lobbies.clone(),
            icons: self.icons.clone(),
            devices: self.devices,
        };
        let view = cx.new(|cx| LobbyView::new(session.clone(), images, shared, window, cx));
        let view_events = cx.subscribe_in(
            &view,
            window,
            |shell, _, ev: &LobbyViewEvent, window, cx| match ev {
                LobbyViewEvent::ChangeLobby => shell.change_lobby(window, cx),
                LobbyViewEvent::SignOut => shell.sign_out(window, cx),
                LobbyViewEvent::OpenSettings => shell.open_settings(window, cx),
                LobbyViewEvent::LeaveSettings => shell.close_settings(window, cx),
                LobbyViewEvent::Switch(pick) => shell.pick(pick.clone(), window, cx),
            },
        );
        // Typing goes to the composer: after the route change settles, so the Sign In fields
        // leaving the tree cannot take the focus back.
        cx.defer_in(window, |shell, window, cx| {
            if shell.settings.is_some() {
                return;
            }
            if let Some(view) = shell.lobby_view().cloned() {
                view.update(cx, |v, cx| v.focus_composer(window, cx));
            }
        });
        let to = Route::Lobby(LobbyRoute {
            origin,
            info,
            session,
            view,
            session_events: Some(session_events),
            _view_events: view_events,
        });
        self.navigate(to, cx);
        self.give_page(cx);
    }

    /// The one way the route changes. A Lobby left this way also leaves Voice and closes its
    /// session in the background.
    fn navigate(&mut self, to: Route, cx: &mut Context<Self>) {
        if let Route::Lobby(left) = std::mem::replace(&mut self.route, to) {
            self.leave_session(left, cx);
        }
        self.follow_route(cx);
        cx.notify();
    }

    fn run_ui(&mut self, ev: &SessionUiEvent, window: &mut Window, cx: &mut Context<Self>) {
        for effect in &ev.0.0 {
            match effect {
                UiEffect::PersistLobby(fresh) => {
                    if let Route::Lobby(lobby) = &mut self.route {
                        lobby.info = Some((**fresh).clone());
                        let origin = lobby.origin.clone();
                        self.lobbies
                            .update(cx, |lobbies, cx| lobbies.upsert(&origin, fresh, cx));
                    }
                }
                UiEffect::Left => self.left_lobby(window, cx),
                UiEffect::NeedsAuth(why) => {
                    let Route::Lobby(lobby) = &self.route else {
                        continue;
                    };
                    let (origin, info) = (lobby.origin.clone(), lobby.info.clone());
                    info!(lobby = %origin, ?why, "session needs a new Sign In");
                    self.lobbies
                        .update(cx, |l, cx| l.set_signed_in(&origin, false, cx));
                    let notice = Feedback::Info(
                        match why {
                            SignedOutWhy::Revoked => copy::SIGNED_OUT_HERE,
                            SignedOutWhy::Removed => copy::REMOVED_FROM_LOBBY,
                            SignedOutWhy::Expired => copy::SESSION_EXPIRED,
                        }
                        .into(),
                    );
                    // The page would stay over the Sign In that says why.
                    self.close_settings(window, cx);
                    self.sign_in_again(origin, info, notice, window, cx);
                }
                UiEffect::MessageArrived(message) => {
                    crate::os_toasts::message_arrived(self, message, window, cx)
                }
                UiEffect::AccessRequested(request) => {
                    crate::os_toasts::access_requested(self, request, window, cx)
                }
                UiEffect::AccessRequestGone(id) => {
                    crate::os_toasts::access_request_gone(self, id, window, cx)
                }
                UiEffect::Sound(sound) => {
                    let settings = self.lobbies.read(cx).settings();
                    if let Some(player) = &self.sounds
                        && settings.sounds
                    {
                        let output =
                            DeviceSetup::new(settings.output_device.clone(), self.devices.source());
                        player.play(*sound, &output);
                    }
                }
                _ => {}
            }
        }
    }

    fn on_lobbies_event(&mut self, ev: &LobbiesEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            LobbiesEvent::SaveFailed(issue) => self.raise_issue(issue.clone(), window, cx),
            LobbiesEvent::SettingsChanged => {
                let settings = self.lobbies.read(cx).settings().clone();
                appearance::apply(&settings, cx);
                voice_shared::follow_settings(&settings, cx);
                cx.notify();
            }
        }
    }

    pub fn switcher_entries(&self, cx: &App) -> Vec<SwitcherEntry> {
        self.lobbies
            .read(cx)
            .switcher_entries(self.current_origin())
    }

    /// The size keys: one step of the UI scale, or back to 100%.
    fn zoom(&mut self, to: Zoom, cx: &mut Context<Self>) {
        self.lobbies.update(cx, |lobbies, cx| {
            lobbies.update_settings(
                |s| {
                    s.ui_scale = match to {
                        Zoom::Bigger => step_ui_scale(s.ui_scale, true),
                        Zoom::Smaller => step_ui_scale(s.ui_scale, false),
                        Zoom::Reset => UI_SCALE_DEFAULT,
                    }
                },
                cx,
            )
        });
    }

    /// Shuts the left Lobby's session down in the background; its tokens stay.
    fn leave_session(&mut self, left: LobbyRoute, cx: &mut Context<Self>) {
        // Its Issues were about that Lobby; this device's own stay.
        self.issues
            .update(cx, |c, cx| c.clear_lobby(&left.origin, cx));
        let images = left.view.read(cx).images.clone();
        let state = left.session.read(cx).kept_state();
        self.kept
            .keep(left.origin.clone(), kept::KeptLobby { state, images });
        let shutdown = left.session.update(cx, |s, cx| {
            s.dispatch(Intent::LeaveVoice, cx);
            s.shutdown(cx)
        });
        self.after_leaving(shutdown, cx);
    }

    /// Chains the shutdown of a Session the window let go onto [`Shell::leaving`].
    fn after_leaving(&mut self, shutdown: ClientTask<()>, cx: &mut Context<Self>) {
        let earlier = self.leaving.take();
        self.leaving = Some(cx.spawn(async move |_, _| {
            if let Some(earlier) = earlier {
                earlier.await;
            }
            let _ = shutdown.await;
        }));
    }

    /// Something the person should know: in the Lobby view a notification, elsewhere the Center
    /// keeps it.
    fn raise_issue(&mut self, issue: Issue, window: &mut Window, cx: &mut Context<Self>) {
        match self.lobby_view().cloned() {
            Some(view) => view.update(cx, |v, cx| v.notify(issue, window, cx)),
            None => self.issues.update(cx, |c, cx| c.raise(issue, cx)),
        }
    }

    fn switcher(&self, cx: &mut Context<Self>) -> LobbySwitcher {
        let lobbies = self.lobbies.read(cx);
        let current = match &self.route {
            Route::SignIn(sign_in) => {
                Some(lobbies.switcher_current(&sign_in.origin, Some(&sign_in.info.name)))
            }
            Route::LobbyUrl(_) | Route::Lobby(_) => None,
        };
        let entries = lobbies.switcher_entries(self.current_origin());
        let shell = cx.weak_entity();
        LobbySwitcher::new(
            current,
            entries,
            Rc::new(move |pick, window, cx| {
                let _ = shell.update(cx, |s, cx| s.pick(pick, window, cx));
            }),
        )
        .icons(icons_of(self.icons.clone()))
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        // Any key or click ends idle; capture runs before the views handle it. Ctrl+1…9 switch
        // Lobbies on every route.
        let input = |el: gpui_kit::Stateful<gpui_kit::Div>, cx: &mut Context<Self>| {
            el.on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| this.on_input(cx)))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.on_input(cx);
                this.on_key(ev, window, cx);
            }))
        };
        let screen = |body: gpui_kit::AnyElement, bar: AppTitleBar, cx: &mut Context<Self>| {
            input(div().id("shell"), cx)
                .size_full()
                .flex()
                .flex_col()
                .bg(background)
                .child(bar)
                .child(body)
                .into_any_element()
        };
        if self.quit != QuitState::Running {
            let current = self.current_origin().map(|o| {
                let lobbies = self.lobbies.read(cx);
                lobbies.switcher_current(o, None)
            });
            let bar = AppTitleBar::new().left(LobbySwitcher::still(current));
            let words = if crate::updates::AppUpdates::restarting(cx) {
                copy::RESTARTING
            } else {
                "Quitting…"
            };
            let body = busy_screen("quitting", words, cx).into_any_element();
            return screen(body, bar, cx);
        }
        let bar = AppTitleBar::new()
            .left(self.switcher(cx))
            .right(div().children(crate::updates::AppUpdates::bar_control(window, cx)))
            .right(BarButton::settings(self.settings.is_some(), {
                let shell = cx.weak_entity();
                move |window, cx| {
                    let _ = shell.update(cx, |s, cx| s.toggle_settings(window, cx));
                }
            }));
        if let (Some(page), false) = (&self.settings, matches!(self.route, Route::Lobby(_))) {
            return screen(page.clone().into_any_element(), bar, cx);
        }
        match &self.route {
            Route::Lobby(lobby) => input(div().id("shell").size_full(), cx)
                .child(lobby.view.clone())
                .into_any_element(),
            Route::LobbyUrl(step) => {
                let said = step.entry.as_ref().and_then(|(e, _)| e.read(cx).said());
                let line = said.or_else(|| step.notice.clone().map(Feedback::Info));
                let rail = step.view(cx).map(|v| v.rail.clone()).unwrap_or_default();
                let form = AddressForm::new(
                    &self.url,
                    CONNECT_IDS,
                    cx.listener(|this, _, window, cx| this.connect(window, cx)),
                )
                .busy(step.busy(cx))
                .line(line);
                let connect = AuthScreen::new(
                    StepRail::new(rail),
                    copy::CONNECT_TITLE,
                    copy::CONNECT_SUBTITLE,
                )
                .child(form);
                screen(connect.into_any_element(), bar, cx)
            }
            Route::SignIn(sign_in) => screen(sign_in.auth.clone().into_any_element(), bar, cx),
        }
    }
}

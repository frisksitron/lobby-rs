//! The app's updates as the window shows them. One [`AppUpdates`] entity, a GPUI global, follows
//! the client's [`Updater`]; every title bar draws its [`UpdatePill`], which cannot be dismissed. A
//! downloaded update is swapped in when the app exits: Restart to update quits and starts it,
//! and a plain Quit leaves it for the next start. `main` does the swap through [`OnExit`] once
//! GPUI has returned, so the new app finds the one-instance pipe free.

use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui_kit::{App, AppContext as _, Context, Entity, Global, Task, Window};
use lobby_client::{
    Client,
    update::{ReleaseFeed, UpdateStatus, UpdateTiming, Updater, Version},
};

use crate::{components::UpdatePill, host_env::HostEnv, scale::design_viewport};

/// Below this design width the pill says less.
const WIDE_FROM: f32 = 640.;

/// The repository a published build was built from, as CI passes it (`owner/name`). A build
/// without it, such as any local one, never installs or updates itself.
pub fn published_repo() -> Option<&'static str> {
    option_env!("LOBBY_UPDATE_REPO").filter(|repo| !repo.is_empty())
}

pub fn current_version() -> Version {
    env!("CARGO_PKG_VERSION")
        .parse()
        .expect("the crate version is major.minor.patch")
}

/// `LOBBY_UPDATE_URL`, else the published build's GitHub Releases.
pub fn feed(env: &HostEnv) -> Option<ReleaseFeed> {
    match &env.update_url {
        Some(url) => ReleaseFeed::at(url).ok(),
        None => published_repo().and_then(|repo| ReleaseFeed::github(repo).ok()),
    }
}

/// The downloaded update to swap in as the app exits, and whether to start it.
#[derive(Debug, Clone, Default)]
pub struct OnExit(Arc<Mutex<Option<(PathBuf, bool)>>>);

impl OnExit {
    fn set(&self, staged: PathBuf, relaunch: bool) {
        *self.0.lock().expect("never poisoned") = Some((staged, relaunch));
    }

    pub fn take(&self) -> Option<(PathBuf, bool)> {
        self.0.lock().expect("never poisoned").take()
    }
}

pub struct AppUpdates {
    status: UpdateStatus,
    restarting: bool,
    asked_for_newer: Cell<bool>,
    updater: Updater,
    on_exit: OnExit,
    quit: Rc<dyn Fn(&mut App)>,
    _follow: Task<()>,
}

struct UpdatesGlobal(Entity<AppUpdates>);

impl Global for UpdatesGlobal {}

impl AppUpdates {
    /// Starts checking `feed` and makes the entity the title bars find. `quit` is the Shell's
    /// Quit flow.
    pub fn install(
        client: &Client,
        feed: ReleaseFeed,
        dir: PathBuf,
        on_exit: OnExit,
        quit: Rc<dyn Fn(&mut App)>,
        cx: &mut App,
    ) {
        let updater = client.updater(feed, current_version(), dir, UpdateTiming::default());
        let entity = cx.new(|cx| {
            let mut status = updater.status();
            let follow = cx.spawn(async move |this, cx| {
                loop {
                    let now = status.borrow_and_update().clone();
                    if this
                        .update(cx, |u: &mut AppUpdates, cx| u.set(now, cx))
                        .is_err()
                    {
                        return;
                    }
                    if status.changed().await.is_err() {
                        return;
                    }
                }
            });
            AppUpdates {
                status: UpdateStatus::Current,
                restarting: false,
                asked_for_newer: Cell::new(false),
                updater,
                on_exit,
                quit,
                _follow: follow,
            }
        });
        cx.set_global(UpdatesGlobal(entity));
    }

    fn set(&mut self, status: UpdateStatus, cx: &mut Context<Self>) {
        if let UpdateStatus::Ready { file, .. } = &status {
            self.on_exit.set(file.clone(), false);
        }
        self.status = status;
        cx.notify();
    }

    /// A Lobby speaks a protocol this app does not: the update that does is probably out, so
    /// look now rather than within the hour. Once per run, since every frame asks.
    pub fn lobby_is_newer(cx: &App) {
        if let Some(global) = cx.try_global::<UpdatesGlobal>() {
            let updates = global.0.read(cx);
            if !updates.asked_for_newer.replace(true) {
                updates.updater.check_now();
            }
        }
    }

    pub fn status(&self) -> &UpdateStatus {
        &self.status
    }

    /// Setup or uninstall is replacing the app: it quits without swapping its update in.
    pub fn keep_update_staged(cx: &App) {
        if let Some(global) = cx.try_global::<UpdatesGlobal>() {
            let _ = global.0.read(cx).on_exit.take();
        }
    }

    /// Whether the app is quitting to start its update.
    pub fn restarting(cx: &App) -> bool {
        cx.try_global::<UpdatesGlobal>()
            .is_some_and(|g| g.0.read(cx).restarting)
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        let UpdateStatus::Ready { file, .. } = &self.status else {
            return;
        };
        self.on_exit.set(file.clone(), true);
        self.restarting = true;
        cx.notify();
        let quit = self.quit.clone();
        cx.defer(move |cx| quit(cx));
    }

    /// The title bar's control, when there is an update to show.
    pub fn bar_control(window: &Window, cx: &App) -> Option<UpdatePill> {
        let entity = cx.try_global::<UpdatesGlobal>()?.0.clone();
        let status = entity.read(cx).status.clone();
        if status == UpdateStatus::Current {
            return None;
        }
        let wide = design_viewport(window).0 >= WIDE_FROM;
        let (restart, retry) = (entity.clone(), entity);
        Some(UpdatePill::new(
            status,
            wide,
            Rc::new(move |cx: &mut App| restart.update(cx, |u, cx| u.restart(cx))),
            Rc::new(move |cx: &mut App| retry.read(cx).updater.check_now()),
        ))
    }
}

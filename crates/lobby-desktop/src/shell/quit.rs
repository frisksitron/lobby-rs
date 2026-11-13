//! The Quit flow, and the shutdown GPUI asks of an app that is going away without it.

use std::{future::Future, time::Duration};

use gpui_kit::Context;
use lobby_client::{ClientTask, Intent};
use tracing::{info, warn};

use super::{QuitState, Route, Shell};
use crate::logging;

/// How long the Quit flow waits for the session's shutdown. Quitting must take under two seconds
/// in all, and the Tokio runtime's own shutdown after it takes up to 250 ms.
pub const QUIT_BUDGET: Duration = Duration::from_millis(1500);
const _: () = assert!(
    QUIT_BUDGET.as_millis() + crate::runtime::DROP_SHUTDOWN.as_millis() < 2000,
    "quitting must take under two seconds in all"
);

impl Shell {
    /// The Quit flow: synchronous work first, then the session's awaited shutdown raced against
    /// [`QUIT_BUDGET`], then `cx.quit()`. Tokens are never cleared on quit. `main` flushes the log
    /// once the run loop returns.
    pub fn quit(&mut self, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running {
            return;
        }
        info!("quit requested");
        let shutdown = self.begin_quit(cx);
        let budget = cx.background_executor().timer(QUIT_BUDGET);
        self.quit_task = Some(cx.spawn(async move |this, cx| {
            let within_budget = futures_lite::future::or(
                async {
                    shutdown.await;
                    true
                },
                async {
                    budget.await;
                    false
                },
            )
            .await;
            let _ = this.update(cx, |shell, cx| {
                if !within_budget {
                    warn!("session shutdown exceeded the quit budget");
                }
                info!("quitting");
                shell.quit = QuitState::Done { within_budget };
                shell.tray = None;
                let exit = shell.exit.clone();
                exit(cx);
            });
        }));
        cx.notify();
    }

    /// The synchronous part of both quit paths: write `state.json` now, stop new work, cancel what
    /// runs and hand back the shutdowns to await (the current session's, those of sessions left
    /// earlier that are still closing, then the keychain calls in line).
    fn begin_quit(&mut self, cx: &mut Context<Self>) -> impl Future<Output = ()> + use<> {
        self.quit = QuitState::Quitting;
        self.instance = None;
        #[cfg(target_os = "linux")]
        self.cancel_reopen();
        if let Err(e) = self.lobbies.update(cx, |lobbies, _| lobbies.flush_now()) {
            warn!(error = %e, "cannot save the Lobbies on quit");
        }
        self.cancel_connect();
        self.switching = None;
        self.pending_switch = None;
        self.icon_pick = None;
        self.end_errand(cx);
        let current = match &mut self.route {
            Route::Lobby(lobby) => {
                lobby.session_events = None;
                lobby.session.update(cx, |s, cx| {
                    s.dispatch(Intent::LeaveVoice, cx);
                    s.shutdown(cx)
                })
            }
            _ => ClientTask::ready(()),
        };
        let leaving = self.leaving.take();
        let signing_out = self.signing_out.take();
        let client = self.client.clone();
        async move {
            if let Some(signing_out) = signing_out {
                signing_out.await;
            }
            let _ = current.await;
            if let Some(leaving) = leaving {
                leaving.await;
            }
            // Last, so the keychain calls the closes above made are in line before it.
            let _ = client.shutdown().await;
        }
    }

    /// GPUI runs this on every quit. After the Quit flow nothing is left to do. Otherwise this is
    /// the OS logoff path: GPUI gives the future 200 ms, then calls `std::process::exit`, so
    /// `main` never returns and the log is flushed here.
    pub(super) fn on_app_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl Future<Output = ()> + use<> {
        let (shutdown, flush) = match self.quit {
            QuitState::Done { .. } => (None, false),
            QuitState::Running => {
                info!("quitting without the Quit flow (OS logoff)");
                (Some(self.begin_quit(cx)), true)
            }
            QuitState::Quitting => (None, true),
        };
        async move {
            if let Some(shutdown) = shutdown {
                shutdown.await;
            }
            if flush {
                logging::flush_and_drop();
            }
        }
    }
}

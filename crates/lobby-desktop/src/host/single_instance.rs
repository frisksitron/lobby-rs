//! One Lobby per person and profile. The first launch serves a named pipe; a second launch finds
//! it taken, lets the first bring its window to the front and exits, before it opens a window or
//! touches `state.json` or the keychain.

#[cfg(not(windows))]
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender};

#[cfg(not(windows))]
use super::HostEvent;

#[cfg(windows)]
pub use windows_pipe::{Guard, acquire, ask_to_quit, is_running, pipe_name};

/// What a launch found.
pub enum Instance {
    /// The first: keep the guard for as long as it should answer second launches.
    First(Guard),
    /// Another one runs and has been asked to show itself; this process is done.
    Signalled,
    /// No guard: more than one instance is allowed (or the pipe could not be made), so it
    /// starts as if it were the only one.
    Unguarded,
}

#[cfg(not(windows))]
pub struct Guard;

#[cfg(not(windows))]
pub fn pipe_name(profile: &str) -> String {
    profile.to_owned()
}

#[cfg(not(windows))]
pub fn acquire(
    _allow_multiple: bool,
    _name: &str,
    _rt: &Handle,
    _events: UnboundedSender<HostEvent>,
) -> Instance {
    Instance::Unguarded
}

#[cfg(windows)]
mod windows_pipe {
    use std::{io, os::windows::io::AsRawHandle as _, time::Duration};

    use tokio::{
        io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
        net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
        runtime::Handle,
        sync::mpsc::UnboundedSender,
        time::{sleep, timeout},
    };
    use tokio_util::task::AbortOnDropHandle;
    use tracing::{info, warn};

    use super::Instance;
    use crate::host::{HostEvent, win};

    const SHOW: &str = "show";
    /// From setup or uninstall, which must replace or delete the running exe.
    const QUIT: &str = "quit";
    /// ERROR_PIPE_BUSY: every instance of the pipe is serving a client right now.
    const PIPE_BUSY: i32 = 231;
    const CONNECT_ATTEMPTS: u32 = 20;
    const CONNECT_RETRY: Duration = Duration::from_millis(50);
    /// A client that connects and says nothing must not hold the pipe.
    const MESSAGE_BUDGET: Duration = Duration::from_secs(2);
    /// Between the first launch's pipe vanishing and ours being made, another launch can win.
    const ROUNDS: u32 = 3;

    /// Answers second launches until dropped.
    pub struct Guard {
        _serve: AbortOnDropHandle<()>,
    }

    /// The pipe's name: this Windows session (a second person's, or a Remote Desktop login's, is
    /// another Lobby) and the profile `KeyringTokenStore::profile` names.
    pub fn pipe_name(profile: &str) -> String {
        format!(r"\\.\pipe\lobby-instance-{}-{profile}", win::session_id())
    }

    /// Becomes the first instance, or tells the one that runs to show itself. `events` gets
    /// [`HostEvent::Relaunched`] for each second launch.
    pub fn acquire(
        allow_multiple: bool,
        name: &str,
        rt: &Handle,
        events: UnboundedSender<HostEvent>,
    ) -> Instance {
        if allow_multiple {
            return Instance::Unguarded;
        }
        for _ in 0..ROUNDS {
            // Registering the pipe with the reactor needs the runtime to be current.
            let created = {
                let _enter = rt.enter();
                create(name, true)
            };
            match created {
                Ok(server) => {
                    let serve = rt.spawn(serve(server, name.to_owned(), events));
                    return Instance::First(Guard {
                        _serve: AbortOnDropHandle::new(serve),
                    });
                }
                // Another process made the first instance of this pipe and still holds it.
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    match rt.block_on(signal(name, SHOW)) {
                        Ok(_) => return Instance::Signalled,
                        Err(e) => info!(pipe = name, error = %e, "the running instance is gone"),
                    }
                }
                Err(e) => {
                    warn!(pipe = name, error = %e, "no single-instance pipe: starting anyway");
                    return Instance::Unguarded;
                }
            }
        }
        warn!(
            pipe = name,
            "could not tell instances apart: starting anyway"
        );
        Instance::Unguarded
    }

    fn create(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(name)
    }

    /// Asks the Lobby that serves `name` to quit. `None` when none answers, else its process id
    /// when the pipe tells it.
    pub async fn ask_to_quit(name: &str) -> Option<Option<u32>> {
        signal(name, QUIT).await.ok()
    }

    /// Whether a Lobby still serves `name`. A busy pipe is one that is there.
    pub fn is_running(name: &str) -> bool {
        match ClientOptions::new().open(name) {
            Ok(_) => true,
            Err(e) => e.raw_os_error() == Some(PIPE_BUSY),
        }
    }

    async fn signal(name: &str, message: &str) -> io::Result<Option<u32>> {
        let mut attempts = 0;
        let mut client = loop {
            match ClientOptions::new().open(name) {
                Ok(client) => break client,
                Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && attempts < CONNECT_ATTEMPTS => {
                    attempts += 1;
                    sleep(CONNECT_RETRY).await;
                }
                Err(e) => return Err(e),
            }
        };
        let pid = win::pipe_server_process(client.as_raw_handle());
        if let Some(pid) = pid {
            win::allow_foreground_for(pid);
        }
        client.write_all(format!("{message}\n").as_bytes()).await?;
        client.flush().await?;
        Ok(pid)
    }

    /// The next instance is made before a client is served, so the name never lapses for another
    /// process to take.
    async fn serve(mut server: NamedPipeServer, name: String, events: UnboundedSender<HostEvent>) {
        loop {
            let connected = server.connect().await;
            let current = server;
            server = match create(&name, false) {
                Ok(next) => next,
                Err(e) => {
                    warn!(pipe = %name, error = %e, "the single-instance pipe cannot be made again");
                    return;
                }
            };
            if connected.is_err() {
                continue;
            }
            let mut lines = BufReader::new(current).lines();
            if let Ok(Ok(Some(line))) = timeout(MESSAGE_BUDGET, lines.next_line()).await {
                let event = match line.trim() {
                    SHOW => Some(HostEvent::Relaunched),
                    QUIT => Some(HostEvent::QuitForSetup),
                    _ => None,
                };
                if let Some(event) = event {
                    let _ = events.send(event);
                }
            }
        }
    }
}

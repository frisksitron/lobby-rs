//! One Lobby per person and profile. The first launch serves a named pipe (a Unix socket on
//! Linux); a second launch finds it taken, lets the first bring its window to the front and exits,
//! before it opens a window or touches `state.json` or the keychain. A second launch that holds a
//! `lobby://` link hands the link over as well.

#[cfg(not(any(windows, target_os = "linux")))]
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender};

#[cfg(not(any(windows, target_os = "linux")))]
use super::HostEvent;

#[cfg(target_os = "linux")]
pub use unix_socket::{Guard, acquire, pipe_name};
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

#[cfg(not(any(windows, target_os = "linux")))]
pub struct Guard;

#[cfg(not(any(windows, target_os = "linux")))]
pub fn pipe_name(profile: &str) -> String {
    profile.to_owned()
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn acquire(
    _allow_multiple: bool,
    _name: &str,
    _link: Option<&str>,
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
    /// `open <link>`: show, and take the link.
    const OPEN: &str = "open ";
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

    /// Becomes the first instance, or tells the one that runs to show itself, and to open `link`
    /// when this launch holds one. `events` gets [`HostEvent::Relaunched`] for each second launch,
    /// and [`HostEvent::OpenLink`] for each link.
    pub fn acquire(
        allow_multiple: bool,
        name: &str,
        link: Option<&str>,
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
                    let message = link.map_or_else(
                        || SHOW.to_owned(),
                        // One line: a newline in an argument would end the message early.
                        |link| format!("{OPEN}{}", link.replace(['\r', '\n'], " ")),
                    );
                    match rt.block_on(signal(name, &message)) {
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
                let line = line.trim();
                let event = match line {
                    SHOW => Some(HostEvent::Relaunched),
                    QUIT => Some(HostEvent::QuitForSetup),
                    _ => line
                        .strip_prefix(OPEN)
                        .map(|link| HostEvent::OpenLink(link.to_owned())),
                };
                if let Some(event) = event {
                    let _ = events.send(event);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod unix_socket {
    use std::{
        fs, io,
        os::unix::fs::{DirBuilderExt as _, PermissionsExt as _},
        path::{Path, PathBuf},
        time::Duration,
    };

    use tokio::{
        io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
        net::{UnixListener, UnixStream},
        runtime::Handle,
        sync::mpsc::UnboundedSender,
        time::{sleep, timeout},
    };
    use tokio_util::task::AbortOnDropHandle;
    use tracing::{info, warn};

    use super::Instance;
    use crate::host::HostEvent;

    const SHOW: &str = "show";
    /// `open <link>`: show, and take the link.
    const OPEN: &str = "open ";
    const QUIT: &str = "quit";
    const DIR: &str = "lobby";
    const CONNECT_ATTEMPTS: u32 = 20;
    const CONNECT_RETRY: Duration = Duration::from_millis(50);
    /// A client that connects and says nothing must not hold the socket.
    const MESSAGE_BUDGET: Duration = Duration::from_secs(2);
    /// A launch that does not hold the lock may find the holder not yet bound, or a stale socket
    /// that the holder is about to replace; it asks again this many times.
    const ROUNDS: u32 = 10;

    /// Answers second launches until dropped, and removes its socket file. It holds the profile's
    /// lock for as long as it lives: only the holder may unlink or bind the socket.
    pub struct Guard {
        path: PathBuf,
        _lock: fs::File,
        _serve: AbortOnDropHandle<()>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // The lock is released when `_lock` closes, after the socket is gone.
            let _ = fs::remove_file(&self.path);
        }
    }

    /// The socket's file name for a profile (`KeyringTokenStore::profile`), safe as one path part.
    pub fn pipe_name(profile: &str) -> String {
        let safe: String = profile
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("instance-{safe}.sock")
    }

    /// Becomes the first instance, or tells the one that runs to show itself, and to open `link`
    /// when this launch holds one. `events` gets [`HostEvent::Relaunched`] for each second launch,
    /// and [`HostEvent::OpenLink`] for each link. Without `$XDG_RUNTIME_DIR` there is nowhere
    /// private to put the socket, so there is no guard.
    pub fn acquire(
        allow_multiple: bool,
        name: &str,
        link: Option<&str>,
        rt: &Handle,
        events: UnboundedSender<HostEvent>,
    ) -> Instance {
        if allow_multiple {
            return Instance::Unguarded;
        }
        let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
        else {
            warn!("no XDG_RUNTIME_DIR: starting without a single-instance socket");
            return Instance::Unguarded;
        };
        acquire_in(&runtime, name, link, rt, events)
    }

    fn acquire_in(
        runtime: &Path,
        name: &str,
        link: Option<&str>,
        rt: &Handle,
        events: UnboundedSender<HostEvent>,
    ) -> Instance {
        let dir = runtime.join(DIR);
        if let Err(e) = make_private_dir(&dir) {
            warn!(dir = %dir.display(), error = %e, "no single-instance socket: starting anyway");
            return Instance::Unguarded;
        }
        let path = dir.join(name);
        // The lock file is never removed: unlinking it would let two launches lock two files.
        let lock = match fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("lock"))
        {
            Ok(lock) => lock,
            Err(e) => {
                warn!(socket = %path.display(), error = %e, "no single-instance lock: starting anyway");
                return Instance::Unguarded;
            }
        };
        let message = link.map_or_else(
            || SHOW.to_owned(),
            // One line: a newline in an argument would end the message early.
            |link| format!("{OPEN}{}", link.replace(['\r', '\n'], " ")),
        );
        for round in 0..ROUNDS {
            match lock.try_lock() {
                // Whatever socket is here is a dead Lobby's: its lock went with it.
                Ok(()) => return serve_first(lock, path, rt, events),
                Err(fs::TryLockError::WouldBlock) => match rt.block_on(signal(&path, &message)) {
                    Ok(()) => return Instance::Signalled,
                    // The holder has not bound yet, or is about to replace a stale socket.
                    Err(e) if is_stale(&e) => {
                        info!(socket = %path.display(), round, "the first launch is not serving yet");
                        std::thread::sleep(CONNECT_RETRY);
                    }
                    Err(e) => {
                        warn!(socket = %path.display(), error = %e, "cannot reach the running instance: starting anyway");
                        return Instance::Unguarded;
                    }
                },
                Err(fs::TryLockError::Error(e)) => {
                    warn!(socket = %path.display(), error = %e, "no single-instance lock: starting anyway");
                    return Instance::Unguarded;
                }
            }
        }
        warn!(
            socket = %path.display(),
            "could not tell instances apart: starting anyway"
        );
        Instance::Unguarded
    }

    /// Binds the socket. The caller holds the profile's lock, so nobody else unlinks or binds.
    fn serve_first(
        lock: fs::File,
        path: PathBuf,
        rt: &Handle,
        events: UnboundedSender<HostEvent>,
    ) -> Instance {
        if fs::remove_file(&path).is_ok() {
            info!(socket = %path.display(), "removed the socket a dead Lobby left");
        }
        // Registering the socket with the reactor needs the runtime to be current.
        let bound = {
            let _enter = rt.enter();
            UnixListener::bind(&path)
        };
        match bound {
            Ok(listener) => {
                let serve = rt.spawn(serve(listener, events));
                Instance::First(Guard {
                    path,
                    _lock: lock,
                    _serve: AbortOnDropHandle::new(serve),
                })
            }
            Err(e) => {
                warn!(socket = %path.display(), error = %e, "no single-instance socket: starting anyway");
                Instance::Unguarded
            }
        }
    }

    /// The directory is the socket's only protection, so it is the person's alone.
    fn make_private_dir(dir: &Path) -> io::Result<()> {
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            }
            Err(e) => Err(e),
        }
    }

    /// A socket file nobody listens on: bound once, its Lobby gone without cleaning up.
    fn is_stale(e: &io::Error) -> bool {
        matches!(
            e.kind(),
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
        )
    }

    async fn signal(path: &Path, message: &str) -> io::Result<()> {
        let mut attempts = 0;
        let mut client = loop {
            match UnixStream::connect(path).await {
                Ok(client) => break client,
                // The first launch's backlog is full: it is serving others right now.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && attempts < CONNECT_ATTEMPTS => {
                    attempts += 1;
                    sleep(CONNECT_RETRY).await;
                }
                Err(e) => return Err(e),
            }
        };
        client.write_all(format!("{message}\n").as_bytes()).await?;
        client.flush().await
    }

    async fn serve(listener: UnixListener, events: UnboundedSender<HostEvent>) {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let mut lines = BufReader::new(stream).lines();
            if let Ok(Ok(Some(line))) = timeout(MESSAGE_BUDGET, lines.next_line()).await {
                let line = line.trim();
                let event = match line {
                    SHOW => Some(HostEvent::Relaunched),
                    QUIT => Some(HostEvent::QuitForSetup),
                    _ => line
                        .strip_prefix(OPEN)
                        .map(|link| HostEvent::OpenLink(link.to_owned())),
                };
                if let Some(event) = event {
                    let _ = events.send(event);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::host::single_instance::Instance;

        fn runtime() -> tokio::runtime::Runtime {
            // As the app's: `Handle::block_on` does not drive a current-thread runtime's tasks.
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap()
        }

        #[test]
        fn a_second_launch_hands_its_link_to_the_first_and_a_dead_ones_socket_is_replaced() {
            let rt = runtime();
            let tmp = tempdir();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let name = pipe_name("a/b");
            assert_eq!(name, "instance-a_b.sock");
            let first = acquire_in(&tmp, &name, None, rt.handle(), tx.clone());
            let Instance::First(guard) = first else {
                panic!("the first launch is the first");
            };
            let mode = fs::metadata(tmp.join(DIR)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);

            let second = acquire_in(&tmp, &name, Some("lobby://x/join"), rt.handle(), tx.clone());
            assert!(matches!(second, Instance::Signalled));
            let third = acquire_in(&tmp, &name, None, rt.handle(), tx.clone());
            assert!(matches!(third, Instance::Signalled));
            let got = rt.block_on(async { (rx.recv().await.unwrap(), rx.recv().await.unwrap()) });
            assert_eq!(
                got,
                (
                    HostEvent::OpenLink("lobby://x/join".into()),
                    HostEvent::Relaunched
                )
            );

            // kill -9 leaves the socket behind without a listener, and the kernel drops the lock.
            let path = tmp.join(DIR).join(&name);
            guard._lock.unlock().unwrap();
            std::mem::forget(guard);
            drop(rt);
            assert!(path.exists());
            let rt = runtime();
            let again = acquire_in(&tmp, &name, None, rt.handle(), tx);
            assert!(matches!(again, Instance::First(_)));
            drop(again);
            assert!(!path.exists(), "the guard removes its socket");
            fs::remove_dir_all(&tmp).unwrap();
        }

        #[test]
        fn two_launches_on_a_dead_ones_socket_make_one_first_instance() {
            // The race is a window of microseconds: many rounds make it likely to be hit.
            const RACES: usize = 20;
            let rt = runtime();
            let tmp = tempdir();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let name = pipe_name("race");
            let path = tmp.join(DIR).join(&name);
            fs::create_dir(tmp.join(DIR)).unwrap();
            for _ in 0..RACES {
                // A socket file nobody listens on, as after kill -9.
                drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
                let start = std::sync::Barrier::new(2);
                let launches: Vec<Instance> = std::thread::scope(|scope| {
                    let threads: Vec<_> = (0..2)
                        .map(|_| {
                            scope.spawn(|| {
                                start.wait();
                                acquire_in(&tmp, &name, None, rt.handle(), tx.clone())
                            })
                        })
                        .collect();
                    threads.into_iter().map(|t| t.join().unwrap()).collect()
                });
                let firsts = launches
                    .iter()
                    .filter(|launch| matches!(launch, Instance::First(_)))
                    .count();
                let signalled = launches
                    .iter()
                    .filter(|launch| matches!(launch, Instance::Signalled))
                    .count();
                assert_eq!((firsts, signalled), (1, 1));
                // The signal reached the one that serves, which is then dropped.
                assert_eq!(rt.block_on(rx.recv()), Some(HostEvent::Relaunched));
                drop(launches);
                assert!(!path.exists());
            }
            fs::remove_dir_all(&tmp).unwrap();
        }

        fn tempdir() -> PathBuf {
            let dir = std::env::temp_dir().join(format!("lobby-si-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir(&dir).unwrap();
            dir
        }
    }
}

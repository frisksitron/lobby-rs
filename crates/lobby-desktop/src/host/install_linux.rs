//! Installing and uninstalling the app for one person, with no package and no root. A published
//! build is its own setup: started from anywhere but `${XDG_DATA_HOME:-~/.local/share}/lobby`, it
//! copies itself there, adds a launcher entry with its icon, makes itself what runs a `lobby://`
//! link, and starts the copy. `lobby --uninstall` removes all of it. Updates swap the binary in
//! place ([`apply_update`]), so the folder never holds more than one app. The installed version
//! is written beside the binary, where Windows keeps it in the registry.
//!
//! A running Linux binary can be renamed and replaced, so neither install nor uninstall asks a
//! running Lobby to quit: it goes on from the file it had open.

use std::{
    env, fs, io,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use lobby_client::{
    DEEP_LINK_SCHEME,
    update::{APP_FILES, EXE, OLD, SETUP_COPY, Version, clean_up, swap_in},
};
use tokio::runtime::Handle;
use tracing::{debug, info, warn};

use super::{
    Startup,
    install_plan::{Plan, plan, uninstall_requested},
    link_in,
};
use crate::host_env::{APP_ID, APP_NAME};

const INSTALL_DIR: &str = "lobby";
const VERSION_FILE: &str = "version";
const DESCRIPTION: &str = "Chat, Voice and Screen Share with your friends";
/// The launcher's icon, the frame of `packaging/windows/lobby.ico` drawn at 256 px.
pub(super) const ICON: &[u8] = include_bytes!("../../../../packaging/linux/icons/lobby-256.png");
/// The sizes of the hicolor theme a desktop asks for. 48 and 64 are the `.ico`'s own frames. 128
/// is the 256 frame scaled down. There is no 512: the art has no frame above 256, and a
/// bigger file would be that frame scaled up.
pub(super) const ICONS: [(u32, &[u8]); 4] = [
    (
        48,
        include_bytes!("../../../../packaging/linux/icons/lobby-48.png"),
    ),
    (
        64,
        include_bytes!("../../../../packaging/linux/icons/lobby-64.png"),
    ),
    (
        128,
        include_bytes!("../../../../packaging/linux/icons/lobby-128.png"),
    ),
    (256, ICON),
];

/// Where an install puts its files.
struct Places {
    /// `${XDG_DATA_HOME:-~/.local/share}`.
    data: PathBuf,
}

impl Places {
    fn resolve() -> Option<Self> {
        let xdg = env::var_os("XDG_DATA_HOME").map(PathBuf::from);
        let data = match xdg {
            Some(dir) if dir.is_absolute() => dir,
            _ => {
                let home = PathBuf::from(env::var_os("HOME").filter(|h| !h.is_empty())?);
                home.join(".local/share")
            }
        };
        Some(Self { data })
    }

    fn dir(&self) -> PathBuf {
        self.data.join(INSTALL_DIR)
    }

    fn exe(&self) -> PathBuf {
        self.dir().join(EXE)
    }

    fn version_file(&self) -> PathBuf {
        self.dir().join(VERSION_FILE)
    }

    fn applications(&self) -> PathBuf {
        self.data.join("applications")
    }

    fn desktop_file(&self) -> PathBuf {
        self.applications().join(format!("{APP_ID}.desktop"))
    }

    fn icon_dir(&self, size: u32) -> PathBuf {
        self.data.join(format!("icons/hicolor/{size}x{size}/apps"))
    }

    fn icon_file(&self, size: u32) -> PathBuf {
        self.icon_dir(size).join(format!("{APP_ID}.png"))
    }
}

/// The start of a published build: installs, uninstalls, hands over to the installed app, or
/// tidies up after an update and runs. Windows waits for a running Lobby with `_rt`; Linux does
/// not need to.
pub fn on_start(_rt: &Handle, me: Version) -> Startup {
    let (Some(exe), Some(places)) = (running_exe(), Places::resolve()) else {
        warn!("no exe path or no home folder: running where it is");
        return Startup::Run;
    };
    let args: Vec<_> = env::args_os().skip(1).collect();
    let installed_exe = places.exe();
    let installed = installed_exe
        .exists()
        .then(|| installed_version(&places))
        .flatten();
    let in_install_dir = same_path(&exe, &installed_exe);
    let link = link_in(args.iter().cloned());
    match plan(uninstall_requested(&args), in_install_dir, installed, me) {
        Plan::Run => {
            clean_up(&exe);
            if let Err(e) = register(&places, me) {
                warn!(error = %e, "cannot update the launcher entry");
            }
            Startup::Run
        }
        Plan::LaunchInstalled if launch(&installed_exe, link.as_deref()) => {
            info!(installed = ?installed, "a newer or the same Lobby is installed: started it");
            Startup::Exit
        }
        // An installed app that does not start is repaired by installing this one over it.
        Plan::Install | Plan::LaunchInstalled => match install(&places, &exe, me) {
            Ok(()) => {
                info!(dir = %places.dir().display(), version = %me, "installed");
                launch(&installed_exe, link.as_deref());
                Startup::Exit
            }
            Err(e) => {
                warn!(error = %e, "cannot install: running where it is");
                Startup::Run
            }
        },
        Plan::Uninstall => {
            uninstall_from(&places);
            Startup::Exit
        }
    }
}

fn install(places: &Places, exe: &Path, me: Version) -> io::Result<()> {
    let dir = places.dir();
    fs::create_dir_all(&dir)?;
    let target = places.exe();
    // Copied beside the target first: the swap is then a rename, even over a Lobby still running.
    let staged = dir.join(SETUP_COPY);
    fs::copy(exe, &staged)?;
    fs::OpenOptions::new()
        .write(true)
        .open(&staged)?
        .sync_all()?;
    swap_in(&target, &staged)?;
    clean_up(&target);
    register(places, me)
}

/// The version file, the launcher entry and its icon, written at each start so they follow
/// updates. The OS is told about the `lobby://` handler only when the entry changed.
fn register(places: &Places, me: Version) -> io::Result<()> {
    fs::write(places.version_file(), format!("{me}\n"))?;
    let entry = desktop_entry(&places.exe());
    let changed = write_if_changed(&places.desktop_file(), entry.as_bytes())?;
    let mut icon_changed = false;
    for (size, png) in ICONS {
        icon_changed |= write_if_changed(&places.icon_file(size), png)?;
    }
    if changed || icon_changed {
        run_quietly("update-desktop-database", [places.applications()]);
        run_quietly(
            "xdg-mime",
            ["default".to_owned(), desktop_id(), scheme_mime()],
        );
    }
    Ok(())
}

fn desktop_id() -> String {
    format!("{APP_ID}.desktop")
}

fn scheme_mime() -> String {
    format!("x-scheme-handler/{DEEP_LINK_SCHEME}")
}

fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         Comment={DESCRIPTION}\n\
         Exec={} %u\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Network;Chat;\n\
         MimeType={};\n\
         StartupNotify=true\n\
         StartupWMClass={APP_ID}\n",
        exec_path(exe),
        scheme_mime(),
    )
}

/// A path as the Desktop Entry spec's `Exec` key wants it: in quotes when it holds anything
/// but plain characters, with the characters the spec reserves escaped at both levels (a
/// backslash needs four).
fn exec_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        return text.into_owned();
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str(r"\\\\"),
            '"' | '`' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|found| found == bytes) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, bytes)?;
    Ok(true)
}

/// Runs a desktop helper that may not be installed, and ignores how it ends.
fn run_quietly<S: AsRef<std::ffi::OsStr>>(program: &str, args: impl IntoIterator<Item = S>) {
    let ended = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match ended {
        Ok(status) if status.success() => {}
        Ok(status) => debug!(program, %status, "the desktop helper failed"),
        Err(e) => debug!(program, error = %e, "the desktop helper cannot run"),
    }
}

fn installed_version(places: &Places) -> Option<Version> {
    fs::read_to_string(places.version_file())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Removes the app's files, the launcher entry, its icon and the `lobby://` default, and the
/// folders that leaves empty. Only the files an install makes are deleted. Lobbies and settings
/// stay.
fn uninstall_from(places: &Places) {
    let dir = places.dir();
    for file in APP_FILES.iter().copied().chain([VERSION_FILE]) {
        let _ = fs::remove_file(dir.join(file));
    }
    let _ = fs::remove_file(places.desktop_file());
    for (size, _) in ICONS {
        let _ = fs::remove_file(places.icon_file(size));
    }
    let icon_dirs = ICONS.map(|(size, _)| places.icon_dir(size));
    for leftover in [dir, places.applications()].into_iter().chain(icon_dirs) {
        prune_empty(&leftover, &places.data);
    }
    forget_link_handler();
    run_quietly("update-desktop-database", [places.applications()]);
    info!("uninstalled");
    println!("Lobby is uninstalled. Your Lobbies and settings stay on this computer.");
}

/// Removes `dir` and then each parent below `stop`, as long as it is empty.
fn prune_empty(dir: &Path, stop: &Path) {
    let mut at = dir;
    while at != stop && at.starts_with(stop) && fs::remove_dir(at).is_ok() {
        match at.parent() {
            Some(parent) => at = parent,
            None => return,
        }
    }
}

/// Takes Lobby out of `mimeapps.list`, where `xdg-mime default` wrote it. The file goes too when
/// that leaves nothing in it but section headers.
fn forget_link_handler() {
    let config = match env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => match env::var_os("HOME").filter(|h| !h.is_empty()) {
            Some(home) => PathBuf::from(home).join(".config"),
            None => return,
        },
    };
    let path = config.join("mimeapps.list");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let (kept, changed) = without_link_handler(&text);
    if !changed {
        return;
    }
    let only_headers = kept
        .lines()
        .all(|l| l.trim().is_empty() || l.trim_start().starts_with('['));
    let done = if only_headers {
        fs::remove_file(&path)
    } else {
        fs::write(&path, kept)
    };
    if let Err(e) = done {
        warn!(error = %e, "cannot take Lobby out of mimeapps.list");
    }
}

/// `text` without Lobby among the handlers of the `lobby://` scheme, and whether that changed
/// anything.
fn without_link_handler(text: &str) -> (String, bool) {
    let key = format!("{}=", scheme_mime());
    let id = desktop_id();
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for line in text.lines() {
        let Some(value) = line.strip_prefix(&key) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let others: Vec<&str> = value
            .split(';')
            .filter(|handler| !handler.is_empty() && *handler != id)
            .collect();
        if others.len() == value.split(';').filter(|h| !h.is_empty()).count() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        changed = true;
        if !others.is_empty() {
            out.push_str(&format!("{key}{};\n", others.join(";")));
        }
    }
    (out, changed)
}

/// Whether the icon theme can name Lobby's icon: an install put it where the theme looks.
pub(super) fn icon_installed() -> bool {
    Places::resolve().is_some_and(|places| places.icon_file(256).exists())
}

/// Starts `exe`, with the `lobby://` link it was asked to open, in a process group of its own
/// and with no terminal: closing the terminal it was started from or pressing Ctrl-C there
/// leaves it running.
pub fn launch(exe: &Path, link: Option<&str>) -> bool {
    let started = Command::new(exe)
        .args(link)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match started {
        Ok(_) => true,
        Err(e) => {
            warn!(exe = %exe.display(), error = %e, "cannot start Lobby");
            false
        }
    }
}

/// Swaps the downloaded update in for this binary, and with `relaunch` starts it. Called once
/// the app has quit.
pub fn apply_update(staged: &Path, relaunch: bool) {
    let Some(exe) = running_exe() else {
        return warn!("no exe path: the update waits");
    };
    match swap_in(&exe, staged) {
        Ok(()) => info!(relaunch, "the update is in place"),
        // The app is still the old one, which offers the update again.
        Err(e) => warn!(error = %e, "cannot swap the update in"),
    }
    if relaunch {
        launch(&exe, None);
    }
}

/// This binary's path: where an update or a relaunch goes.
fn running_exe() -> Option<PathBuf> {
    Some(installed_name(env::current_exe().ok()?))
}

/// Linux appends " (deleted)" to the path of a binary that was replaced while it runs, such as
/// the one an update swapped in. An earlier version moved the running app aside to `lobby.old`
/// before replacing it, so a process from then reports `lobby.old (deleted)`: the app it stands
/// for is the `lobby` beside it.
fn installed_name(exe: PathBuf) -> PathBuf {
    let plain = match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(plain) => PathBuf::from(plain),
        None => exe,
    };
    if plain.file_name().is_some_and(|name| name == OLD) {
        return plain.with_file_name(EXE);
    }
    plain
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

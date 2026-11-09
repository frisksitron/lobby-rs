//! Installing and uninstalling the app for one person, with no installer and no admin prompt. A
//! published build is its own setup: started from anywhere but `%LOCALAPPDATA%\Programs\Lobby`, it
//! copies itself there, adds Start menu and desktop shortcuts, an entry in Installed apps and the
//! `lobby://` link handler, and starts the copy. That entry runs `Lobby.exe --uninstall`. Updates
//! swap the exe in place ([`relaunch`]), so the folder never holds more than one app.

#![allow(unsafe_code)]

use std::{
    env,
    ffi::OsString,
    fs, io,
    os::windows::process::CommandExt as _,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use lobby_client::{
    DEEP_LINK_SCHEME,
    update::{APP_FILES, Version, clean_up, swap_in},
};
use tokio::runtime::Handle;
use tracing::{info, warn};
use windows::{
    Win32::{
        Storage::EnhancedStorage::PKEY_AppUserModel_ID,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, IPersistFile,
                StructuredStorage::{PROPVARIANT, PVCHF_DEFAULT, PropVariantChangeType},
            },
            Variant::VT_LPWSTR,
        },
        UI::{
            Shell::{
                FOLDERID_Desktop, FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT,
                PropertiesSystem::IPropertyStore, SHGetKnownFolderPath, ShellLink,
            },
            WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW},
        },
    },
    core::{GUID, HSTRING, Interface as _},
};

use super::{link_in, single_instance, win};
use crate::host_env::{APP_NAME, AUMID_PREFIX};

pub const EXE: &str = "Lobby.exe";
pub const UNINSTALL_ARG: &str = "--uninstall";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Lobby";
const AUMID_KEY: &str = r"Software\Classes\AppUserModelId";
const CLASSES_KEY: &str = r"Software\Classes";
const SCHEME_DESCRIPTION: &str = "URL:Lobby";
const SHORTCUT: &str = "Lobby.lnk";
const DESCRIPTION: &str = "Chat, Voice and Screen Share with your friends";
/// How long setup and uninstall wait for a running Lobby to quit. Quitting takes under two
/// seconds.
const QUIT_WAIT: Duration = Duration::from_secs(10);
const QUIT_POLL: Duration = Duration::from_millis(100);
const CLEAN_UP_AGAIN: Duration = Duration::from_secs(10);
/// CREATE_NO_WINDOW: the clean-up after uninstall runs without a console window.
const NO_WINDOW: u32 = 0x0800_0000;

/// What `main` does after [`on_start`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    Run,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    Run,
    Install,
    /// This exe is no newer than the installed app: start that one instead.
    LaunchInstalled,
    Uninstall,
}

fn plan(uninstall: bool, in_install_dir: bool, installed: Option<Version>, me: Version) -> Plan {
    match (uninstall, in_install_dir, installed) {
        (true, _, _) => Plan::Uninstall,
        (false, true, _) => Plan::Run,
        (false, false, Some(installed)) if installed >= me => Plan::LaunchInstalled,
        (false, false, _) => Plan::Install,
    }
}

/// Where the app lives once installed.
pub fn install_dir() -> Option<PathBuf> {
    let local = env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("Programs").join(APP_NAME))
}

/// The start of a published build: installs, uninstalls, hands over to the installed app, or
/// tidies up after an update and runs.
pub fn on_start(rt: &Handle, me: Version) -> Startup {
    let (Ok(exe), Some(dir)) = (env::current_exe(), install_dir()) else {
        warn!("no exe path or no LOCALAPPDATA: running where it is");
        return Startup::Run;
    };
    let uninstall = uninstall_requested(&env::args_os().skip(1).collect::<Vec<_>>());
    let installed_exe = dir.join(EXE);
    let in_install_dir = same_path(&exe, &installed_exe);
    let installed = installed_exe.exists().then(installed_version).flatten();
    match plan(uninstall, in_install_dir, installed, me) {
        Plan::Run => {
            clean_up(&exe);
            // The app this update replaced may still be exiting: try again once it is gone.
            let later = exe.clone();
            std::thread::spawn(move || {
                std::thread::sleep(CLEAN_UP_AGAIN);
                clean_up(&later);
            });
            if let Err(e) = register(&installed_exe, me) {
                warn!(error = %e, "cannot update the Installed apps entry");
            }
            Startup::Run
        }
        Plan::LaunchInstalled if launch(&installed_exe) => {
            info!(installed = ?installed, "a newer or the same Lobby is installed: started it");
            Startup::Exit
        }
        // An installed app that does not start is repaired by installing this one over it.
        Plan::Install | Plan::LaunchInstalled => {
            match install(rt, &exe, &dir, me) {
                Ok(()) => {
                    info!(dir = %dir.display(), version = %me, "installed");
                    let _ = launch(&installed_exe);
                }
                Err(e) => {
                    warn!(error = %e, "cannot install: running where it is");
                    return Startup::Run;
                }
            }
            Startup::Exit
        }
        Plan::Uninstall => {
            if confirm_uninstall() {
                uninstall_from(rt, &dir);
            }
            Startup::Exit
        }
    }
}

/// `--uninstall` among the arguments, unless a `lobby://` link is there too: a link is untrusted
/// text, and quotes in it can pass for more arguments.
fn uninstall_requested(args: &[OsString]) -> bool {
    link_in(args.iter().cloned()).is_none() && args.iter().any(|a| a == UNINSTALL_ARG)
}

fn install(rt: &Handle, exe: &Path, dir: &Path, me: Version) -> io::Result<()> {
    ask_running_to_quit(rt);
    fs::create_dir_all(dir)?;
    let target = dir.join(EXE);
    // Copied beside the target first: the swap is then a rename, even over a Lobby still exiting.
    let staged = dir.join("Lobby.setup.exe");
    fs::copy(exe, &staged)?;
    fs::OpenOptions::new()
        .write(true)
        .open(&staged)?
        .sync_all()?;
    swap_in(&target, &staged)?;
    // The copy keeps the download's internet-zone mark, which would make Windows warn at every
    // start from the shortcut.
    let _ = fs::remove_file(format!("{}:Zone.Identifier", target.display()));
    for folder in [FOLDERID_Programs, FOLDERID_Desktop] {
        match known_folder(&folder) {
            Some(at) => {
                if let Err(e) = shortcut(&at.join(SHORTCUT), &target) {
                    warn!(folder = %at.display(), error = %e, "cannot make a shortcut");
                }
            }
            None => warn!("a shortcut folder is unknown"),
        }
    }
    register(&target, me)
}

/// The Installed apps entry, written again at each start so its version follows updates.
fn register(exe: &Path, me: Version) -> io::Result<()> {
    let key = windows_registry::CURRENT_USER
        .create(UNINSTALL_KEY)
        .map_err(io::Error::other)?;
    let exe_s = exe.display().to_string();
    let dir = exe
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let size_kb = fs::metadata(exe).map(|m| m.len() / 1024).unwrap_or(0);
    let strings = [
        ("DisplayName", APP_NAME.to_owned()),
        ("DisplayVersion", me.to_string()),
        ("Publisher", APP_NAME.to_owned()),
        ("DisplayIcon", format!("{exe_s},0")),
        ("InstallLocation", dir),
        ("UninstallString", format!("\"{exe_s}\" {UNINSTALL_ARG}")),
    ];
    for (name, value) in strings {
        key.set_string(name, value).map_err(io::Error::other)?;
    }
    if let Some(repo) = crate::updates::published_repo() {
        key.set_string("URLInfoAbout", format!("https://github.com/{repo}"))
            .map_err(io::Error::other)?;
    }
    for (name, value) in [
        ("NoModify", 1),
        ("NoRepair", 1),
        ("EstimatedSize", u32::try_from(size_kb).unwrap_or(u32::MAX)),
    ] {
        key.set_u32(name, value).map_err(io::Error::other)?;
    }
    register_link_handler(&link_handler_key(), exe)
}

/// Where Windows looks for what runs a `lobby://` link.
fn link_handler_key() -> String {
    format!(r"{CLASSES_KEY}\{DEEP_LINK_SCHEME}")
}

/// Makes `exe` what Windows runs for a `lobby://` link, in `key` under the current user: written
/// at each start, so it follows the exe's place.
fn register_link_handler(key: &str, exe: &Path) -> io::Result<()> {
    let exe = exe.display();
    let user = windows_registry::CURRENT_USER;
    let scheme = user.create(key).map_err(io::Error::other)?;
    // The empty name is the key's default value.
    scheme
        .set_string("", SCHEME_DESCRIPTION)
        .map_err(io::Error::other)?;
    scheme
        .set_string("URL Protocol", "")
        .map_err(io::Error::other)?;
    for (sub, value) in [
        ("DefaultIcon", format!("\"{exe}\",0")),
        (r"shell\open\command", format!("\"{exe}\" \"%1\"")),
    ] {
        user.create(format!(r"{key}\{sub}"))
            .and_then(|k| k.set_string("", value))
            .map_err(io::Error::other)?;
    }
    Ok(())
}

fn installed_version() -> Option<Version> {
    windows_registry::CURRENT_USER
        .open(UNINSTALL_KEY)
        .and_then(|k| k.get_string("DisplayVersion"))
        .ok()?
        .parse()
        .ok()
}

fn confirm_uninstall() -> bool {
    let text = HSTRING::from(
        "Uninstall Lobby?\n\nYour Lobbies and settings stay on this PC, so signing in again is \
         not needed if you install it later.",
    );
    let caption = HSTRING::from("Uninstall Lobby");
    // SAFETY: both strings outlive the call.
    let answer = unsafe { MessageBoxW(None, &text, &caption, MB_YESNO | MB_ICONQUESTION) };
    answer == IDYES
}

/// Removes the shortcuts and the registry entries, then deletes the app's own files once this
/// process has exited. Only the files an install makes are deleted, and the folder only if that
/// leaves it empty.
fn uninstall_from(rt: &Handle, dir: &Path) {
    ask_running_to_quit(rt);
    for folder in [FOLDERID_Programs, FOLDERID_Desktop] {
        if let Some(at) = known_folder(&folder) {
            let _ = fs::remove_file(at.join(SHORTCUT));
        }
    }
    let user = windows_registry::CURRENT_USER;
    let _ = user.remove_tree(UNINSTALL_KEY);
    let _ = user.remove_tree(format!(r"{AUMID_KEY}\{AUMID_PREFIX}"));
    let _ = user.remove_tree(link_handler_key());
    let files: Vec<String> = APP_FILES
        .iter()
        .map(|f| format!("\"{}\"", dir.join(f).display()))
        .collect();
    // The exe that runs this cannot delete itself: a shell does it a few seconds later.
    let script = format!(
        "ping -n 4 127.0.0.1 >nul & del /f /q {} & rmdir \"{}\"",
        files.join(" "),
        dir.display()
    );
    let spawned = Command::new("cmd.exe")
        .args(["/d", "/c"])
        .raw_arg(&script)
        .creation_flags(NO_WINDOW)
        .spawn();
    match spawned {
        Ok(_) => info!("uninstalled"),
        Err(e) => warn!(error = %e, "cannot delete the app's files"),
    }
}

/// A Lobby that runs from the install folder holds its exe: ask it to quit, as its tray menu
/// would, and wait until it has let go of its one-instance pipe.
fn ask_running_to_quit(rt: &Handle) {
    let name = single_instance::pipe_name(&lobby_client::KeyringTokenStore::profile(None, None));
    let Some(pid) = rt.block_on(single_instance::ask_to_quit(&name)) else {
        return;
    };
    info!("asked the running Lobby to quit");
    // The pipe goes as quitting starts, the exe is free only once the process is gone.
    if let Some(pid) = pid {
        if !win::wait_for_exit(pid, QUIT_WAIT) {
            warn!("the running Lobby did not quit in time");
        }
        return;
    }
    let started = Instant::now();
    while single_instance::is_running(&name) {
        if started.elapsed() > QUIT_WAIT {
            warn!("the running Lobby did not quit in time");
            return;
        }
        std::thread::sleep(QUIT_POLL);
    }
}

/// Starts `exe` and lets it take the foreground, which this process has from the person's click.
pub fn launch(exe: &Path) -> bool {
    match Command::new(exe).spawn() {
        Ok(child) => {
            win::allow_foreground_for(child.id());
            true
        }
        Err(e) => {
            warn!(exe = %exe.display(), error = %e, "cannot start Lobby");
            false
        }
    }
}

/// Swaps the downloaded update in for this exe, and with `relaunch` starts it. Called once the
/// app has quit, so the new one finds the one-instance pipe free.
pub fn apply_update(staged: &Path, relaunch: bool) {
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return warn!(error = %e, "no exe path: the update waits"),
    };
    match swap_in(&exe, staged) {
        Ok(()) => info!(relaunch, "the update is in place"),
        // The app is still the old one, which offers the update again.
        Err(e) => warn!(error = %e, "cannot swap the update in"),
    }
    if relaunch {
        launch(&exe);
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: the returned string is copied out, then freed with the allocator it came from.
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let out = path.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(path.0 as _));
        out
    }
}

/// A shortcut to `target` with the app's AppUserModelID, so a pinned shortcut, the running
/// window and its toasts are one app to the taskbar.
fn shortcut(at: &Path, target: &Path) -> windows::core::Result<()> {
    // SAFETY: COM calls on interfaces this function made; the strings outlive each call.
    unsafe {
        // Already initialised on this thread is fine.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target.as_os_str()))?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        link.SetDescription(&HSTRING::from(DESCRIPTION))?;
        let store: IPropertyStore = link.cast()?;
        let mut id = PROPVARIANT::default();
        PropVariantChangeType(
            &mut id,
            &PROPVARIANT::from(AUMID_PREFIX),
            PVCHF_DEFAULT,
            VT_LPWSTR,
        )?;
        store.SetValue(&PKEY_AppUserModel_ID, &id)?;
        store.Commit()?;
        link.cast::<IPersistFile>()?
            .Save(&HSTRING::from(at.as_os_str()), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_installs_only_over_an_older_app_and_the_installed_app_just_runs() {
        let v = |s: &str| s.parse::<Version>().unwrap();
        let me = v("0.11.0");
        assert_eq!(plan(false, false, None, me), Plan::Install);
        assert_eq!(plan(false, false, Some(v("0.10.2")), me), Plan::Install);
        assert_eq!(plan(false, false, Some(me), me), Plan::LaunchInstalled);
        assert_eq!(
            plan(false, false, Some(v("0.12.0")), me),
            Plan::LaunchInstalled
        );
        assert_eq!(plan(false, true, Some(me), me), Plan::Run);
        assert_eq!(plan(true, true, Some(me), me), Plan::Uninstall);
    }

    #[test]
    fn a_link_never_passes_for_the_uninstall_flag() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(uninstall_requested(&args(&["--uninstall"])));
        assert!(!uninstall_requested(&args(&[])));
        assert!(!uninstall_requested(&args(&["lobby://x", "--uninstall"])));
        assert!(!uninstall_requested(&args(&["--uninstall", "LOBBY:x"])));
    }
}

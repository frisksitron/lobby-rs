//! What a published build does when it starts, decided once for every system where the app
//! installs itself (`install.rs` on Windows, `install_linux.rs` on Linux).

use std::ffi::OsString;

use lobby_client::update::Version;

use super::link_in;

pub const UNINSTALL_ARG: &str = "--uninstall";

/// What `main` does after the install's `on_start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    Run,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Run,
    Install,
    /// This exe is no newer than the installed app: start that one instead.
    LaunchInstalled,
    Uninstall,
}

pub fn plan(
    uninstall: bool,
    in_install_dir: bool,
    installed: Option<Version>,
    me: Version,
) -> Plan {
    match (uninstall, in_install_dir, installed) {
        (true, _, _) => Plan::Uninstall,
        (false, true, _) => Plan::Run,
        (false, false, Some(installed)) if installed >= me => Plan::LaunchInstalled,
        (false, false, _) => Plan::Install,
    }
}

/// `--uninstall` among the arguments, unless a `lobby://` link is there too: a link is untrusted
/// text, and quotes in it can pass for more arguments.
pub fn uninstall_requested(args: &[OsString]) -> bool {
    link_in(args.iter().cloned()).is_none() && args.iter().any(|a| a == UNINSTALL_ARG)
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

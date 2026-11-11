//! Repository gates run by CI, the scenario driver and the Linux package.

pub mod changelog;
pub mod check_api;
pub mod check_comments;
pub mod check_deps;
pub mod check_kit;
pub mod check_px;
pub mod drive;
pub mod gates;
pub mod package_linux;
pub mod release;
pub mod toolchain;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// A cargo that builds the workspace, as if run from a plain shell. `cargo xtask` sets xtask's
/// `CARGO_MANIFEST_DIR` and `CARGO_PKG_*` in its process. ring's build script tracks them, so a
/// cargo that inherits them rebuilds ring, rustls and everything above them (lobby-client
/// included) every time it differs from the last build.
pub fn workspace_cargo(cargo: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(cargo);
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text == "CARGO_MANIFEST_DIR" || name_text.starts_with("CARGO_PKG_") {
            command.env_remove(&name);
        }
    }
    command
}

/// Runs git in `workspace` and returns its trimmed output.
pub fn git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn workspace_root() -> PathBuf {
    // `cargo run` sets it for this checkout. The compiled-in value can name another worktree that
    // shares `./target` and built xtask last.
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    Path::new(&manifest)
        .parent()
        .expect("xtask sits in the workspace")
        .to_owned()
}

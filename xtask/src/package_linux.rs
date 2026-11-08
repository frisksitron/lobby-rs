//! `cargo xtask package-linux`: the release `lobby` binary with its desktop entry, icon and
//! AppStream metadata, laid out as a prefix (`bin/`, `share/`) and packed into
//! `target/package/lobby-<version>-x86_64-linux.tar.gz`. Extracting it into `~/.local` installs it
//! for one person.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const APP_ID: &str = "io.github.frisksitron.Lobby";

pub fn run(workspace: &Path) -> Result<PathBuf, String> {
    if !cfg!(target_os = "linux") {
        return Err("package-linux runs on Linux (on Windows, inside WSL)".into());
    }
    let status = crate::workspace_cargo(env!("CARGO"))
        .args([
            "build",
            "--release",
            "-p",
            "lobby-desktop",
            "--bin",
            "lobby",
        ])
        .current_dir(workspace)
        .status()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build failed".into());
    }

    let name = format!("lobby-{}-x86_64-linux", env!("CARGO_PKG_VERSION"));
    let out = workspace.join("target/package");
    let root = out.join(&name);
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    }
    let packaging = workspace.join("packaging/linux");
    let files = [
        (
            workspace.join("target/release/lobby"),
            "bin/lobby".to_owned(),
        ),
        (
            packaging.join(format!("{APP_ID}.desktop")),
            format!("share/applications/{APP_ID}.desktop"),
        ),
        (
            packaging.join(format!("{APP_ID}.svg")),
            format!("share/icons/hicolor/scalable/apps/{APP_ID}.svg"),
        ),
        (
            packaging.join(format!("{APP_ID}.metainfo.xml")),
            format!("share/metainfo/{APP_ID}.metainfo.xml"),
        ),
    ];
    for (from, to) in &files {
        let to = root.join(to);
        fs::create_dir_all(to.parent().expect("a file under the root"))
            .map_err(|e| format!("{}: {e}", to.display()))?;
        fs::copy(from, &to).map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))?;
    }

    let tarball = out.join(format!("{name}.tar.gz"));
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("-C")
        .arg(&out)
        .arg(&name)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err("tar failed".into());
    }
    Ok(tarball)
}

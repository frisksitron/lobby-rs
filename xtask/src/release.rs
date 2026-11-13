//! Publishing the apps. `cargo xtask release <version>` refuses a commit subject that
//! breaks the rules in `changelog.rs`, writes the release's entry into `CHANGELOG.md`, bumps the
//! workspace version, commits, tags `v<version>` and pushes; the tag's CI run
//! (`.github/workflows/release.yml`) then runs `cargo xtask dist` on each system and attaches
//! its files to a GitHub Release, which every published app reads within the hour. On Windows:
//!
//! - `LobbySetup.exe`: the release `lobby`, built knowing its repository, so it installs and
//!   updates itself from that repository's Releases.
//! - `latest.json`: the version, size and SHA-256 the app checks a download against.
//! - `release-notes.md`: the newest changelog entry, as the Release's description. It is not an
//!   asset.
//!
//! On x86_64 Linux the same two files are `lobby-linux-x86_64` and `latest-linux.json`, and the
//! notes are the Windows job's.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

use crate::{changelog, git};

const NOTES: &str = "release-notes.md";
const DLL_CRT: &[u8] = b"vcruntime140.dll";
/// The newest glibc a Linux build may need: the one on the `ubuntu-24.04` runner that builds a
/// release, and so the oldest system a Release's app is promised to start on.
const GLIBC_CEILING: (u32, u32) = (2, 39);

/// What one system's `dist` builds and checks.
struct Target {
    /// The release asset, and the file `cargo build` writes it from.
    asset: &'static str,
    built: &'static str,
    manifest: &'static str,
    /// Whether this system's job writes the Release's description.
    writes_notes: bool,
    /// Whether the shipped copy drops the debug info a release build embeds: Windows keeps it in
    /// a `.pdb` beside the exe, and on Linux it would be four fifths of every update's download.
    strip_debug: bool,
    /// Fails when the app needs a runtime that a system it is promised to run on lacks.
    check: fn(&[u8]) -> Result<(), String>,
}

const WINDOWS: Target = Target {
    asset: "LobbySetup.exe",
    built: "lobby.exe",
    manifest: "latest.json",
    writes_notes: true,
    strip_debug: false,
    check: check_windows_runtime,
};

const LINUX: Target = Target {
    asset: "lobby-linux-x86_64",
    built: "lobby",
    manifest: "latest-linux.json",
    writes_notes: false,
    strip_debug: true,
    check: check_glibc,
};

fn host_target() -> Result<&'static Target, String> {
    if cfg!(windows) {
        Ok(&WINDOWS)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(&LINUX)
    } else {
        Err("dist builds the Windows app on Windows and the Linux app on x86_64 Linux".into())
    }
}

/// Import table names are plain ASCII. A dependency built against the DLL C runtime brings this
/// import back, and a Windows without the Visual C++ runtime then cannot start the app.
fn check_windows_runtime(app: &[u8]) -> Result<(), String> {
    if app
        .windows(DLL_CRT.len())
        .any(|w| w.eq_ignore_ascii_case(DLL_CRT))
    {
        return Err(
            "lobby.exe imports VCRUNTIME140.dll: a dependency links the DLL C runtime".into(),
        );
    }
    Ok(())
}

/// The newest `GLIBC_<major>.<minor>` the binary asks the system for, read from the version names
/// in its dynamic symbol table (each ends in a NUL).
fn glibc_needed(app: &[u8]) -> Option<(u32, u32)> {
    const PREFIX: &[u8] = b"GLIBC_";
    let mut newest = None;
    for (at, _) in app
        .windows(PREFIX.len())
        .enumerate()
        .filter(|(_, w)| *w == PREFIX)
    {
        let rest = &app[at + PREFIX.len()..];
        let Some(end) = rest.iter().position(|&b| b == 0) else {
            continue;
        };
        let Ok(name) = std::str::from_utf8(&rest[..end]) else {
            continue;
        };
        let mut parts = name.split('.').map(|p| p.parse::<u32>().ok());
        if let (Some(Some(major)), Some(Some(minor))) = (parts.next(), parts.next()) {
            newest = newest.max(Some((major, minor)));
        }
    }
    newest
}

fn check_glibc(app: &[u8]) -> Result<(), String> {
    match glibc_needed(app) {
        Some(needed) if needed > GLIBC_CEILING => Err(format!(
            "lobby needs glibc {}.{}, newer than the {}.{} of ubuntu-24.04: build it there",
            needed.0, needed.1, GLIBC_CEILING.0, GLIBC_CEILING.1
        )),
        _ => Ok(()),
    }
}

/// The bytes of `built` without its debug info (its symbols stay, so a backtrace names functions),
/// made through `scratch`, which is deleted again.
fn without_debug_info(built: &Path, scratch: &Path) -> Result<Vec<u8>, String> {
    fs::copy(built, scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let status = std::process::Command::new("strip")
        .arg("--strip-debug")
        .arg(scratch)
        .status()
        .map_err(|e| format!("strip: {e}"))?;
    let bytes = fs::read(scratch).map_err(|e| format!("{}: {e}", scratch.display()));
    let _ = fs::remove_file(scratch);
    if !status.success() {
        return Err("strip failed".into());
    }
    bytes
}

/// Builds the release app for `repo` (`owner/name`) and writes `dist/` at the workspace root.
pub fn dist(workspace: &Path, repo: &str) -> Result<PathBuf, String> {
    let target = host_target()?;
    if repo.split('/').filter(|part| !part.is_empty()).count() != 2 {
        return Err(format!("--repo wants owner/name, got {repo:?}"));
    }
    let entries = changelog::parse(&changelog::read(workspace)?)?;
    let newest = match entries.first() {
        Some(entry) if entry.version == env!("CARGO_PKG_VERSION") => entry,
        _ => {
            return Err(format!(
                "{} has no newest entry for {}",
                changelog::FILE,
                env!("CARGO_PKG_VERSION")
            ));
        }
    };
    let status = crate::workspace_cargo(env!("CARGO"))
        .args([
            "build",
            "--release",
            "-p",
            "lobby-desktop",
            "--bin",
            "lobby",
        ])
        .env("LOBBY_UPDATE_REPO", repo)
        .current_dir(workspace)
        .status()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build failed".into());
    }
    let built = target_dir(workspace).join("release").join(target.built);
    let out = workspace.join("dist");
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let app = if target.strip_debug {
        without_debug_info(&built, &out.join(format!("{}.strip", target.asset)))?
    } else {
        fs::read(&built).map_err(|e| format!("{}: {e}", built.display()))?
    };
    (target.check)(&app)?;
    fs::write(out.join(target.asset), &app).map_err(|e| format!("{}: {e}", target.asset))?;
    let sha256: String = Sha256::digest(&app)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let manifest = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "size": app.len(),
        "sha256": sha256,
    });
    let manifest = serde_json::to_string_pretty(&manifest).expect("JSON");
    fs::write(out.join(target.manifest), manifest + "\n")
        .map_err(|e| format!("{}: {e}", target.manifest))?;
    if target.writes_notes {
        fs::write(out.join(NOTES), newest.body()).map_err(|e| format!("{NOTES}: {e}"))?;
    }
    Ok(out)
}

fn target_dir(workspace: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"))
}

/// Writes the release's changelog entry and bumps the workspace to `version`, commits and tags
/// it, and pushes both when there is an `origin`.
pub fn release(workspace: &Path, version: &str) -> Result<String, String> {
    let next = parse(version).ok_or_else(|| format!("{version:?} is not major.minor.patch"))?;
    let current = env!("CARGO_PKG_VERSION");
    if next <= parse(current).expect("the workspace version parses") {
        return Err(format!("{version} is not newer than {current}"));
    }
    let git = |args: &[&str]| git(workspace, args);
    if !git(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("commit or drop your changes first: a release is one commit".into());
    }
    let subjects = changelog::subjects_since_last_tag(workspace)?;
    let date = changelog::head_date(workspace)?;
    let entry = changelog::entry(version, &date, &subjects).map_err(|problems| {
        format!(
            "reword these commits first (see AGENTS.md, Commits):\n{}",
            problems.join("\n")
        )
    })?;
    let changelog_path = workspace.join(changelog::FILE);
    let changelog_text = changelog::prepend(&changelog::read(workspace)?, &entry)?;
    let manifest = workspace.join("Cargo.toml");
    let text = fs::read_to_string(&manifest).map_err(|e| format!("Cargo.toml: {e}"))?;
    let from = format!("[workspace.package]\nversion = \"{current}\"");
    if !text.contains(&from) {
        return Err("Cargo.toml has no `version` right under [workspace.package]".into());
    }
    let to = format!("[workspace.package]\nversion = \"{version}\"");
    fs::write(&manifest, text.replacen(&from, &to, 1)).map_err(|e| format!("Cargo.toml: {e}"))?;
    fs::write(&changelog_path, changelog_text).map_err(|e| format!("{}: {e}", changelog::FILE))?;
    // Cargo.lock names each workspace crate's version.
    let status = crate::workspace_cargo(env!("CARGO"))
        .args(["update", "--workspace", "--offline"])
        .current_dir(workspace)
        .status()
        .map_err(|e| format!("cargo update: {e}"))?;
    if !status.success() {
        return Err("cargo update --workspace failed".into());
    }
    let tag = format!("v{version}");
    let message = format!("build: release {version}");
    git(&["commit", "-a", "-m", &message])?;
    git(&["tag", "-a", &tag, "-m", &message])?;
    if git(&["remote", "get-url", "origin"]).is_err() {
        return Ok(format!(
            "tagged {tag}; no `origin` yet, so push it yourself: git push --atomic origin HEAD {tag}"
        ));
    }
    git(&["push", "--atomic", "origin", "HEAD", &tag])?;
    Ok(format!(
        "pushed {tag}: CI builds it and publishes the Release, and the apps update within the hour"
    ))
}

fn parse(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary's dynamic string table: names, each ended by a NUL.
    fn strings(names: &[&str]) -> Vec<u8> {
        names
            .iter()
            .flat_map(|n| format!("{n}\0").into_bytes())
            .collect()
    }

    #[test]
    fn a_linux_app_may_need_the_ceilings_glibc_and_nothing_newer() {
        let at_ceiling = strings(&["GLIBC_2.2.5", "GLIBC_2.39", "libc.so.6", "GLIBC_TUNABLES"]);
        assert_eq!(glibc_needed(&at_ceiling), Some((2, 39)));
        assert!(check_glibc(&at_ceiling).is_ok());

        let too_new = strings(&["GLIBC_2.17", "GLIBC_2.43", "GLIBC_2.38"]);
        let refused = check_glibc(&too_new).unwrap_err();
        assert!(
            refused.contains("2.43") && refused.contains("2.39"),
            "{refused}"
        );

        assert_eq!(glibc_needed(&strings(&["libc.so.6"])), None);
    }
}

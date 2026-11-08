//! Publishing the Windows app. `cargo xtask release <version>` bumps the workspace version,
//! commits, tags `v<version>` and pushes; the tag's CI run (`.github/workflows/release.yml`)
//! then runs `cargo xtask dist` and attaches its two files to a GitHub Release, which every
//! published app reads within the hour:
//!
//! - `LobbySetup.exe`: the release `lobby`, built knowing its repository, so it installs and
//!   updates itself from that repository's Releases.
//! - `latest.json`: the version, size and SHA-256 the app checks a download against.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest as _, Sha256};

const ASSET: &str = "LobbySetup.exe";
const MANIFEST: &str = "latest.json";
const DLL_CRT: &[u8] = b"vcruntime140.dll";

/// Builds the release app for `repo` (`owner/name`) and writes `dist/` at the workspace root.
pub fn dist(workspace: &Path, repo: &str) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Err("dist builds the Windows app, so it runs on Windows".into());
    }
    if repo.split('/').filter(|part| !part.is_empty()).count() != 2 {
        return Err(format!("--repo wants owner/name, got {repo:?}"));
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
        .env("LOBBY_UPDATE_REPO", repo)
        .current_dir(workspace)
        .status()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build failed".into());
    }
    let built = target_dir(workspace).join("release").join("lobby.exe");
    let app = fs::read(&built).map_err(|e| format!("{}: {e}", built.display()))?;
    // Import table names are plain ASCII. A dependency built against the DLL C runtime brings this
    // import back, and a Windows without the Visual C++ runtime then cannot start the app.
    if app
        .windows(DLL_CRT.len())
        .any(|w| w.eq_ignore_ascii_case(DLL_CRT))
    {
        return Err(
            "lobby.exe imports VCRUNTIME140.dll: a dependency links the DLL C runtime".into(),
        );
    }
    let out = workspace.join("dist");
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    fs::write(out.join(ASSET), &app).map_err(|e| format!("{ASSET}: {e}"))?;
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
    fs::write(out.join(MANIFEST), manifest + "\n").map_err(|e| format!("{MANIFEST}: {e}"))?;
    Ok(out)
}

fn target_dir(workspace: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"))
}

/// Bumps the workspace to `version`, commits and tags it, and pushes both when there is an
/// `origin`.
pub fn release(workspace: &Path, version: &str) -> Result<String, String> {
    let next = parse(version).ok_or_else(|| format!("{version:?} is not major.minor.patch"))?;
    let current = env!("CARGO_PKG_VERSION");
    if next <= parse(current).expect("the workspace version parses") {
        return Err(format!("{version} is not newer than {current}"));
    }
    let git = |args: &[&str]| -> Result<String, String> {
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
    };
    if !git(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("commit or drop your changes first: a release is one commit".into());
    }
    let manifest = workspace.join("Cargo.toml");
    let text = fs::read_to_string(&manifest).map_err(|e| format!("Cargo.toml: {e}"))?;
    let from = format!("[workspace.package]\nversion = \"{current}\"");
    if !text.contains(&from) {
        return Err("Cargo.toml has no `version` right under [workspace.package]".into());
    }
    let to = format!("[workspace.package]\nversion = \"{version}\"");
    fs::write(&manifest, text.replacen(&from, &to, 1)).map_err(|e| format!("Cargo.toml: {e}"))?;
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
    git(&["commit", "-a", "-m", &format!("Lobby {version}")])?;
    git(&["tag", "-a", &tag, "-m", &format!("Lobby {version}")])?;
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

//! `cargo xtask check-toolchain`: the C toolchain the media crates need (libopus builds with
//! cmake), checked up front with an actionable message instead of a failure deep inside a build
//! script. It also pins the Docker image's builder
//! (`deploy/Dockerfile`) to the channel in `rust-toolchain.toml`.

use std::{fs, path::Path, process::Command};

struct Tool {
    name: &'static str,
    args: &'static [&'static str],
    why: &'static str,
    install: &'static str,
}

const TOOLS: &[Tool] = &[Tool {
    name: "cmake",
    args: &["--version"],
    why: "builds libopus (Voice)",
    install: "Windows: `winget install Kitware.CMake` or `scoop install cmake`; Debian/Ubuntu: `apt install cmake`",
}];

pub fn check() -> bool {
    let mut ok = true;
    for tool in TOOLS {
        match first_line(tool.name, tool.args) {
            Some(version) => println!("ok       {} ({version})", tool.name),
            None => {
                ok = false;
                println!(
                    "missing  {}: {}\n         install: {}",
                    tool.name, tool.why, tool.install
                );
            }
        }
    }
    ok &= check_dockerfile(&crate::workspace_root());
    ok
}

fn check_dockerfile(root: &Path) -> bool {
    let read = |file: &str| fs::read_to_string(root.join(file)).map_err(|e| format!("{file}: {e}"));
    let verdict = read("rust-toolchain.toml")
        .and_then(|toolchain| builder_matches_toolchain(&toolchain, &read("deploy/Dockerfile")?));
    match verdict {
        Ok(channel) => {
            println!("ok       deploy/Dockerfile builds on rust:{channel}");
            true
        }
        Err(why) => {
            println!("drifted  {why}");
            false
        }
    }
}

fn builder_matches_toolchain(toolchain: &str, dockerfile: &str) -> Result<String, String> {
    let channel =
        toolchain_channel(toolchain).ok_or("rust-toolchain.toml has no `channel = \"…\"` line")?;
    let tag = builder_tag(dockerfile).ok_or("deploy/Dockerfile has no `FROM rust:<tag>` line")?;
    let version = tag.split('-').next().unwrap_or(&tag);
    if version == channel {
        Ok(channel)
    } else {
        Err(format!(
            "deploy/Dockerfile builds on rust:{tag} but rust-toolchain.toml pins {channel}; change the tag to rust:{channel}-bookworm"
        ))
    }
}

fn toolchain_channel(toml: &str) -> Option<String> {
    toml.lines()
        .filter_map(|line| line.trim().strip_prefix("channel"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .next()
}

/// `FROM rust:1.95-bookworm AS build` → `1.95-bookworm`.
fn builder_tag(dockerfile: &str) -> Option<String> {
    dockerfile.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let from = words.next()?.eq_ignore_ascii_case("FROM");
        let image = words.next()?.strip_prefix("rust:")?;
        from.then(|| image.to_owned())
    })
}

fn first_line(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_dockerfile_matches_its_toolchain() {
        assert!(check_dockerfile(&crate::workspace_root()));
    }
}

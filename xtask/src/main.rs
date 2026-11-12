use std::{fmt::Display, path::PathBuf, process::ExitCode};

use xtask::{
    changelog, check_api, check_comments, check_deps, check_kit, check_px, dpdfnet_weights, drive,
    gates, release, toolchain, workspace_root,
};

const USAGE: &str = "usage: cargo xtask <command>

commands:
  check-api
      Fail if lobby-client exports an `async fn`. It reads the public API from rustdoc's JSON, so
      it builds the crate's docs.
  changelog [<version>]
      Print the CHANGELOG.md entry the next release would write from the commits since the last
      tag, and change nothing. Reword a commit first if a line reads badly.
  check-commits
      Fail on a commit subject since the last tag that is not
      `<type>[(<scope>)][!]: <description>` (see AGENTS.md, Commits). Needs the tags fetched.
  check-comments
      Fail on a comment line over 100 columns in the Rust sources of crates/ and xtask/ or in a
      Cargo.toml, on a literal \\n followed by a comment marker inside a comment, or on a comment
      that cites the old design docs.
  check-deps [--manifest-path <Cargo.toml>]
      Fail if a crate's normal dependency closure contains a forbidden package
      (see the rules in check_deps.rs), or if aws-lc-rs/aws-lc-sys appear anywhere (dev and build too).
  check-px
      Fail on a `px(..)` call in the desktop's view files without a `// px: <why>` comment: layout
      sizes are `u(..)` so they follow the UI scale.
  check-kit
      Fail on a `component::` path in lobby-desktop outside src/kit.rs: gpui-kit's styled
      components are imported only through that file.
  check-toolchain
      Check that cmake (needed by libopus) is on PATH and that
      deploy/Dockerfile builds on the channel rust-toolchain.toml pins.
  dpdfnet-weights <model.onnx>
      Write the DPDFNet weights the client embeds (crates/lobby-client/src/media/dpdfnet/) from
      Ceva's dpdfnet2_48khz_hr.onnx, and print the blob's SHA-256 and whether it changed.
  drive <scenario> [--instances a,b] [--no-build] [--visible]
      Run a scenario of crates/lobby-desktop/tests/scenarios/ (a name or a path) against the
      real binaries: a lobby-server on free ports and one `lobby` per client, driven through
      its automation pipe without focus, mouse or Windows toasts. Output in
      target/drive/<scenario>/out/.
  gates
      Run fmt, both clippy runs, nextest, the doc tests and the seven checks in order, print each
      step's wall time and the total, and stop at the first failure.
  dist --repo <owner/name>
      Build the release `lobby` for that GitHub repository's Releases (it installs and updates
      itself from them) and write dist/LobbySetup.exe, dist/latest.json and
      dist/release-notes.md (the newest changelog entry). CHANGELOG.md must have the workspace version on top
      (Windows only).
  release <version>
      Refuse a bad commit subject, write the changelog entry, bump the workspace to <version>,
      commit, tag v<version> and push both to origin. CI builds the tag with `dist` and
      publishes the GitHub Release the apps update from.";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("check-deps") => {
            let mut opts = check_deps::Options::default();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--manifest-path" => match args.next() {
                        Some(path) => opts.manifest_path = Some(PathBuf::from(path)),
                        None => return usage("--manifest-path needs a path"),
                    },
                    other => return usage(&format!("unknown option {other:?}")),
                }
            }
            report("check-deps", "violation", check_deps::check(&opts))
        }
        Some("check-api") => report("check-api", "finding", check_api::check(&workspace_root())),
        Some("check-comments") => report(
            "check-comments",
            "finding",
            check_comments::check(&workspace_root()),
        ),
        Some("check-px") => report("check-px", "finding", check_px::check(&workspace_root())),
        Some("check-kit") => report("check-kit", "finding", check_kit::check(&workspace_root())),
        Some("check-commits") => report(
            "check-commits",
            "bad subject",
            changelog::check(&workspace_root()),
        ),
        Some("changelog") => match changelog::preview(&workspace_root(), args.next().as_deref()) {
            Ok(entry) => {
                print!("{entry}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("changelog: {e}");
                ExitCode::FAILURE
            }
        },
        Some("gates") => {
            if gates::run(&workspace_root()) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("check-toolchain") => {
            if toolchain::check() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("dpdfnet-weights") => {
            let Some(model) = args.next() else {
                return usage("dpdfnet-weights needs the path of dpdfnet2_48khz_hr.onnx");
            };
            match dpdfnet_weights::run(&workspace_root(), model.as_ref()) {
                Ok(done) => {
                    println!("dpdfnet-weights: {done}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("dpdfnet-weights: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("drive") => {
            let opts = match drive::parse_args(args) {
                Ok(opts) => opts,
                Err(e) => return usage(&e),
            };
            match drive::run(&opts) {
                Ok(summary) => {
                    println!("{summary}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("drive: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("dist") => {
            let repo = match (args.next().as_deref(), args.next()) {
                (Some("--repo"), Some(repo)) => repo,
                _ => return usage("dist needs --repo <owner/name>"),
            };
            match release::dist(&workspace_root(), &repo) {
                Ok(out) => {
                    println!("dist: {}", out.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("dist: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("release") => {
            let Some(version) = args.next() else {
                return usage("release needs a version, such as 0.2.0");
            };
            match release::release(&workspace_root(), &version) {
                Ok(done) => {
                    println!("release: {done}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("release: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("help" | "--help" | "-h") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => usage(&format!("unknown command {other:?}")),
    }
}

fn report<F: Display, E: Display>(
    command: &str,
    noun: &str,
    result: Result<Vec<F>, E>,
) -> ExitCode {
    match result {
        Ok(found) if found.is_empty() => {
            println!("{command}: ok");
            ExitCode::SUCCESS
        }
        Ok(found) => {
            for f in &found {
                eprintln!("{command}: {f}");
            }
            eprintln!("{command}: {} {noun}(s)", found.len());
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("{command}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage(problem: &str) -> ExitCode {
    eprintln!("{problem}\n\n{USAGE}");
    ExitCode::from(2)
}

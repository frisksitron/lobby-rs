//! `cargo xtask gates`: every check that must pass before a change is done, in order, each
//! timed, stopping at the first failure.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

enum Step {
    Cargo(&'static [&'static str]),
    Xtask(&'static str),
}

/// The checks that take seconds come first, so a long comment line or a banned dependency is
/// reported before the minutes of clippy and tests. `check-toolchain` leads them because the
/// builds after it need cmake.
const STEPS: [(&str, Step); 12] = [
    ("fmt", Step::Cargo(&["fmt", "--all", "--check"])),
    ("check-toolchain", Step::Xtask("check-toolchain")),
    ("check-comments", Step::Xtask("check-comments")),
    ("check-px", Step::Xtask("check-px")),
    ("check-kit", Step::Xtask("check-kit")),
    ("check-commits", Step::Xtask("check-commits")),
    ("check-deps", Step::Xtask("check-deps")),
    ("check-api", Step::Xtask("check-api")),
    (
        "clippy all-targets",
        Step::Cargo(&[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
    ),
    (
        "clippy shipped",
        Step::Cargo(&["clippy", "--workspace", "--", "-D", "warnings"]),
    ),
    // Not the examples: clippy has checked that they build, and building them takes a minute.
    (
        "nextest",
        Step::Cargo(&["nextest", "run", "--workspace", "--lib", "--tests"]),
    ),
    ("doc tests", Step::Cargo(&["test", "--workspace", "--doc"])),
];

/// Returns whether every step passed. Prints one line per step and the total at the end.
pub fn run(workspace: &Path) -> bool {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let xtask = park_running_exe();
    let started = Instant::now();
    let mut timings: Vec<(&str, Duration)> = Vec::new();
    for (name, step) in &STEPS {
        println!("gates: {name} ...");
        let mut command = match step {
            Step::Cargo(args) => {
                let mut command = crate::workspace_cargo(&cargo);
                command.args(*args);
                command
            }
            Step::Xtask(arg) => {
                let mut command = Command::new(&xtask);
                command.arg(arg);
                command
            }
        };
        let step_started = Instant::now();
        let passed = command
            .current_dir(workspace)
            .status()
            .is_ok_and(|status| status.success());
        let took = step_started.elapsed();
        timings.push((name, took));
        if !passed {
            print_timings(&timings);
            eprintln!(
                "gates: {name} failed after {:.1} s ({:.1} s in all)",
                took.as_secs_f64(),
                started.elapsed().as_secs_f64()
            );
            return false;
        }
    }
    print_timings(&timings);
    println!("gates: ok in {:.1} s", started.elapsed().as_secs_f64());
    true
}

/// The test build relinks `xtask.exe`, which Windows refuses while it runs. A running exe can be
/// renamed, so move it aside and run the checks from there. Returns its new path.
fn park_running_exe() -> PathBuf {
    let exe = std::env::current_exe().expect("xtask knows its own path");
    let parked = exe.with_file_name("xtask-gates.exe");
    let _ = std::fs::remove_file(&parked);
    match std::fs::rename(&exe, &parked) {
        Ok(()) => parked,
        Err(_) => exe,
    }
}

fn print_timings(timings: &[(&str, Duration)]) {
    for (name, took) in timings {
        println!("gates: {name:<18} {:>7.1} s", took.as_secs_f64());
    }
}

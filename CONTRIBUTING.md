# Contributing to Lobby

Bug reports and fixes are welcome. Before you start a larger change, open an issue that says what
you want to change and why, so that we agree on the approach before you write the code.

To report a security problem, follow [SECURITY.md](SECURITY.md) instead of opening an issue.

## Build and run

Lobby is built on Windows first. The desktop app builds on Linux too, but Screen Share and Watch
work only on Windows. macOS is not supported.

You need:

- Rust, the version in `rust-toolchain.toml`. rustup installs it on the first build.
- cmake on `PATH`.
- On Windows, the MSVC build tools.

To run a local server and two desktop clients, follow "Develop" in [README.md](README.md).

## Run the checks before you push

Run every check on your own machine before you open a pull request:

```sh
cargo xtask gates
```

`gates` runs the formatter, the repository checks, clippy, the tests and the doctests in order,
and stops at the first failure. It takes a few minutes. CI runs the same checks on Windows and
Linux, and a pull request merges only when they pass with no warnings.

While you work, run only the tests of the crate you changed, for example
`cargo nextest run -p lobby-client`. Run `cargo xtask gates` once, at the end.

## Follow the project rules

[AGENTS.md](AGENTS.md) holds the rules for every change, whether a person or a coding agent wrote
it. It covers the domain words, the dependency rules, the code style and what to test. Read it
before your first pull request.

If you change the desktop UI, check it in the default window (680x480) and in the small window
(480x400). The component gallery shows each component in its states:

```sh
cargo run -p lobby-desktop --features gallery --bin lobby-gallery [story]
```

## Licence

Lobby is licensed under either of the MIT licence or the Apache License 2.0, at your option.
Unless you say otherwise, any contribution you submit for inclusion in Lobby, as defined in the
Apache-2.0 licence, is licensed the same way, with no extra terms or conditions.

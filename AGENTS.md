# Working in lobby-rs

Rules for anyone changing this repository, human or agent. The code is the source of truth: there
is no separate spec, and the Rust types in `lobby-protocol` are the wire contract.

## Product

- Lobby is for friend groups who know each other and host their own server. Every Member sees
  everything in the Lobby. The first Member is the owner, and can hand ownership to another Member.
- Security effort goes to the front door: sign in, Magic Codes, admission (Vouch), sessions, rate
  limits and TLS. There is no encryption at rest and no end-to-end encryption. Elsewhere, prefer
  simplicity over hardening.
- Use the domain language: Lobby, Member, owner, Vouch, Ask to Join, Voice, Screen Share, Watch,
  Issue, Magic Code. The Lobbies a person has added on this device are "Lobbies", never
  "Known Lobbies".
- Make the desktop app delightful, and keep it usable in the small window (480×400). Build UI from
  `lobby-desktop/src/components` and the controls in `lobby-desktop/src/ui` before writing new
  ones. `ui` draws Lobby's look on gpui-kit's unstyled `base` layer, and a control enters it once
  two components use it. `ui` imports nothing from `components`, `lobby_client` or
  `lobby_protocol`.
- gpui-kit's styled layer (`gpui_kit::component`) is reached only through
  `lobby-desktop/src/kit.rs`, which keeps what `base` has no primitive for: the theme, menus, the
  select, dialogs, toasts, the title bar and the message scroller. `cargo xtask check-kit`
  enforces this.
- Never copy code from other projects into this repository. They are for ideas only.

## Dependency rules

`cargo xtask check-deps` enforces these.

- gpui appears only in `lobby-desktop`. sqlx and axum appear only in `lobby-server`.
- `lobby-protocol` has no tokio, no reqwest and no IO. The server never depends on reqwest or cpal.
- ring is the only rustls provider: `aws-lc-rs` and `aws-lc-sys` must not appear.
- Exact versions are pinned in the root `Cargo.toml`. Change a pin only on purpose.
- Only `lobby-desktop/src/main.rs` builds a Tokio runtime. Tests build their own.
- `lobby-client` exports no `pub async fn` outside its `testing` module (feature `test-support`).
  IO entry points spawn on the client's runtime and return a `ClientTask` or a channel.
  `cargo xtask check-api` enforces this one.
- Add a trait only when two implementations exist in the repository.

## Code style

- Errors are domain enums with `thiserror`, and map to the wire once, at the edge. Use `anyhow` only
  in `main` functions and tests.
- Use plain structs with explicit constructors, and one composition root per binary.
- One place decides each rule. A running Lobby has one Session, owned by its driver task. Views
  act through `Intent`s on its `SessionHandle` (Entry's the same way) and read the snapshot;
  they never decide again what the Session decides. Tests go in by Intents too.
- Every server task runs in a `TaskTracker` tier with a `CancellationToken`. Every client task is
  owned by a handle whose `Drop` cancels it. Cleanup that must finish is an awaited `shutdown()`.
- Never hold a `std::sync::MutexGuard` across `.await`.
- Timeouts, retries and limits are named constants or config values.
- Desktop layout sizes are `u(px)` from `crate::scale`: rems, so they follow the UI scale. A bare
  `px(..)` in a view file needs a `// px: <why>` comment (`cargo xtask check-px`).
- Secrets live only in the `lobby-protocol` secret newtypes, whose `Debug` prints `***`. Log emails
  as their domain only.
- Write a comment only when the code cannot explain itself: a non-obvious why, a constraint or a
  trap. Write it in plain words, within 100 columns: rustfmt does not wrap comments, and
  `cargo xtask check-comments` fails on a longer line.

## Tests

- Test the main user flows, each once: sign in with a Magic Code, bootstrap, Vouch
  and Ask to Join, chat with attachments, Voice join, mute and leave, Screen Share and Watch,
  owner actions, reconnect, and the small window. They are the desktop scenarios in
  `crates/lobby-desktop/tests/scenarios/` and client flows against the real server. Breakage
  anywhere else is fixed when it is found, so do not add a test to guard it.
- Test deep modules at their boundary, such as the Voice state machine, the SFU, the Markdown normaliser
  and file-type checks. Do not test small helpers, tuning or timing on their own.
- A test never asserts a frame rate, a bitrate or a duration measured on the real clock.
- The suite runs in under two minutes and `cargo xtask gates` in under five. A test that fails
  once without a code change is deleted or made deterministic, never retried.
- While iterating on UI in the gallery, only build `lobby-gallery` and open the page
  (`lobby-gallery "<story name>"`). Run checks once the design is settled, not per tweak.
- Tests never wait out a real timer. Use a paused clock or shorter config.
- nextest runs each test in its own process, so tests share nothing through statics. A test that
  needs the machine to itself, such as the hardware encoder, joins the `real-media` group in
  `.config/nextest.toml`.
- Every server integration test ends with `assert!(server.shutdown().await.is_clean())`.

## Commits

A subject is `<type>[(<scope>)][!]: <description>`. The commits since the last release become its
`CHANGELOG.md` entry and the Release's description. `cargo xtask check-commits` enforces the
format, and `cargo xtask changelog` previews the entry.

- `feat` adds something a person using Lobby can do. `fix` fixes something that was broken.
  `perf` makes something faster or lighter. These three reach the changelog.
- `refactor` changes structure, not behaviour. `style` is formatting. `test` is tests. `docs` is
  documentation. `build` is the build, dependencies and releases. `ops` is CI, deploy and
  hosting. `chore` is anything else. None of these reach the changelog.
- `!` after the type marks a breaking change, and lists it first under Breaking.
- The scope is optional: 1 to 20 lowercase letters, digits or hyphens, such as `voice`. It is not
  shown.
- The description is imperative, starts lowercase and has no period. For a type the changelog
  shows, write it for someone using Lobby: what changes for them, not which code moved.
- A follow-up to work not yet released is folded into its commit, or typed `chore`: it is news
  to no one.
- A `Revert "..."` subject is valid and never shown.

## Commands

```sh
cargo fmt --all --check
cargo xtask check-toolchain
cargo xtask check-comments
cargo xtask check-px
cargo xtask check-kit
cargo xtask check-commits  # the subjects since the last tag
cargo xtask check-deps
cargo xtask check-api
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace -- -D warnings  # the shipped build: no tests, no automation feature
cargo nextest run --workspace --lib --tests  # not the examples: clippy checks they build
cargo test --workspace --doc  # nextest skips doctests
```

All of them must pass with no warnings before a change is done. `cargo xtask gates` runs them in
this order, prints each step's time and stops at the first failure. The checks that take seconds
come first, so a long comment line is reported before the minutes of clippy and tests. Use the single workspace
target directory (`./target`).

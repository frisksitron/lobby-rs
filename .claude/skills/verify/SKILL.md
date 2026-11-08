---
name: verify
description: Drive the lobby-rs desktop app (the `lobby` GPUI client against a real `lobby-server`) the way a Member does and capture proof, headless and without touching the person's desktop. Use to prove a UI change, reproduce a desktop bug, user-test or critique a change, or check the small window, before calling desktop work done.
---

# Verify lobby-rs

The surface is the Windows desktop app `lobby` (crate `lobby-desktop`) talking to `lobby-server`. You drive it with JSONL scenarios through the app's automation channel. That works in two places: in process (Layer 3) and against the real binaries (Layer 4). The scenario language is the header comment of `crates/lobby-desktop/src/automation/scenario.rs`. This skill is the short path and the rules.

Hard rules (the person uses this PC while you work):

- Never use SendInput, move the mouse, take the focus, or raise a window. The harness never does. Do not add tools that do.
- Never sleep or wait out a real timer (idle after 10 min, toast expiry, backoff). Wait for something the person would see. Prove timers in Layer 1 with `advance_clock`, or Layer 3 with `{"advance": ms}`.
- Kill only processes you started, by PID. Never by image name: another agent's drive or the person's own `lobby` may be running.
- Check the UI at 680×480 (default) and at 480×400 (minimum).

## Launch

There is no long-lived instance to start. Each run starts and stops its own server and clients.

- **Layer 3, in process (default, fastest).** `LOBBY_SCENARIO=<part of a name> cargo nextest run -p lobby-desktop --test e2e_ui`. Each scenario runs a real in-process `lobby-server` behind a chaos proxy with an SMTP sink, plus one real `Shell` per client. Ready means the test started; there is nothing to poll. Add `LOBBY_RENDER_TESTS=1` for pictures, which needs a Vulkan or GL driver. It supports everything, including `network`, `advance`, `freeze`/`thaw` and `{code:<email>}`.
- **Layer 4, real binaries.** `cargo xtask drive <name-or-path> [--instances a,b] [--no-build] [--visible]`. It builds `lobby-server` and `lobby --features automation` into `./target` unless `--no-build` is given. It starts the server on free ports with scratch data, then one `lobby` per client with instance id `tV-drive-<driver pid>-<client>`. Each window opens off screen without activation. Ready is the line `drive <name>: server http://127.0.0.1:<port>, clients a, b (pipes tV-drive-<pid>-*)`. Each step then prints `ok` or `FAIL` with its line number. Use Layer 4 for DirectX and DirectWrite pixels, real Quit and restart, Credential Manager, window resizes and real window sizes (`drive_only` scenarios). Its servers mail into the driver's own SMTP sink, so `{code:<email>}` works there too. It does not support `freeze`/`thaw`, `network` or `advance`. It drives only a server of its own: a deployed Lobby's mail is out of its reach, so nothing could sign in there.
- **Teardown** is automatic. The driver quits each client through the Quit flow, stops the server, and removes its Credential Manager entries. The next run removes entries left by drivers that died.

Use one shared target dir (`./target`). In a git worktree, set `CARGO_TARGET_DIR` to the main checkout's `target` so the build reuses compiled dependencies.

## Doctor

Run this first whenever anything looks off (a pipe that will not open, a stuck drive, stale pictures):

```powershell
pwsh -NoProfile -File .claude/skills/verify/scripts/doctor.ps1
```

It is read-only. It reports:

- the toolchain check;
- whether the drive binaries are older than the newest source change;
- `lobby` and `lobby-server` processes running from this repo's `target`, with their PIDs, start times and command lines;
- running `xtask drive` processes;
- leftover `lobby-rs/tV-drive-*` Credential Manager entries.

A driven client shows `LOBBY_AUTOMATION` only in its environment, so the doctor cannot tell whose it is. A process you did not start is not yours to kill.

## Drive

1. Find the scenario that covers the behaviour: `features/README.md` here maps features to scenarios, and each scenario's header line says what it proves.
2. Run the existing scenario first (Layer 3, then Layer 4 if the change touches pixels, windows or process lifecycle).
3. For a new check, write a scenario.
   - A permanent one goes in `crates/lobby-desktop/tests/scenarios/<flow>-<behaviour>.jsonl`, whose header's `scenario` says what it proves. It then runs in every `cargo nextest run --workspace`.
   - A throwaway exploration goes in `target/verify/<name>.jsonl`. Never put it under `target/drive/`: a drive deletes `target/drive/<name>/` first.
   - Copy the sign-in prefix from `chat.jsonl`. A Member `alice` has the email `alice@lobby.test`, and the address field starts empty:

     ```json
     {"scenario": "…", "clients": ["a", "b"], "members": ["alice", "bob"]}
     {"on": "a", "type": "{lobby}", "into": "lobby-url"}
     {"press": "enter"}
     {"wait": {"path": "/auth/step", "eq": "Email"}}
     {"type": "alice@lobby.test", "into": "email"}
     {"press": "enter"}
     {"wait": {"path": "/auth/step", "eq": "Code"}, "timeout_ms": 5000}
     {"type": "{code:alice@lobby.test}", "into": "code-input"}
     {"wait": {"path": "/lobby/phase", "eq": "connected"}, "timeout_ms": 5000}
     ```
4. Handles are element ids, never coordinates. To see what exists, add `{"dump": "x"}`. The dump has `/elements/<id>` (bounds, visibility, accessible name) and the view model (`/route`, `/auth/step`, `/lobby/phase`, `/lobby/members/<name>/…`, `/lobby/chat/rows`, `/lobby/connection`, `/lobby/voice/controls`, `/issues`, `/lobbies`, `/window/width`, …). An element without an id needs `.test_support()` in its builder. Icon-only buttons need an `accessibility_label`, which is their name in the dump.
5. "Nothing moves" is `{"mark": "m"}` … `{"wait": {"unchanged": "<id>", "since": "m"}}`. Window size changes go through `{"action": "resize:480x400"}`, or a header `"size": [480, 400]`.

## Evidence

Where proof lands:

- **Layer 3.** `target/ui-shots/scenarios/<scenario>/`: `transcript.jsonl` (every step, client, time and failure), `<client>-failed.json` (the dump a failed wait saw), `<client>-<name>.png|json` for `shot` and `dump` steps.
- **Layer 4.** `target/drive/<scenario>/out/` holds the same files. `target/drive/<scenario>/server/server.log` and `clients/<client>/logs/` hold the processes' logs.
- **Layer 2 crops.** `target/ui-shots/<test>/<size>/<step>.png`.

What counts as proof:

- **Drive the user path.** Clicks, typing and key presses on the real views. No internal setters and no test-only endpoints.
- **Capture the action and the state it produced.** Use a `wait`/`expect` on the model, plus a `shot` of the region, not just the last screen.
- **Check side effects on the other client.** For anything a Member shares, the second client must see it: wait on client `b` too.
- **Read the shots.** Open the PNGs. Layer 2 and 3 draw with the bundled Figtree in all four weights, on wgpu and Cosmic text; judge how text weight and hierarchy come out on the real DirectWrite on Layer 4 shots.
- **Name the exact command, the scenario file and the output folder** in your report.

A run is a pass only if every step printed `ok` (Layer 4) or the test passed (Layer 3). "Inconclusive" or a check on the wrong layer is not a pass.

## Cleanup

- A finished drive cleans up after itself. After a drive you interrupted, run the doctor. End only the `lobby`/`lobby-server` PIDs your run started (the drive's ready line names its `tV-drive-<pid>` prefix): `Stop-Process -Id <pid>`. The next drive removes stale Credential Manager entries.
- Evidence survives cleanup. A rerun of the same scenario name replaces `target/drive/<name>/`. Before re-running, copy `out/` to `target/verify/evidence/<name>-<label>/` if you need the earlier pictures.
- Delete throwaway scenarios in `target/verify/` when done. Keep the evidence.

## Helpers

- `.claude/skills/verify/scripts/doctor.ps1`: read-only health check, see Doctor.
- `xtask/scripts/focus-watch.ps1`: logs the foreground window and cursor during a run, to prove nothing took the focus.
- `xtask/scripts/resize-check.ps1`: 200 rapid resizes and maximize/restore with PrintWindow. Run it after changes near the window, the renderer or gpui-kit's version.

## Feature map

`features/README.md` indexes the user-facing features with their recipes and gotchas. Keep it honest with `/maintain-verification-skill`.

What still needs the person, not this skill: hearing audio and a real microphone, a real sleep and resume, IME input, the OS idle signal, a VPS and a second network.

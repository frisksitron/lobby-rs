# lobby-rs verification map

The maintained source for verifying what a Member sees and does in the desktop app. Read this index before driving the app, then use the matching feature file as the recipe. Commands, layers and rules are in `../SKILL.md`.

## Baseline preconditions

- Every run starts from nothing. The harness creates the server, its database, its mail sink and the Members named in the scenario header (`members`; `alice` has the email `alice@lobby.test` and signs in with `{code:alice@lobby.test}`). Each client gets an empty data directory.
- Layer 3: `LOBBY_SCENARIO=<name> cargo nextest run -p lobby-desktop --test e2e_ui`. Layer 4: `cargo xtask drive <name>`.
- Run `../scripts/doctor.ps1` when a run misbehaves. Never drive or kill a `lobby` this run did not start.

## Scenarios in the suite

Only 18 scenarios in `tests/scenarios/` run in `cargo nextest run --workspace`, one for each main flow. A feature file can name a scenario that is not in the suite. To check that behaviour, write the scenario again from the feature file's recipe in `target/verify/`, or add it to the suite with the change that needs it.

## Driving conventions

- Start from the sign-in prefix in `../SKILL.md`, then act through element ids (`click`, `type` + `into`, `press`) and check through the model (`wait`/`expect` on dump paths).
- Wait on client `b` for anything other Members should see.
- Check small layouts with a header `"size": [480, 400]` or `{"action": "resize:480x400"}`.
- Never sleep and never wait out a real timer.

## Proof and skip reporting

- Each proof names the scenario file, the layer, the command and the output folder.
- Pair every action with a model check and, for visual claims, a `shot` of the region.
- Report a path the harness cannot reach (audio, real sleep, IME, OS idle, VPS) as not verified, and name the person check it needs. Never report it as verified through a nearby path.

## Feature entry contract

Each feature file has an H1 title and one paragraph, then exactly four H2 sections in order: `Sub-features`, `How to get to it (user POV)`, `Driving it with the scenario harness` (starting with `Preconditions:`), `Gotchas`.

## Features

- [Sign in and admission](./sign-in-and-admission.md): add a Lobby URL, Magic Code sign in, the step rail, set up a Lobby, Ask to Join and Vouch, Sign Out.
- [Chat](./chat.md): send and receive Messages, rich text, Load Older, typing, backfill, send failure.
- [Connection and presence](./connection-and-presence.md): the title bar connection state, reconnect, Idle and Do Not Disturb, the notification center.
- [Voice and Screen Share](./voice-and-screen-share.md): join, mute and leave Voice, share a screen, watch and stop watching.
- [Lobbies and window](./lobbies-and-window.md): restore on start, switch Lobbies, a corrupt state file, the small window and resizes.
- [Settings and appearance](./settings-and-appearance.md): the Settings page, the UI size, the one dark theme, Close to tray.

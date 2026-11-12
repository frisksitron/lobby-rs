# Sign in and admission

A person enters a Lobby's address, then an email, then the six-digit Magic Code mailed there. The Lobby then decides: a Member is signed in, a request already filed waits for a Vouch, the bootstrap email of a Lobby without Members names itself and sets it up, and anyone else picks a name and Asks to Join. There are no passwords. A signed-in Member can Sign Out of this device only.

## Sub-features

- `url-step` enters the Lobby address and refuses a remote `http://` Lobby.
- `magic-code` signs a Member in with an emailed code. The cells submit by themselves once the sixth digit is in.
- `rail` is the row of step chips across the top (Lobby › Email › Code › Name). A done chip shows what was settled; pressing the Lobby or Email chip goes back to that step.
- `bootstrap` lets a fresh Lobby's owner name itself and set the Lobby up, without a Vouch. Anyone earlier lands on "{name} isn't open yet".
- `vouch` covers Ask to Join with a name, Waiting for a Vouch, and a Member Vouching from the Decision list, live on the other client.
- `sign-out` ends this device's session only.

## How to get to it (user POV)

- The first window: the Lobby address field (empty on a first start), then Connect.
- The email step ("Welcome to {name}"), then the code step ("Check your email").
- The Decision list in the notification center, for a Member Vouching someone in.
- The Member Card on your own row in the sidebar (right-click), for Sign out.

## Driving it with the scenario harness

Preconditions:

- The header lists the clients and the existing Members. A Member `alice` has the email `alice@lobby.test`. `bootstrap_email` and `join_requests` set those server options.

- **Connect.** `{"type": "{lobby}", "into": "lobby-url"}`, then `{"click": "connect"}` or `{"press": "enter"}`, then wait `/auth/step` = `Email`. Errors show at `/url_step/error`. A scheme-less loopback address (`127.0.0.1:<port>`) connects over http.
- **Magic Code.** Type into `email`, `{"click": "send-code"}` (or `enter`), wait `/auth/step` = `Code`, then type `{code:<email>}` into `code-input`. A Member lands on `/lobby/phase` = `connected`. Both layers: Layer 3 reads the in-process sink, Layer 4 the driver's own.
- **Name.** After the code a new email waits at `/auth/step` = `Name`, with the name suggested from the email at `/auth/name`. Type into `name`, then `{"click": "join"}` or `enter`. The bootstrap email is signed in; anyone else goes to `AwaitingVouch`.
- **Rail.** `/auth/rail` lists the chips (`stage`, `at` = `done`, `current` or `ahead`, `value`, `back`). Press `chip-lobby` or `chip-email` to go back.
- **Vouch.** On `b`, Ask to Join with a new email and a name. On `a`, `{"click": "notification-bell"}` then `{"click": "vouch-carol"}`. On `b`, `{"click": "check-again"}`. Both clients then show carol at `/lobby/members/carol/presence`.
- **Sign Out.** `{"right_click": "member-<you>"}`, then `{"click": "sign-out"}`. Wait `/auth/step` = `Email` on that client; the other client stays signed in.
- **Proof.** `sign-in-and-presence`, `sign-in-and-presence-small`, `setup-first-member`, `ask-to-join-and-vouch`. Add a `shot` of `auth` for visual checks.

## Gotchas

- A second Lobby's codes are `{code2:<email>}` in Layer 3, which keeps one sink per server. Layer 4 has one sink for both, and `{code2:…}` reads it too.
- Layer 4 clients keep tokens in the real Credential Manager. A reopened client skips Sign In by design, not a bug.
- Copy refers to Lobbies, never "Known Lobbies".

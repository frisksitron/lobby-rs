# Connection and presence

The app shows connection state by the title bar's tint and one live entry at the top of the notification center, and recovers by itself. Members see each other online, Idle or Do Not Disturb. Errors are toasts that the notification center keeps.

## Sub-features

- `presence` shows both Members online within 1 s, and offline within 1 s of a close.
- `reconnect` covers a server restart: Connection lost with Try now, then recovery with full history.
- `half-open` replaces a dead socket within seconds.
- `wake` checks the connection at once after a wake.
- `unavailable` shows Unavailable after ten failures, and Try now in the center brings it back (Layer 3 only).
- `idle-dnd` shows Idle and Do Not Disturb on the other client.
- `one-home` keeps connection state to the tint and the center's entry. Nothing moves when it drops and returns.
- `notifications` keeps toasts in the center, with the same action.

## How to get to it (user POV)

- The title bar's tint, and the connection entry in the notification center with Try now when shown. Only Offline, Unavailable, Signed in elsewhere and Update required also toast.
- The Member Card on your own row (right-click): the Do Not Disturb switch and Sign out.
- The bell in the title bar opens the notification center.

## Driving it with the scenario harness

Preconditions:

- Two clients signed in. Header `heartbeat_ms` shortens heartbeats where a scenario needs it.

- **Presence.** On `b`, wait `/lobby/members/alice/presence` = `online`. After `{"close": "a"}` (Layer 4) it reads offline.
- **Server restart.** `{"server": "stop"}`, wait `/lobby/connection/title` = Connection lost (`/lobby/connection` is null while calm), open the bell and `{"click": "connection-try-now"}` or `{"server": "start"}`, then wait `/lobby/phase` = `connected`.
- **Freeze.** `{"server": "freeze"}` / `"thaw"` makes a half-open socket (Layer 3 only).
- **Wake and idle.** `{"action": "wake"}`, `{"action": "idle"}`, `{"action": "active"}` are injected host events, never the person's input.
- **DND.** `{"right_click": "member-alice"}` (a right-click on any row opens its Member Card at the pointer; a left-click opens nothing, and Esc closes the card), `{"click": "presence-dnd"}` (the switch: `/lobby/member_panel/own/dnd`; click it again to turn it off), then wait on `b`.
- **Nothing moves.** `{"mark": "m"}` before the drop, then `{"wait": {"unchanged": "member-list", "since": "m"}}` after the return.
- **Proof.** `sign-in-and-presence`, `sign-in-and-presence-small`, `reconnect-server-restart` (the `t4-*` scenarios are out of the suite, see the README). Take a `shot` of `title-bar` or `notification-center`.

## Gotchas

- `t4-unavailable` leans on the tests' short reconnect policy and is `layer3_only`.
- Never wait out the 10-minute idle. Inject `idle`.
- Real sleep/resume and the OS idle signal need the person.

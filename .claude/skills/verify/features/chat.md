# Chat

Members talk in one shared feed in group-chat style: grouped rows with day dividers, rich text, a composer with a formatting bar, typing on the other side, older history on demand, and what was missed while offline.

## Sub-features

- `send` delivers a Message to both clients in one order. Markup stays literal text, and Shift+Enter makes a new line.
- `rich-text` renders bold, lists, code and links. A link click is recorded, never opened in a browser.
- `load-older` keeps what was read in sight (the first Message in view goes to the top) down to the beginning of history.
- `typing` shows on the other client, again right after a send.
- `backfill` shows a client that was offline the Messages it missed.
- `send-timeout` restores the draft with an Issue.
- `jump` shows the "↓ New Messages" pill and the jump button without covering text at 480×400.

## How to get to it (user POV)

- The composer at the foot of the Lobby view: type and press Enter.
- The formatting bar, which folds into an "Aa" menu in the small window.
- Scroll up the feed to the Load Older control.

## Driving it with the scenario harness

Preconditions:

- Two clients signed in (the sign-in prefix). For history, the header's `history` (`{"author": "bob", "count": 120}`) stores Messages before the server starts.

- **Send.** `{"type": "hello", "into": "composer"}`, `{"press": "enter"}`, then wait `/lobby/feed` contains `{"author": "alice", "text": "hello", "pending": false}`. Do the same wait on `b`.
- **Rows.** `/lobby/chat/rows` holds dividers, groups and pending states. Also check `/lobby/chat/at_bottom`, `/lobby/unseen` and `/lobby/chat/typing`.
- **Formatting.** `{"click": "format-bold"}` (`format-menu` when small), then check `/lobby/formats_on`.
- **Load Older.** `{"scroll": "feed-list", "to": "load-older"}`, `{"click": "load-older"}`, then wait for the first Message that was in view to be `visible` and `load-older` to be `gone`. The kit scroller scrolls to a row's top only, so the rows move up by what sat above them: `unchanged` no longer holds.
- **Backfill.** Layer 3 `{"network": "down"}` on a client in `own_paths`, send from the other client, then `{"network": "up"}`. Or close and reopen the client in Layer 4.
- **Timeout.** Layer 3 `{"advance": 5100}` past the send deadline, then check `/lobby/composer` and `/issues`.
- **Proof.** `chat`, `chat-load-older` (the other `t9-*` scenarios are out of the suite, see the README). Take a `shot` of `feed-list`, `composer` or `typing-line`.

## Gotchas

- The feed is virtualized. Use `scroll` to bring a row into view before clicking or shooting it.
- Hover times and tooltips appear only with `{"shot": …, "hover": true}`. Shots otherwise move the pointer out of the window.
- Typing expiry and send timeouts are timers. Use `advance` (Layer 3) or Layer 1 clock tests, never a wait.
- Long text and toasts must stay inside their bounds at 480×400. Check crops, not only the model.

# Voice and Screen Share

Members join the Lobby's Voice, mute, and leave. A Member in Voice shares a screen; others watch it in the viewer panel and stop watching. The Sharer's own share is one more share to show: drawn from this computer, it opens in the panel when they start sharing (unless they watch someone already), and closing its view keeps sharing. Leaving Voice stops one's own share.

## Sub-features

- `join-leave` shows both In Voice; leaving shows within 1 s. Joining an empty Voice moves no row.
- `mute` shows on both clients.
- `quit-in-voice` makes quitting leave Voice for everyone.
- `share` lets A share (the test pattern source) and B see it within 1 s.
- `watch` shows a picture within 3 s; stopping from the Member row's tile works.

## How to get to it (user POV)

- The Voice section in the sidebar, with controls at its foot (the Voice strip in the small window).
- The share button opens the source picker.
- The watch tile at the end of a sharing Member's row (`watch-<name>`), one's own row included. Pressing it watches; the share on screen fills its tile, and pressing that one again stops watching. Out of Voice, and while Voice connects, the tile is a disabled outline with the tooltip "Join Voice to watch". Nothing but Viewers' ink is laid over the video.
- Whose share shows is chosen by the tiles only (the member list is covered in fullscreen, so leave it first). A press on another tile while one is on screen asks for it: that tile reads `coming` until its first picture and the first stays on screen; a press on the filled tile meanwhile takes the switch back.
- The toolbar over the picture's top on a dark fade (`share-toolbar`), always shown, all at the right: in fullscreen while sharing, Stop Sharing (`viewer-stop-share`); the draw control; fullscreen (`share-fullscreen` / `share-fullscreen-exit`); Close at the far right (`share-close`). The draw control is "Let others draw" with a kit switch on one's own share (`share-let-draw`; the word turns it too), or "Draw" on a friend's share with drawing on (`share-pen`), and nothing on one without. Only the buttons take the pointer: ink drawn on the fade beside them reaches the picture. In the panel the picture's top is the member sheet's top.

## Driving it with the scenario harness

Preconditions:

- Two clients signed in. Media is fake (test tone, test pattern source `picker-test-0`), so no microphone or real capture is needed.

- **Join.** `{"click": "voice-join"}`, then wait `/lobby/voice/controls` = `in_voice` (`timeout_ms` 8000). On the other client, wait `/lobby/members/alice/in_voice`.
- **Mute and leave.** `{"click": "voice-mute"}` then `/lobby/members/alice/muted` on `b`. `{"click": "voice-leave"}` leaves.
- **Share.** `{"click": "share-screen"}`, wait `/elements/picker-test-0` exists, `{"scroll": "source-picker-list", "to": "picker-test-0"}`, `{"click": "picker-test-0"}`, then wait `/lobby/voice/share` = `live`. `picker-test-3` is fake media's window that blocks capture: greyed, not pickable (`share-blocked-window*`). A share of it ends at once with an Issue (lobby-client's `integration` tests, `share::`).
- **Watch.** On `b`, wait `/lobby/members/alice` contains `{"sharing": true, "tile": "rest"}`, `{"click": "watch-alice"}`, wait `/lobby/viewer/shown` ne 0, `{"shot": "watching", "id": "viewer-panel"}`. `/lobby/members/<name>/tile` reads `faint`, `rest`, `coming` or `on_screen` (`null`: no tile). A second `{"click": "watch-alice"}` (the filled tile) clears `/lobby/watching`.
- **Take back a switch.** Watching alice, `{"server": "freeze"}`, `{"click": "watch-carol"}`: `/lobby/watch_switching` is true and carol's tile `coming`; `{"click": "watch-alice"}` takes it back. Proof: `t13a-take-back-a-switch*` (Layer 3 only, out of the suite, see the README).
- **Own share.** After Share, wait `/lobby/viewer/whose` = `mine` (another Member's share reads their name). `/lobby/sharers` lists `You` first while sharing.
- **Ink.** The Sharer, viewing their own share, clicks `share-let-draw`. A Watcher clicks `share-pen` (shown only while drawing is on) to enter drawing mode, then `{"pen": "down", "at": [0.25, 0.5]}`, `{"pen": "move", "at": [...]}`, `{"pen": "up"}`: the left button on `ink-canvas`, at fractions of the picture (from -0.5 to 1.5: outside 0..1 lands on the letterbox), each its own step, with `{"advance": ms}` between (a batch goes 50 ms after the first point). Read `/lobby/ink` (the board: `can_draw`, `drawing_on`, `pen_down`, `strokes[]` with `author`, `points`, `runs`, `lifted`, `fade` = `full` or `fading`) and `/lobby/viewer/pen` and `/lobby/viewer/ink` (what the view paints: `strokes[]` with `author`, `color`, `fade`; `tags[]`, the names at the tips; the tags are elements `ink-tag-<name>`). The Sharer's own view has no `ink-canvas`. Esc in fullscreen ends a stroke first. Proof: `ss2-s15-pen-across-steps`, `screen-share-viewers-draw*`.
- **Ink overlay (sharing a monitor or a window, Layer 4 only).** Sharing `picker-screen-<n>` opens an overlay window over that monitor, and `picker-window-<n>` one just above that window, which follows it and hides while it is minimised or cloaked; `/lobby/ink/overlay` reads `null` otherwise, else `over` (`monitor`, `window`), `state` (`opening`, `ready`, `failed`), `shown` (only while a share it paints has drawing on) and `panes` (`whole` for one's own share, `picture` for a watched share whose Lobby window is on the shared monitor). `share-let-draw` is disabled until `ready`, and stays disabled on `failed`, with a tooltip that says why; `LOBBY_OVERLAY_FAULT=exclusion` in the drive's environment fails it on purpose. While sharing a monitor no Lobby window on it paints ink (`/lobby/viewer/ink/strokes` is empty). A scenario that needs its Lobby window on the shared monitor says `"visible": true` in its header. The overlay is left out of every capture, so no shot shows it: `cargo run -p lobby-desktop --example overlay_check -- <dir>` checks a monitor share, a watched picture's place and a window share (moved, resized, minimised, restored, raised) with WGC captures, the overlay left out and let back in (it shows windows on the person's screen for several seconds; run it only with their leave). Proof: `ss2-s18-share-monitor-while-watching*`; unit proof of the routing: `lobby_view::ink::tests`.
- **Source size.** The Sharer reports the size of what is captured with its first picture and when it changes (a shared window resized), at most every 500 ms; each new size moves the drawing epoch, so a stroke ends and the boards clear, and the Lobby drops ink sent at the old epoch. Its tests (`e2e_ink`, `ink_session`) are out of the suite; they are in git history.
- **Proof.** `voice-join-leave`, `voice-mute-and-quit`, `screen-share`, `watch-and-fullscreen`, `watch-own-share`, `screen-share-viewers-draw`, `voice-bar-small`. Shots of `voice-section`, `voice-strip`, `source-picker`, `viewer-panel`.

## Gotchas

- A live video shot never settles. Layer 4 takes it after 1 s and the answer says `"settled": false`; that is expected.
- Hearing decoded audio and a real microphone need a person. Do not claim audio quality from the harness.
- Real window capture enumeration (WGC) is Layer 4 only.
- A `shot` moves the pointer out of the window with no button held, which the canvas takes as the button coming up: a stroke ends. Shoot a client whose pen is down with `"hover": true`.

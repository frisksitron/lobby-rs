# Lobbies and window

The app remembers the Lobbies a Member added on this device and restores the last one on start. The Member switches between Lobbies from the title bar without signing in again. The window works from 680×480 down to the small 480×400 minimum.

## Sub-features

- `restore` reopens an entered Lobby connected with tokens, or prefilled without.
- `switch` moves between two Lobbies and back with no Sign In; switching while In Voice asks first.
- `add-lobby` opens a dialog from the switcher; the person stays where they are.
- `lobby-menu` is every Lobby's own menu, beside its row in the switcher (its ⋯ or a right-click on the row): Change icon… (an OS file picker; inline Remove while it has an icon) and Leave Lobby…. It works on every Lobby, not only the open one: another Lobby's action runs on a short-lived Session of its own (an errand: no Lobby view, no Voice, no OS toasts or sounds), so the window and Voice stay put. Leave asks once ("Leave {Lobby}?"); the owner's asks who owns it next (`hand-over`, a row per other Member) and hands over and leaves in one Intent. Leaving another Lobby drops it from the switcher and leaves the window where it is; leaving the open one moves on to the next Lobby. A Lobby that needs a new Sign In has both lines disabled ("Sign in to it first"). An errand's refusal is a notification in the window.
- `corrupt-state` moves a corrupt `state.json` aside and shows an Issue.
- `small` keeps everything usable at 480×400: the Voice strip and the members button.
- `resize` renders the whole window through resizes and maximize.
- `sidebar-width` drags the member sidebar's edge (220 px up to 420 px, leaving the chat 340 px), steps it with Ctrl+Alt+Left/Right, resets it with a double click, and keeps the width in `state.json`.

## How to get to it (user POV)

- The Lobby switcher in the title bar, with "Add Lobby…" in it, and each row's ⋯ (or a right-click on the row) for that Lobby's menu.
- Restarting the app.
- Resizing the window, or the members button when small.
- Dragging the line between the chat and the member sidebar.

## Driving it with the scenario harness

Preconditions:

- Header `second_lobby: true` starts a second server, `{lobby2}`. A header `size` sets the window size.

- **Switch.** `{"click": "lobby-switcher"}`, wait `{"visible": "switcher-add"}`, `{"click": "switcher-add"}`, wait `/add_lobby_open` = true, type `{lobby2}` into `add-lobby-url`, `{"click": "add-lobby-connect"}`. Check `/lobbies` and `/switch_asks`.
- **Lobby menu.** Open the switcher, then `{"click": "switcher-more-<n>"}` or `{"right_click": "switcher-lobby-<n>"}` (n counts the rows from 0), wait `{"visible": "lobby-menu"}`. Its lines are `lobby-menu-icon` (script the picker first: `{"host": {"pick": [["photo-640x480.png"]]}}`), `lobby-menu-icon-remove` and `lobby-menu-leave`. The Member's Leave is a confirm (`{"press": "enter"}`); the owner's is `hand-over` with rows `heir-<name>` and the button `hand-over-leave`. Check `/lobbies` (its `icon` and `icon_shown`) and `/errand` (the origin an errand runs on, `null` once it ended). A row's icon is fetched while the switcher is drawn: open it before waiting on `icon_shown`.
- **Restore.** `{"close": "a"}`, then `{"open": "a"}` (Layer 4 real Quit and restart), then wait `/lobby/phase` = `connected` without Sign In.
- **Corrupt.** `{"close": "a"}`, `{"corrupt": "a"}`, `{"open": "a"}`, then check `/issues`.
- **Small.** Header `"size": [480, 400]` or `{"action": "resize:480x400"}`. Check `/window/width`, `{"click": "members-toggle"}`, `/lobby/members_open`, and take a `shot` of the window.
- **Sidebar width.** `{"drag": "sidebar-edge", "by": [-96, 0]}` (the edge exists only while the sidebar is docked, 600 px and up), `{"press": "ctrl-alt-left"}`, then check `/lobby/sidebar_width` (`null` while folded).
- **Tray.** `{"action": "window:close"}` is the close button: it hides the window (`/window/hidden` = true, `/quit` stays `running`). `{"action": "tray:show"}` brings it back and `{"action": "tray:quit"}` is the tray menu's Quit; `{"close": "a"}` is Ctrl+Q and works from a hidden window. In Layer 4 these call the real `ShowWindow`; the driven client's tray icon is registered but hidden, and a show never takes the focus.
- **One instance.** A second launch of the same profile (`LOBBY_INSTANCE_ID`, else `LOBBY_USER_DATA_DIR`, else the default) asks the first to show itself and exits 0 within milliseconds, before any window, state or keychain access. The first logs `shown by a second launch`. Scenarios never hit it: every driven client has its own instance id. To check it on the real binary start two identical `lobby.exe` with `LOBBY_AUTOMATION=1` (so the show never takes the focus) and read the first's log. `ALLOW_MULTIPLE_INSTANCES=1` skips the check.
- **Deep link.** `{"action": "open-link:lobby://{host}"}` (`{host2}` for the second Lobby) is a `lobby://` link as the OS hands it over. The Lobby the Member is in, or a link that is not a bare address, shows nothing. A Lobby this device has is switched to. Any other puts its address in Add Lobby (`/add_lobby_url`) or, outside a Lobby, in the address step (`/url_step/value`), and connects only when the Member clicks Connect. On the real binary, a second `lobby.exe` of the same profile given `lobby://<address>` sends `open <link>` down the one-instance pipe and exits 0; the first logs `link: …`. A first launch given a link takes the same path.
- **Proof.** `lobby-deep-link`, `switch-lobbies`, `voice-bar-small`, `sign-in-and-presence-small` (the other scenarios are out of the suite, see the README). After window or renderer changes, also run `pwsh xtask/scripts/resize-check.ps1`.

## Gotchas

- `resize:` in Layer 4 uses `SetWindowPos(SWP_NOACTIVATE)`. Never resize any other way.
- `drive_only` scenarios (real window sizes) are skipped by Layer 3. Run them with `cargo xtask drive`.
- `/lobby/sidebar_width` is a float: compare with `220.0`, not `220`.
- A driven client never remembers its window's size and place, so a drive does not leave the parked, off-screen rectangle in `state.json`.
- The default size is 680×480. Always check 480×400 too, and judge crops for overlap and clipping.

# Living Room

`living-room.json` is the app's one theme, a gpui-component theme file. `appearance::apply` loads
it. Views read its colours from `cx.theme()`, so a second theme needs no change in a view.

A key the file leaves out takes Default Dark's colour, or a shade the kit derives from another key.
`appearance.rs` has a test that fails when a colour is still Default Dark's.

Lobby's own roles borrow these slots:

| Role | Slot |
| --- | --- |
| Member list, composer | `sidebar` |
| Voice box, Voice strip | `sidebar.accent` |
| Watch tile at rest | `sidebar.primary` |
| Voice accent: speaking ring, waiting marks | `primary` |
| Watched tile, Join Voice | `primary` with `primary.foreground` |
| Leave | `button.danger` with `button.danger.foreground` |
| Mute, deafen and Share Screen at rest | `secondary` |
| Mute and deafen when on | `button.danger.active` with `button.danger.foreground` |
| Share Screen while you share | `primary` mixed 24 % into `secondary`, icon `primary` |
| Error and muted text, "New" | `danger` |
| Idle's dot | `base.yellow` |
| Do Not Disturb's dot, LIVE | `base.red` |
| Reconnecting | `warning` |
| Behind a Screen Share picture | `background`, darker |
| The share toolbar's fade over the picture | `overlay`, fading to clear |

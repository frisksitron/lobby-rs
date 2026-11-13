//! The watch tile at the end of a sharer's member row: one screen-shaped tile, the same size in
//! every state, that puts their share on screen and takes it off again.

use gpui_kit::{
    App, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, assets::IconName, div, prelude::FluentBuilder as _,
};
use lobby_client::copy;

use super::{
    Callback, ROW_HEIGHT, SURFACE_INNER_RADIUS, Sharer, Whose, member_row::ROW_PAD_X, nested_radius,
};
use crate::kit::{ActiveTheme as _, Colorize as _, Theme};
use crate::scale::u;
use crate::ui::{self, Icon};

/// The drawn tile. The press lands on its slot, which is as tall as the row.
const TILE_W: f32 = 44.;
const TILE_H: f32 = 32.;
/// The tile's gap to the row's top and bottom edges, and, in the row's padding, to its end.
const TILE_INSET: f32 = (ROW_HEIGHT - TILE_H) / 2.;
const _: () = assert!(TILE_INSET == ROW_PAD_X);
const TILE_RADIUS: f32 = nested_radius(SURFACE_INNER_RADIUS, TILE_INSET);
const TILE_ICON: f32 = 18.;
/// The muted mark beside a tile has a slot of the same width, so right edges line up.
pub const SLOT_W: f32 = TILE_W;

/// What a sharer's tile shows. Pressing it asks for what [`Tile::press`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    /// Watching needs Voice: an outline that does nothing.
    Faint,
    /// This computer cannot watch a share at all, and no Voice would change that: the same
    /// outline, whose tooltip says so.
    Unavailable,
    /// Pressing it puts this share on screen.
    Rest,
    /// Asked for, until its first picture comes; the share on screen stays meanwhile. Pressing
    /// it again asks again, which the Session sends only if the first ask went unanswered.
    Coming,
    /// On screen. Pressing it stops watching, or, while another share is coming, keeps this one.
    OnScreen { switching: bool },
}

/// A tile's fill, ring and icon colour, and the fill and ring under the pointer.
#[derive(Debug, Clone, Copy)]
pub struct TilePaint {
    pub bg: Hsla,
    pub ring: Hsla,
    pub fg: Hsla,
    pub hover: Option<(Hsla, Hsla)>,
}

/// What a press on a tile asks the Lobby view for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TilePress {
    Show(Sharer),
    Stop,
}

impl Tile {
    /// The press on the tile of `sharer`'s row. Showing the share on screen while another is
    /// coming is the Session's take-back of that switch.
    pub fn press(self, sharer: Sharer) -> Option<TilePress> {
        match self {
            Tile::Faint | Tile::Unavailable => None,
            Tile::Rest | Tile::Coming | Tile::OnScreen { switching: true } => {
                Some(TilePress::Show(sharer))
            }
            Tile::OnScreen { switching: false } => Some(TilePress::Stop),
        }
    }

    /// An outline that does nothing, for whatever reason.
    pub fn faint(self) -> bool {
        matches!(self, Tile::Faint | Tile::Unavailable)
    }

    /// The short words of its tooltip; the row already names the Member.
    pub fn tip(self, own: bool) -> &'static str {
        match (self, own) {
            (Tile::Faint, _) => copy::WATCH_NEEDS_VOICE,
            (Tile::Unavailable, _) => copy::WATCH_UNAVAILABLE,
            (Tile::Rest | Tile::Coming, true) => "Show your share",
            (Tile::Rest | Tile::Coming, false) => "Watch",
            (Tile::OnScreen { switching: true }, _) => "Keep watching",
            (Tile::OnScreen { switching: false }, true) => "Hide your share",
            (Tile::OnScreen { switching: false }, false) => "Stop watching",
        }
    }

    /// Its colours: the face badges of the small window's Voice bar wear the same.
    pub fn paint(self, theme: &Theme) -> TilePaint {
        let (primary, rest) = (theme.primary, theme.sidebar_primary);
        let lifted = primary.mix(rest, 0.3);
        let clear = gpui_kit::transparent_black();
        let (bg, ring, fg, hover) = match self {
            Tile::Faint | Tile::Unavailable => (clear, theme.border, theme.muted_foreground, None),
            Tile::Rest => (
                rest,
                clear,
                theme.sidebar_primary_foreground,
                Some((lifted, primary.opacity(0.6))),
            ),
            Tile::Coming => (lifted, primary, theme.sidebar_primary_foreground, None),
            Tile::OnScreen { .. } => (
                primary,
                primary,
                theme.primary_foreground,
                Some((theme.primary_hover, theme.primary_hover)),
            ),
        };
        TilePaint {
            bg,
            ring,
            fg,
            hover,
        }
    }

    /// What a press does, with whose share it is: "Watch bob", "Show your share".
    pub fn label(self, sharer: &Sharer) -> String {
        let tip = self.tip(sharer.whose == Whose::Mine);
        match sharer.whose {
            Whose::Mine => tip.to_owned(),
            Whose::Member(_) => format!("{tip} {}", sharer.name),
        }
    }
}

#[derive(IntoElement)]
pub struct WatchTile {
    id: SharedString,
    tile: Tile,
    sharer: Sharer,
    on_press: Option<Callback<TilePress>>,
    hovered: bool,
}

impl WatchTile {
    pub fn new(id: impl Into<SharedString>, tile: Tile, sharer: Sharer) -> Self {
        Self {
            id: id.into(),
            tile,
            sharer,
            on_press: None,
            hovered: false,
        }
    }

    /// Paints the look under the pointer with no pointer there, for the gallery.
    pub fn hovered(mut self) -> Self {
        self.hovered = true;
        self
    }

    pub fn on_press(mut self, on_press: Callback<TilePress>) -> Self {
        self.on_press = Some(on_press);
        self
    }
}

impl RenderOnce for WatchTile {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let tip = self.tile.tip(self.sharer.whose == Whose::Mine);
        let label = self.tile.label(&self.sharer);
        let clear = gpui_kit::transparent_black();
        let TilePaint {
            bg,
            ring,
            fg,
            hover,
        } = self.tile.paint(theme);
        let group = self.id.clone();
        let hovered = self.hovered;
        let drawn = div()
            .w(u(TILE_W))
            .h(u(TILE_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(u(TILE_RADIUS))
            .border_1()
            .border_color(ring)
            .bg(bg)
            .child(
                Icon::new(IconName::Monitor)
                    .size(u(TILE_ICON))
                    .text_color(fg),
            )
            .when_some(hover, |d, (bg, ring)| {
                if hovered {
                    d.bg(bg).border_color(ring)
                } else {
                    d.group_hover(group.clone(), move |s| s.bg(bg).border_color(ring))
                }
            });
        let press = self.tile.press(self.sharer);
        ui::Button::new(self.id)
            .group(group)
            .variant(ui::Variant::Custom(ui::Custom::new(
                clear,
                theme.foreground,
            )))
            .w(u(SLOT_W))
            .h(u(ROW_HEIGHT))
            .p_0()
            .accessibility_label(label)
            .tooltip(tip)
            .disabled(press.is_none())
            // Its press is the tile's, not the row's: the Member card stays shut.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(drawn)
            .when_some(press.zip(self.on_press), |b, (press, on_press)| {
                b.on_click(move |_, window, cx| on_press(press.clone(), window, cx))
            })
    }
}

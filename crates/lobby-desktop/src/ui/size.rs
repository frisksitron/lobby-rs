//! Lobby's control sizes. A control takes its height from the surface it stands on, so every
//! control of a surface lines up with the others, and its type, padding and icon follow.

use gpui_kit::{App, FontWeight, Pixels, Rems};

use crate::{
    kit::ActiveTheme as _,
    scale::{device_in, u},
};

pub const ICON_XS: f32 = 12.;
pub const ICON_SM: f32 = 14.;
pub const ICON_MD: f32 = 16.;

// A control takes its height from here and never from the kit's size, whose steps do not match
// (`Button::large` is still 32).

/// The onboarding screens' fields, the button beside a field and the code's cells.
pub const FIELD_H: f32 = 44.;

/// Every control of a page (Settings) and of a dialog: selects, buttons, the Back arrow.
pub const PAGE_CONTROL_H: f32 = 36.;
pub const PAGE_CONTROL_RADIUS: f32 = 8.;

/// Every title-bar control has this height, so their hover boxes line up. So do a toast's and a
/// card's word buttons and the quiet link.
pub const BAR_CONTROL_H: f32 = 28.;

/// Dense surfaces: the composer, a rename field and the buttons at a line's end.
pub const INLINE_H: f32 = 24.;

/// The small icon buttons in an item's corner.
pub const CORNER_H: f32 = 20.;

pub const ROW_HEIGHT: f32 = 40.;

/// The surface a control stands on, which sets its height, padding, type and icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    /// A line of a list or a tile, whose caller usually sets the box around its own content.
    #[default]
    Row,
    /// The composer and the buttons at a line's end.
    Inline,
    /// The title bar.
    Bar,
    /// A worded action on a card, a toast or a notice, such as Vouch or Jump to latest: the title
    /// bar's height with a page's weight.
    Word,
    /// An item's corner: dismiss, remove or retry on a card or a row.
    Corner,
    /// A page (Settings) or a dialog.
    Page,
    /// An onboarding step, beside its field.
    Step,
}

impl Size {
    pub(crate) fn height(self) -> Rems {
        u(match self {
            Size::Row => ROW_HEIGHT,
            Size::Inline => INLINE_H,
            Size::Bar | Size::Word => BAR_CONTROL_H,
            Size::Corner => CORNER_H,
            Size::Page => PAGE_CONTROL_H,
            Size::Step => FIELD_H,
        })
    }

    pub(crate) fn pad_x(self) -> Rems {
        u(match self {
            Size::Corner => 4.,
            Size::Inline | Size::Bar => 8.,
            Size::Row => 10.,
            Size::Word => 12.,
            Size::Page => 14.,
            Size::Step => 16.,
        })
    }

    pub(crate) fn gap(self) -> Rems {
        u(match self {
            Size::Inline | Size::Bar | Size::Word | Size::Corner => 4.,
            Size::Row | Size::Page | Size::Step => 8.,
        })
    }

    pub(crate) fn icon(self) -> Rems {
        u(match self {
            Size::Corner => ICON_XS,
            Size::Inline | Size::Bar | Size::Word => ICON_SM,
            Size::Row | Size::Page | Size::Step => ICON_MD,
        })
    }

    pub(crate) fn weight(self) -> Option<FontWeight> {
        match self {
            Size::Word | Size::Page | Size::Step => Some(FontWeight::SEMIBOLD),
            Size::Row | Size::Inline | Size::Bar | Size::Corner => None,
        }
    }

    pub(crate) fn radius(self, cx: &App) -> Pixels {
        match self {
            Size::Page => device_in(PAGE_CONTROL_RADIUS, cx),
            Size::Row | Size::Inline | Size::Bar | Size::Word | Size::Corner | Size::Step => {
                cx.theme().radius
            }
        }
    }
}

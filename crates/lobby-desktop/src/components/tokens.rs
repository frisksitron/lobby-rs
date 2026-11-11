//! The sizes and colours the Lobby UI shares, by name. The kit theme has the rest.

use crate::kit::{Colorize as _, Theme};
use gpui_kit::Hsla;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvatarSize {
    Chip,
    Bar,
    Menu,
    Row,
    Alert,
    Hero,
}

impl AvatarSize {
    pub const ALL: [AvatarSize; 6] = [
        AvatarSize::Chip,
        AvatarSize::Bar,
        AvatarSize::Menu,
        AvatarSize::Row,
        AvatarSize::Alert,
        AvatarSize::Hero,
    ];

    pub const fn px(self) -> f32 {
        match self {
            AvatarSize::Chip => 18.,
            AvatarSize::Bar => 22.,
            AvatarSize::Menu => 28.,
            AvatarSize::Row => 30.,
            AvatarSize::Alert => 32.,
            AvatarSize::Hero => 48.,
        }
    }
}

// The control and icon sizes belong to `ui`, whose controls take them.
pub use crate::ui::{
    BAR_CONTROL_H, FIELD_H, ICON_MD, ICON_SM, ICON_XS, INLINE_H, PAGE_CONTROL_H,
    PAGE_CONTROL_RADIUS, ROW_HEIGHT,
};

/// A line of a Member card and of the mention menu.
pub const LINE_H: f32 = 30.;

/// The width of a percent readout (a stepper's or a slider's), and the gap between a control and
/// its readout.
pub const PERCENT_W: f32 = 48.;
pub const READOUT_GAP: f32 = 8.;

/// The frame rate of a looping opacity pulse (the speaking ring, the typing dots). Each frame
/// steps the opacity by under 0.05, which reads as smooth, and the view around it is drawn half
/// as often as at 60 Hz.
pub const PULSE_FPS: f32 = 30.;

/// The space between Member rows in the member list, inside the Voice box as well as outside it.
pub const MEMBER_ROW_GAP: f32 = 8.;

/// The width of every border Lobby draws. A border is not scaled and is not a length, so it counts
/// as this many design pixels in [`nested_radius`].
pub const BORDER: f32 = 1.;

/// The corner radius of a box whose outer edge lies `inset` inside the outer edge of a box with
/// corner radius `outer`, so the gap between the two curves is even. `inset` is the outer box's
/// border plus padding, less any negative margin of the inner box. A box that sits that deep
/// has square corners.
pub const fn nested_radius(outer: f32, inset: f32) -> f32 {
    if inset >= outer { 0. } else { outer - inset }
}

/// The corner of every floating surface of the main window: the member sheet, the composer, the
/// Voice strip and the Screen Share picture. They stand side by side on the window, so they round
/// alike.
pub const SURFACE_RADIUS: f32 = 20.;

/// The air between such a surface's border and the controls in its corners: the member list's
/// sides and top, the Voice foot all round, and the composer's attach and Send.
pub const SURFACE_PAD: f32 = 8.;

/// A box that fills that padding: a Member's row, the Voice foot's buttons, and the composer's
/// attach and Send.
pub const SURFACE_INNER_RADIUS: f32 = nested_radius(SURFACE_RADIUS, BORDER + SURFACE_PAD);

// Lobby's own colours, on the theme slots they borrow, so another theme brings its own.

/// Lavender: the speaking ring, the Voice box's edge and the waiting marks.
pub fn voice_accent(theme: &Theme) -> Hsla {
    theme.primary
}

/// Idle's dot: a warm yellow, apart from the amber of "Reconnecting" (the theme's warning).
pub fn idle_color(theme: &Theme) -> Hsla {
    theme.yellow
}

/// A Screen Share's red: Stop sharing, its dot and the outlines of a share.
pub fn live_red(theme: &Theme) -> Hsla {
    theme.red
}

/// Behind a Screen Share picture and a screen's thumbnail: darker than the window, like a screen.
pub fn picture_backdrop(theme: &Theme) -> Hsla {
    theme.background.darken(0.4)
}

/// The Lobby marks' eight calm colours as hue, saturation and lightness, each dark enough for white
/// initials at 4.7:1.
pub const MARK_COLORS: [(f32, f32, f32); 8] = [
    (4., 0.68, 0.47),
    (22., 0.85, 0.40),
    (36., 0.92, 0.32),
    (145., 0.63, 0.31),
    (176., 0.80, 0.28),
    (204., 0.82, 0.39),
    (240., 0.58, 0.55),
    (330., 0.68, 0.47),
];

/// WCAG's contrast of white initials on a mark's fill.
fn white_contrast(fill: Hsla) -> f32 {
    let c = fill.to_rgb();
    let channel = |v: f32| {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b);
    1.05 / (luminance + 0.05)
}

/// A Lobby's mark on a dark surface: a little lighter, so its edge shows against the near-black,
/// as far as white initials keep their 4.5:1.
pub fn mark_on_dark(mark: Hsla) -> Hsla {
    let mut shown = mark;
    for step in 1..=6 {
        let lifted = Hsla {
            l: mark.l + 0.01 * step as f32,
            ..mark
        };
        if white_contrast(lifted) < 4.5 {
            break;
        }
        shown = lifted;
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::hsla;

    #[test]
    fn white_initials_read_on_every_mark_and_its_lifted_shade() {
        for (h, s, l) in MARK_COLORS {
            let mark = hsla(h / 360., s, l, 1.);
            assert!(white_contrast(mark) >= 4.5, "{h}: {}", white_contrast(mark));
            let lifted = mark_on_dark(mark);
            assert!(white_contrast(lifted) >= 4.5, "{h} on dark");
            assert!(lifted.l >= mark.l);
        }
    }
}

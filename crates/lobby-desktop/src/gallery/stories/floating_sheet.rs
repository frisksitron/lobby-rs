//! The story of the floating sheet: the member list lifted off the window by its edge alone,
//! docked beside the chat and folded over a dimmed small window.

use gpui_kit::{
    AnyElement, App, Context, Div, IntoElement as _, ParentElement as _, Styled as _, Window, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _,
};
use lobby_protocol::VoiceState;

use crate::components::{
    BORDER, FootSize, MEMBER_ROW_GAP, MemberRowModel, SURFACE_PAD, SURFACE_RADIUS, ShareControl,
    TITLE_BAR_HEIGHT, Tile, VoiceBarModel, VoiceFoot, card_surface, floating_sheet, nested_radius,
    voice_box,
};
use crate::gallery::{
    Gallery, Section,
    fixtures::{joining, member, muted, sharing, speaking},
};
use crate::kit::ActiveTheme as _;
use crate::lobby_view::{BODY_TOP, FOLDED_SHEET_W, SHEET_INSET, SIDEBAR_SLIM_W, SIDEBAR_W};
use crate::scale::u;

use super::{
    caption,
    members::row,
    voice::{in_voice, out_of_voice, voice_actions},
    wrap,
};

/// The sheet's height in a 680 × 480 window: the window under the title bar, less the air above
/// and below the sheet.
const DOCKED_H: f32 = 480. - TITLE_BAR_HEIGHT - BODY_TOP - SHEET_INSET;
/// The small window's view under its title bar, and the sheet's height in it.
pub(super) const VIEW: (f32, f32) = (480., 360.);
pub(super) const FOLDED_H: f32 = VIEW.1 - BODY_TOP - SHEET_INSET;
/// How tall the list's fade is where it meets the foot or the sheet's end.
const FADE_H: f32 = 16.;

fn sharer(name: &str, tile: Tile, state: VoiceState) -> MemberRowModel {
    MemberRowModel {
        voice: Some(state),
        sharing: true,
        tile: Some(tile),
        ..member(name)
    }
}

fn in_the_box(name: &str, state: VoiceState) -> MemberRowModel {
    MemberRowModel {
        voice: Some(state),
        ..member(name)
    }
}

/// Everyone, as the list shows them: the Voice box first, then who is online, then who is not.
/// `me` is In Voice, or the tiles are faint.
fn list(me: bool, cx: &App) -> Div {
    let (sidebar, inside) = (cx.theme().sidebar, card_surface(cx));
    let offline = ["frank", "grace", "heidi"].map(|name| MemberRowModel {
        presence: lobby_protocol::Presence::Offline,
        ..member(name)
    });
    let (boxed, online): (Vec<MemberRowModel>, Vec<MemberRowModel>) = if me {
        (
            vec![
                MemberRowModel {
                    is_me: true,
                    ..in_the_box("bob", speaking())
                },
                sharer("alice", Tile::OnScreen { switching: false }, sharing()),
                in_the_box("carol", muted()),
                sharer(
                    "dave",
                    Tile::Rest,
                    VoiceState {
                        muted: true,
                        ..sharing()
                    },
                ),
            ],
            vec![
                member("Eve Online"),
                member("Maximilian Featherstonehaugh-Wolfeschlegel"),
            ],
        )
    } else {
        (
            vec![
                sharer("alice", Tile::Faint, sharing()),
                in_the_box("carol", muted()),
                sharer(
                    "dave",
                    Tile::Faint,
                    VoiceState {
                        muted: true,
                        ..sharing()
                    },
                ),
                in_the_box("frank", joining()),
            ],
            vec![
                MemberRowModel {
                    is_me: true,
                    ..member("bob")
                },
                member("Eve Online"),
                member("Maximilian Featherstonehaugh-Wolfeschlegel"),
            ],
        )
    };
    let boxed = boxed
        .into_iter()
        .map(|m| row(m, true, true, inside))
        .collect();
    div()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .px(u(SURFACE_PAD))
        .pt(u(SURFACE_PAD))
        .pb_4()
        .flex()
        .flex_col()
        .gap(u(MEMBER_ROW_GAP))
        .child(voice_box(boxed, inside))
        .children(online.into_iter().map(|m| row(m, true, false, sidebar)))
        .children(offline.into_iter().map(|m| row(m, true, false, sidebar)))
}

/// A sheet `width` wide and `height` tall with `foot` at its foot; without a foot its list ends
/// at the sheet's rounded end.
pub(super) fn sheet(
    width: f32,
    height: f32,
    me: bool,
    foot: Option<VoiceBarModel>,
    cx: &App,
) -> Div {
    let sidebar = cx.theme().sidebar;
    floating_sheet(cx)
        .w(u(width))
        .h(u(height))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(list(me, cx))
                .child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .h(u(FADE_H))
                        .when(foot.is_none(), |d| {
                            d.rounded_b(u(nested_radius(SURFACE_RADIUS, BORDER)))
                        })
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(sidebar.opacity(0.), 0.),
                            linear_color_stop(sidebar, 1.),
                        )),
                ),
        )
        .children(foot.map(|model| {
            div()
                .flex_shrink_0()
                .p(u(SURFACE_PAD))
                .child(VoiceFoot::new(model, voice_actions()).size(FootSize::Sheet))
        }))
}

/// The small window's view under its title bar, a few lines standing for the feed.
pub(super) fn small_view(cx: &App) -> Div {
    let theme = cx.theme();
    let line = |w: f32| div().h(u(8.)).w(u(w)).rounded_full().bg(theme.muted);
    div()
        .relative()
        .w(u(VIEW.0))
        .h(u(VIEW.1))
        .flex_shrink_0()
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(
            div()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(line(180.))
                .child(line(260.))
                .child(line(120.))
                .child(line(220.)),
        )
}

/// The small window's view, dimmed, with the sheet unfolded at the right.
fn folded(cx: &App) -> AnyElement {
    let theme = cx.theme();
    small_view(cx)
        .child(div().absolute().inset_0().bg(theme.overlay))
        .child(
            div()
                .absolute()
                .top(u(BODY_TOP))
                .right(u(SHEET_INSET))
                .child(sheet(FOLDED_SHEET_W, FOLDED_H, true, None, cx)),
        )
        .into_any_element()
}

pub(super) fn floating_sheet_story(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let out = Some(out_of_voice());
    let inside = Some(in_voice(false, ShareControl::Off));
    vec![
        Section::new(
            "docked",
            "Docked in a window of 680 × 480, 220 wide and as tall as the window leaves it. Lifted by \
             its edge alone: no shadow. The list scrolls and fades into the Voice foot. Out of \
             Voice the tiles are faint; In Voice, yours is in the purple box",
            wrap([
                caption(
                    "Out of Voice",
                    sheet(SIDEBAR_SLIM_W, DOCKED_H, false, out, cx),
                    cx,
                ),
                caption(
                    "In Voice",
                    sheet(SIDEBAR_SLIM_W, DOCKED_H, true, inside.clone(), cx),
                    cx,
                ),
                caption(
                    "Wider: 264",
                    sheet(SIDEBAR_W, DOCKED_H, true, inside, cx),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "folded",
            "Folded in the small window, 232 wide over the dimmed window with no foot: the Voice bar \
             above the composer has the controls. It rounds its own end, since GPUI clips only to \
             rectangles",
            wrap([caption("480 × 360 under the title bar", folded(cx), cx)]),
        ),
    ]
}

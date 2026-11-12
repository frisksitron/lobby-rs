//! One Member's face in the small window's Voice bar. A sharer's face is a button: it carries a
//! small screen badge in their watch tile's colours, and a press does what a press on the tile
//! does. Muted swaps the presence dot for a red mic badge; speaking keeps its ring. A face that
//! shares nothing only names the Member. A right-click on any face opens that Member's card.

use gpui_kit::{
    AnyElement, App, Div, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, Point, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, assets::IconName, div, prelude::FluentBuilder as _,
};

use lobby_protocol::UserId;

use super::{
    AvatarState, Callback, MemberAvatar, MemberRowModel, PictureOf, Sharer, Tile, TilePaint,
    TilePress, tokens::AvatarSize, voice_bar::STRIP_INNER_RADIUS,
};
use crate::kit::ActiveTheme as _;
use crate::scale::{device_in, u};
use crate::ui::{self, Icon};

pub const FACE_W: f32 = 40.;
pub const FACE_H: f32 = 44.;
const SCREEN_BADGE_W: f32 = 20.;
const SCREEN_BADGE_H: f32 = 14.;
const SCREEN_BADGE_RADIUS: f32 = 4.;
const SCREEN_BADGE_ICON: f32 = 11.;
const MIC_BADGE: f32 = 15.;
const MIC_BADGE_ICON: f32 = 9.;
/// Each badge sits in a ring of the bar's colour, so it reads as cut out of the face.
const BADGE_RING: f32 = 2.;
/// How far the screen badge reaches past the avatar's right edge.
const SCREEN_BADGE_REACH: f32 = 3.;

#[derive(IntoElement)]
pub struct VoiceFace {
    row: MemberRowModel,
    known: bool,
    pictures: Option<PictureOf>,
    surface: Hsla,
    on_press: Option<Callback<TilePress>>,
    on_context: Option<Callback<(UserId, Point<Pixels>)>>,
}

impl VoiceFace {
    pub fn new(row: MemberRowModel, known: bool, surface: Hsla) -> Self {
        Self {
            row,
            known,
            pictures: None,
            surface,
            on_press: None,
            on_context: None,
        }
    }

    /// Where the avatar's picture comes from, once it is decoded; the initials stand in until then.
    pub fn pictures(mut self, pictures: PictureOf) -> Self {
        self.pictures = Some(pictures);
        self
    }

    pub fn on_press(mut self, on_press: Callback<TilePress>) -> Self {
        self.on_press = Some(on_press);
        self
    }

    /// A right-click on the face: the Member, and where the pointer was.
    pub fn on_context(mut self, on_context: Callback<(UserId, Point<Pixels>)>) -> Self {
        self.on_context = Some(on_context);
        self
    }
}

/// A badge in its cut-out ring at the avatar's right, `reach` past its edge: at the bottom, or at
/// the top.
fn badge_slot(surface: Hsla, radius: f32, top: bool, reach: f32) -> Div {
    div()
        .absolute()
        .right(u(-BADGE_RING - reach))
        .map(|d| {
            if top {
                d.top(u(-BADGE_RING))
            } else {
                d.bottom(u(-BADGE_RING))
            }
        })
        .p(u(BADGE_RING))
        .rounded(u(radius + BADGE_RING))
        .bg(surface)
}

fn screen_badge(tile: Tile, group: SharedString, surface: Hsla, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let TilePaint {
        bg,
        ring,
        fg,
        hover,
    } = tile.paint(theme);
    // The tile's faint edge is lost on the bar's purple.
    let ring = if tile == Tile::Faint {
        theme.muted_foreground.opacity(0.45)
    } else {
        ring
    };
    let badge = div()
        .w(u(SCREEN_BADGE_W))
        .h(u(SCREEN_BADGE_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(u(SCREEN_BADGE_RADIUS))
        .border_1()
        .border_color(ring)
        .bg(bg)
        .child(
            Icon::new(IconName::Monitor)
                .size(u(SCREEN_BADGE_ICON))
                .text_color(fg),
        )
        .when_some(hover, |d, (bg, ring)| {
            d.group_hover(group, move |s| s.bg(bg).border_color(ring))
        });
    badge_slot(surface, SCREEN_BADGE_RADIUS, false, SCREEN_BADGE_REACH)
        .child(badge)
        .into_any_element()
}

/// Deafened is muted too: its one badge says both.
fn mic_badge(deafened: bool, top: bool, surface: Hsla, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let icon = if deafened {
        IconName::HeadphoneOff
    } else {
        IconName::MicOff
    };
    badge_slot(surface, MIC_BADGE / 2., top, 0.)
        .child(
            div()
                .size(u(MIC_BADGE))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.button_danger)
                .child(
                    Icon::new(icon)
                        .size(u(MIC_BADGE_ICON))
                        .text_color(theme.button_danger_foreground),
                ),
        )
        .into_any_element()
}

impl RenderOnce for VoiceFace {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let row = self.row;
        let context = self.on_context.map(|on_context| {
            let id = row.id.clone();
            move |ev: &gpui_kit::MouseDownEvent, window: &mut Window, cx: &mut App| {
                on_context((id.clone(), ev.position), window, cx)
            }
        });
        let image = self
            .pictures
            .and_then(|pictures| pictures(row.avatar.as_ref(), cx));
        let id = SharedString::from(format!("face-{}", row.name));
        let muted = row.voice.filter(|v| v.muted);
        let state = AvatarState {
            presence: (row.tile.is_none() && muted.is_none()).then_some(row.presence),
            known: self.known,
            voice: row.voice,
        };
        let avatar = div()
            .relative()
            .child(
                MemberAvatar::new(
                    SharedString::from(format!("face-avatar-{}", row.id)),
                    row.name.clone(),
                    state,
                )
                .size(AvatarSize::Row)
                .image(image)
                .surface(self.surface),
            )
            .children(
                row.tile
                    .map(|tile| screen_badge(tile, id.clone(), self.surface, cx)),
            )
            // A muted sharer's mic goes above the screen badge: at the bottom left it would sit
            // against the face before, and read as that face's.
            .children(muted.map(|v| mic_badge(v.deafened, row.tile.is_some(), self.surface, cx)));
        let Some(tile) = row.tile else {
            let name = if row.is_me {
                format!("{} (you)", row.name)
            } else {
                row.name.to_string()
            };
            let tip = match muted {
                Some(v) if v.deafened => format!("{name}, deafened"),
                Some(_) => format!("{name}, muted"),
                None => name,
            };
            return div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .w(u(FACE_W))
                .h(u(FACE_H))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(avatar)
                .tooltip(move |_, cx| ui::tooltip(tip.clone(), cx))
                .when_some(context, |d, context| {
                    d.on_mouse_down(MouseButton::Right, context)
                })
                .test_support()
                .into_any_element();
        };
        let sharer = Sharer::of(&row);
        let label = tile.label(&sharer);
        let tip = if tile == Tile::Faint {
            tile.tip(false).to_owned()
        } else {
            label.clone()
        };
        let press = tile.press(sharer);
        let wash = cx.theme().foreground;
        ui::Button::new(id.clone())
            .group(id.clone())
            .variant(ui::Variant::Custom(
                ui::Custom::new(gpui_kit::transparent_black(), wash)
                    .hover(wash.opacity(0.06))
                    .active(wash.opacity(0.09)),
            ))
            .rounded(device_in(STRIP_INNER_RADIUS, cx))
            .w(u(FACE_W))
            .h(u(FACE_H))
            .p_0()
            .accessibility_label(label)
            .tooltip(tip)
            .disabled(press.is_none())
            .child(avatar)
            .when_some(press.zip(self.on_press), |b, (press, on_press)| {
                b.on_click(move |_, window, cx| on_press(press.clone(), window, cx))
            })
            .when_some(context, |b, context| {
                b.on_mouse_down(MouseButton::Right, context)
            })
            .into_any_element()
    }
}

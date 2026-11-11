//! One Member's row in the member list, the same for every Member: one line, whose name never
//! moves. A right-click opens their Member Card at the pointer; a left-click opens nothing. A
//! sharer's row ends in their [`WatchTile`], with the muted mark in a slot of the same width
//! before it.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Hsla, ImageSource, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, Point, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, assets::IconName, div, prelude::FluentBuilder as _,
};
use lobby_protocol::{BlobId, Presence, UserId, VoiceState};

use super::{
    AvatarState, Callback, MemberAvatar, Sharer, Tile, TilePress, WatchTile,
    tokens::{
        AvatarSize, BORDER, ICON_MD, MEMBER_ROW_GAP, SURFACE_INNER_RADIUS, SURFACE_PAD,
        SURFACE_RADIUS, nested_radius,
    },
    truncated_text, watch_tile,
};
use crate::kit::{ActiveTheme as _, tooltip::Tooltip};
use crate::scale::{device_in, u};
use crate::ui::{self, Icon};

/// The row's air at its sides, which a watch tile at its end sits in.
pub const ROW_PAD_X: f32 = 4.;

/// How far the Voice box reaches into the gaps around its rows.
const VOICE_REACH: f32 = MEMBER_ROW_GAP / 2.;

/// The Voice box lies inside the sheet's border and the list's padding, less its reach.
const VOICE_BOX_RADIUS: f32 = nested_radius(SURFACE_RADIUS, BORDER + SURFACE_PAD - VOICE_REACH);

// A row in the box lies the box's padding and border in from its edge, which is its whole reach,
// and its corners are the ones a row on the sheet has.
const _: () = assert!(nested_radius(VOICE_BOX_RADIUS, VOICE_REACH) == SURFACE_INNER_RADIUS);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRowModel {
    pub id: UserId,
    pub name: SharedString,
    pub avatar: Option<BlobId>,
    pub is_me: bool,
    pub presence: Presence,
    pub voice: Option<VoiceState>,
    /// The Lobby says they share; the Member card's sentence says so.
    pub sharing: bool,
    /// The tile at the row's end, for a share this person could show.
    pub tile: Option<Tile>,
    pub owner: bool,
}

/// The purple box around the Members who are In Voice. Its padding and border are half the gap
/// between rows and its margins are the negative of that, so it takes no room of its own: it only
/// fills the gaps around its rows, and appearing, growing or shrinking moves no row outside it and
/// covers none.
pub fn voice_box(rows: Vec<AnyElement>, surface: Hsla) -> AnyElement {
    div()
        .id("voice-section")
        .debug_selector(|| "voice-section".into())
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(u(MEMBER_ROW_GAP))
        .m(u(-VOICE_REACH))
        .p(u(VOICE_REACH - BORDER))
        .border_1()
        .border_color(surface)
        .rounded(u(VOICE_BOX_RADIUS))
        .bg(surface)
        .children(rows)
        .test_support()
        .into_any_element()
}

#[derive(Clone)]
pub struct MemberRowActions {
    /// The Member whose card a right-click asked for, and where the pointer was.
    pub open: Callback<(UserId, Point<Pixels>)>,
    pub watch: Callback<Sharer>,
    pub stop_watching: Callback<()>,
}

#[derive(IntoElement)]
pub struct MemberRow {
    model: MemberRowModel,
    known: bool,
    in_voice_group: bool,
    surface: Option<Hsla>,
    /// Their card is open: the row keeps the wash a hover gives it.
    open: bool,
    actions: Option<MemberRowActions>,
    image: Option<ImageSource>,
    still: bool,
}

impl MemberRow {
    pub fn new(model: MemberRowModel, known: bool) -> Self {
        Self {
            model,
            known,
            in_voice_group: false,
            surface: None,
            open: false,
            actions: None,
            image: None,
            still: false,
        }
    }

    /// See [`MemberAvatar::still`].
    pub fn still(mut self, still: bool) -> Self {
        self.still = still;
        self
    }

    /// The avatar's picture, once it is decoded; the initials stand in until then.
    pub fn image(mut self, image: Option<ImageSource>) -> Self {
        self.image = image;
        self
    }

    pub fn in_voice_group(mut self, in_voice_group: bool) -> Self {
        self.in_voice_group = in_voice_group;
        self
    }

    pub fn surface(mut self, color: Hsla) -> Self {
        self.surface = Some(color);
        self
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn actions(mut self, actions: MemberRowActions) -> Self {
        self.actions = Some(actions);
        self
    }

    pub fn key(id: &UserId, in_voice_group: bool) -> SharedString {
        format!("{id}{}", in_voice_group as u8).into()
    }
}

impl RenderOnce for MemberRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let row = self.model;
        let in_voice_group = self.in_voice_group;
        let theme = cx.theme();
        let (fg, muted, danger) = (theme.foreground, theme.muted_foreground, theme.danger);
        let surface = self.surface.unwrap_or(theme.sidebar);
        let key = Self::key(&row.id, in_voice_group);
        let dimmed = row.presence == Presence::Offline;
        let voice_shown = in_voice_group || row.is_me;
        let voice = row.voice.filter(|_| voice_shown);
        let state = AvatarState {
            presence: Some(row.presence),
            known: self.known,
            voice,
        };
        // Deafened is muted too: its one mark says both.
        let muted_token = voice.filter(|v| v.muted).map(|v| {
            let (icon, label) = if v.deafened {
                (IconName::HeadphoneOff, "Deafened")
            } else {
                (IconName::MicOff, "Muted")
            };
            div()
                .id(SharedString::from(format!("muted-{}", row.id)))
                .flex_shrink_0()
                .w(u(watch_tile::SLOT_W))
                .flex()
                .justify_center()
                .child(Icon::new(icon).size(u(ICON_MD)).text_color(danger))
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        });
        let tile = row.tile.filter(|_| in_voice_group).map(|tile| {
            let tile = WatchTile::new(format!("watch-{}", row.name), tile, Sharer::of(&row));
            match self.actions.clone() {
                Some(a) => tile.on_press(Rc::new(move |press, window, cx| match press {
                    TilePress::Show(sharer) => (a.watch)(sharer, window, cx),
                    TilePress::Stop => (a.stop_watching)((), window, cx),
                })),
                None => tile,
            }
        });
        let name_id = SharedString::from(format!("name-{key}"));
        let content = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                MemberAvatar::new(
                    SharedString::from(format!("avatar-{key}")),
                    row.name.clone(),
                    state,
                )
                .size(AvatarSize::Row)
                .image(self.image)
                .surface(surface)
                .still(self.still),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .text_sm()
                    .line_height(u(18.))
                    .text_color(if dimmed { muted } else { fg })
                    .child(truncated_text(name_id, row.name.clone()).debug_selector({
                        let name = row.name.clone();
                        move || format!("member-name-{name}")
                    })),
            )
            .children(muted_token)
            .children(tile);
        let name = row.name.clone();
        let trigger = ui::Button::new(SharedString::from(format!("member-{name}")))
            .accessibility_label(if row.is_me {
                format!("{name} (you)")
            } else {
                name.to_string()
            })
            .debug_selector({
                let name = name.clone();
                move || format!("member-row-{name}")
            })
            // A wash of the text colour, so hover and the open panel read the same on the sidebar
            // and in the purple Voice box.
            .variant(ui::Variant::Custom(
                ui::Custom::new(gpui_kit::transparent_black(), fg)
                    .hover(fg.opacity(0.06))
                    .active(fg.opacity(0.09)),
            ))
            .rounded(device_in(SURFACE_INNER_RADIUS, cx))
            .w_full()
            .px(u(ROW_PAD_X))
            .justify_start()
            .selected(self.open)
            .child(content);
        let id = row.id.clone();
        div()
            .w_full()
            .flex_shrink_0()
            .child(trigger)
            .when_some(self.actions, |d, actions| {
                d.on_mouse_down(MouseButton::Right, move |ev, window, cx| {
                    (actions.open)((id.clone(), ev.position), window, cx)
                })
            })
    }
}

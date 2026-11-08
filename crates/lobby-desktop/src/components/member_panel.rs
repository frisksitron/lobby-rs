//! The Member Card a right-click on a Member's row (or their face in the small window's Voice
//! strip) opens, text only. On anyone else's it is their name, since when they have been a Member,
//! Watch while they share, how loud they play for this person (a slider and a reset), and for the
//! owner Make owner and Remove, each asked once more first. On the signed-in Member's own it is
//! what is theirs to change: the name (renamed in place), the email their Magic Codes go to, the
//! picture, Do Not Disturb, Show Voice stats while In Voice, and Sign out.

use gpui_kit::{
    AnyElement, App, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, TestSupportExt as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        input::{Input, InputState},
        slider::{Slider, SliderState},
        switch::Switch,
        tooltip::Tooltip,
    },
    div,
    prelude::FluentBuilder as _,
};
use lobby_client::{Volume, copy};

use super::{
    Callback, MemberRowModel, Tile,
    auth::{Tone, tone_text},
    nested_radius,
};
use crate::scale::{BASE_RADIUS, device_in, u};

/// Wide enough for the longest line, whatever the sidebar's width: the card may reach over the
/// feed.
const OWN_W: f32 = 256.;
const OTHER_W: f32 = 240.;
const LINE_HEIGHT: f32 = 30.;
/// The name's own line: it becomes the field, so nothing below it moves.
const NAME_LINE_H: f32 = 24.;

/// The card's padding around its lines.
pub const MEMBER_CARD_PAD: f32 = 4.;

/// The lines fill that padding, in a card with the kit's radius.
const LINE_RADIUS: f32 = nested_radius(BASE_RADIUS, MEMBER_CARD_PAD);

pub use lobby_client::OwnerAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelStep {
    #[default]
    Actions,
    Confirm(OwnerAction),
    Busy(OwnerAction),
}

/// What the card offers about the Member's Screen Share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatchLine {
    #[default]
    Hidden,
    Watch,
    /// Watching them now.
    StopWatching,
    /// They share, and this person is not In Voice.
    NeedsVoice,
}

impl WatchLine {
    pub fn of(row: &MemberRowModel) -> Self {
        match row.tile.filter(|_| !row.is_me) {
            None => Self::Hidden,
            Some(Tile::Faint) => Self::NeedsVoice,
            Some(Tile::Rest | Tile::Coming) => Self::Watch,
            Some(Tile::OnScreen { .. }) => Self::StopWatching,
        }
    }
}

/// What the line under the name says while the name is being edited, in place of the email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub text: SharedString,
    /// The Lobby refused the name, as opposed to a rule the typed text breaks.
    pub refusal: bool,
}

/// The name on the Member's own card: a line of text with a pencil, or the field with Save.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Rename {
    #[default]
    Idle,
    Editing {
        can_save: bool,
        saving: bool,
        said: Option<Said>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberPanelModel {
    pub name: SharedString,
    pub is_me: bool,
    pub owner: bool,
    pub watch: WatchLine,
    pub dnd: bool,
    /// The signed-in Member is In Voice, so Show Voice stats is there.
    pub in_voice: bool,
    pub manage: bool,
    pub step: PanelStep,
    /// The day they joined, as shown: "12 Mar 2026".
    pub since: SharedString,
    /// The Member's own email, as the Lobby said it.
    pub email: Option<SharedString>,
    pub has_picture: bool,
    /// A picture is being picked, sent or removed.
    pub picture_busy: bool,
    pub rename: Rename,
}

#[derive(Clone)]
pub struct PanelActions {
    pub watch: Callback<()>,
    pub stop_watching: Callback<()>,
    pub reset_volume: Callback<()>,
    pub set_dnd: Callback<bool>,
    pub voice_stats: Callback<()>,
    pub sign_out: Callback<()>,
    pub ask: Callback<Option<OwnerAction>>,
    pub confirm: Callback<OwnerAction>,
    pub start_rename: Callback<()>,
    pub save_rename: Callback<()>,
    pub change_picture: Callback<()>,
    pub remove_picture: Callback<()>,
}

#[derive(IntoElement, Clone)]
pub struct MemberPanel {
    model: MemberPanelModel,
    actions: Option<PanelActions>,
    /// The sidebar's slider, pointed at this Member while their card is open.
    volume: Option<Entity<SliderState>>,
    /// The name field, while the signed-in Member renames themselves.
    name_field: Option<Entity<InputState>>,
}

impl MemberPanel {
    pub fn new(model: MemberPanelModel) -> Self {
        Self {
            model,
            actions: None,
            volume: None,
            name_field: None,
        }
    }

    pub fn actions(mut self, actions: PanelActions) -> Self {
        self.actions = Some(actions);
        self
    }

    pub fn volume(mut self, slider: Entity<SliderState>) -> Self {
        self.volume = Some(slider);
        self
    }

    pub fn name_field(mut self, field: Entity<InputState>) -> Self {
        self.name_field = Some(field);
        self
    }
}

type Press = Box<dyn Fn(&mut Window, &mut App)>;

/// One line the Member can press. `press` is `None` in the gallery.
fn line_button(
    id: &'static str,
    label: impl Into<SharedString>,
    danger: bool,
    press: Option<Press>,
    cx: &App,
) -> impl IntoElement {
    card_line(id, label, danger, cx).when_some(press, |b, press| {
        b.on_click(move |_, window, cx| press(window, cx))
    })
}

/// A line of a card or of a Lobby's menu, its words at the start, for the caller to give a press.
pub(crate) fn card_line(
    id: &'static str,
    label: impl Into<SharedString>,
    danger: bool,
    cx: &App,
) -> Button {
    let label = label.into();
    let theme = cx.theme();
    Button::new(id)
        .debug_selector(move || id.into())
        .accessibility_label(label.clone())
        .ghost()
        .rounded(device_in(LINE_RADIUS, cx))
        .w_full()
        .h(u(LINE_HEIGHT))
        .px_2()
        .justify_start()
        .text_color(if danger {
            theme.danger
        } else {
            theme.foreground
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_left()
                .text_sm()
                .font_weight(FontWeight::NORMAL)
                .child(label),
        )
}

/// `line` with `end`, a small worded button, over its end. `end` blocks the mouse, so a press on
/// it is not a press on the line.
pub(crate) fn line_with_end(line: Button, end: Option<Button>, cx: &App) -> AnyElement {
    div()
        .relative()
        .child(line)
        .children(end.map(|end| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(u(MEMBER_CARD_PAD))
                .flex()
                .items_center()
                .child(
                    end.ghost()
                        .small()
                        .text_color(cx.theme().muted_foreground)
                        .occlude(),
                )
        }))
        .into_any_element()
}

fn text_line(text: String, cx: &App) -> impl IntoElement {
    div()
        .px_2()
        .py_1p5()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// A hairline between groups of lines.
pub(crate) fn separator(cx: &App) -> impl IntoElement {
    div()
        .h(u(1.))
        .mx(u(MEMBER_CARD_PAD))
        .my(u(MEMBER_CARD_PAD))
        .bg(cx.theme().border)
}

fn pressing<T: 'static>(
    action: Option<&Callback<T>>,
    value: impl Fn() -> T + 'static,
) -> Option<Press> {
    let action = action?.clone();
    Some(Box::new(move |window, cx| action(value(), window, cx)))
}

impl RenderOnce for MemberPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let model = self.model;
        let actions = self.actions;
        let (width, body) = if model.is_me {
            (
                OWN_W,
                own_lines(&model, actions.as_ref(), self.name_field.as_ref(), cx)
                    .into_any_element(),
            )
        } else {
            (
                OTHER_W,
                others_lines(&model, actions.as_ref(), self.volume.as_ref(), cx),
            )
        };
        div()
            .id("member-panel")
            .debug_selector(|| "member-panel".into())
            .w(u(width))
            .flex()
            .flex_col()
            .child(body)
    }
}

/// Their name in the card's weight, with the Owner pill after it when they own the Lobby.
fn name_and_pill(model: &MemberPanelModel, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .id("member-panel-name")
                .debug_selector(|| "member-panel-name".into())
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(model.name.clone())
                .test_support(),
        )
        .when(model.owner, |d| {
            d.child(
                div()
                    .flex_shrink_0()
                    .px(u(7.))
                    .py(u(1.))
                    .rounded_full()
                    .bg(theme.secondary)
                    .text_size(u(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(copy::OWNER_PILL),
            )
        })
}

fn caption(
    id: &'static str,
    text: impl Into<SharedString>,
    color: gpui_kit::Hsla,
    tip: Option<&'static str>,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .truncate()
        .text_xs()
        .text_color(color)
        .child(text.into())
        .when_some(tip, |d, tip| {
            d.tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        })
        .test_support()
}

/// The card's top. Another Member's is their name and since when. The signed-in Member's has the
/// pencil, and their email under the name; the pencil turns the name's own line into a field with
/// Save, and a refusal takes the email's line.
fn head(
    model: &MemberPanelModel,
    actions: Option<&PanelActions>,
    field: Option<&Entity<InputState>>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let editing = match &model.rename {
        Rename::Editing {
            can_save,
            saving,
            said,
        } if model.is_me => Some((*can_save, *saving, said.clone())),
        _ => None,
    };
    let (title, line): (AnyElement, AnyElement) = match editing {
        Some((can_save, saving, said)) => {
            let save = pressing(actions.map(|a| &a.save_rename), || ());
            let title = div()
                .h(u(NAME_LINE_H))
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().flex_1().min_w_0().when_some(field, |d, field| {
                    // `Input::h` is for multi-line fields: the line's height goes by the style.
                    d.child(
                        Styled::h(
                            Input::new(field)
                                .id("rename-field")
                                .xsmall()
                                .focus_bordered(false),
                            u(NAME_LINE_H),
                        )
                        .border_color(theme.ring),
                    )
                }))
                .child(
                    Button::new("rename-save")
                        .debug_selector(|| "rename-save".into())
                        .accessibility_label("Save name")
                        .primary()
                        .h(u(NAME_LINE_H))
                        .px_2p5()
                        .text_xs()
                        .label(copy::SAVE)
                        .loading(saving)
                        .disabled(!can_save)
                        .when_some(save, |b, save| {
                            b.on_click(move |_, window, cx| save(window, cx))
                        }),
                )
                .into_any_element();
            let line = match said {
                Some(said) => caption(
                    "rename-said",
                    said.text,
                    if said.refusal {
                        tone_text(Tone::Danger, cx)
                    } else {
                        muted
                    },
                    None,
                )
                .into_any_element(),
                None => email_line(model, muted).into_any_element(),
            };
            (title, line)
        }
        None => {
            let start = pressing(actions.map(|a| &a.start_rename), || ());
            let title = div()
                .h(u(NAME_LINE_H))
                .flex()
                .items_center()
                .gap_1()
                .child(name_and_pill(model, cx))
                .when(model.is_me, |d| {
                    d.child(
                        div().ml_auto().flex_shrink_0().child(
                            Button::new("rename-open")
                                .debug_selector(|| "rename-open".into())
                                .accessibility_label(copy::RENAME)
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::Pencil))
                                .tooltip(copy::RENAME)
                                .when_some(start, |b, start| {
                                    b.on_click(move |_, window, cx| start(window, cx))
                                }),
                        ),
                    )
                })
                .into_any_element();
            let line = if model.is_me {
                email_line(model, muted).into_any_element()
            } else {
                caption(
                    "member-since",
                    copy::member_since(&model.since),
                    muted,
                    None,
                )
                .into_any_element()
            };
            (title, line)
        }
    };
    div()
        .px_2()
        .py_1p5()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(title)
        .child(line)
}

/// The Magic Codes' address; a line of the same height when the Lobby has not said it yet.
fn email_line(model: &MemberPanelModel, muted: gpui_kit::Hsla) -> impl IntoElement {
    caption(
        "card-email",
        model.email.clone().unwrap_or_default(),
        muted,
        Some(copy::MAGIC_CODES_GO_HERE),
    )
}

/// Watch their screen, or stop.
fn watch_block(
    line: WatchLine,
    actions: Option<&PanelActions>,
    name: &SharedString,
) -> Option<AnyElement> {
    let (id, label, icon) = match line {
        WatchLine::Hidden => return None,
        WatchLine::Watch | WatchLine::NeedsVoice => ("member-watch", "Watch", IconName::Eye),
        WatchLine::StopWatching => ("member-stop-watching", "Stop watching", IconName::EyeOff),
    };
    let press = match line {
        WatchLine::Watch => pressing(actions.map(|a| &a.watch), || ()),
        WatchLine::StopWatching => pressing(actions.map(|a| &a.stop_watching), || ()),
        WatchLine::Hidden | WatchLine::NeedsVoice => None,
    };
    let button = Button::new(id)
        .debug_selector(move || id.into())
        .accessibility_label(match line {
            WatchLine::StopWatching => format!("Stop watching {name}"),
            _ => format!("Watch {name}'s screen"),
        })
        .small()
        .w_full()
        .icon(Icon::new(icon))
        .label(label)
        .map(|b| match line {
            WatchLine::Watch => b.primary(),
            _ => b.outline(),
        })
        .when(line == WatchLine::NeedsVoice, |b| {
            b.disabled(true).tooltip(copy::SHARE_NEEDS_VOICE)
        })
        .when_some(press, |b, press| {
            b.on_click(move |_, window, cx| press(window, cx))
        });
    Some(div().px_2().pb_1().child(button).into_any_element())
}

/// Wide enough for "200%", so the percent and the reset beside it never move.
const PERCENT_W: f32 = 48.;

/// How loud they play for this person: the label, the percent and a reset that is always there
/// and disabled at 100, over the slider.
fn volume_block(slider: &Entity<SliderState>, reset: Option<Press>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let percent = slider.read(cx).value().start().round() as u16;
    let off_default = percent != Volume::DEFAULT.percent();
    div()
        .id("member-volume")
        .debug_selector(|| "member-volume".into())
        .px_2()
        .pt_1()
        .pb_2()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .h(u(22.))
                .flex()
                .items_center()
                .justify_between()
                .text_sm()
                .child(div().text_color(theme.muted_foreground).child("Volume"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .w(u(PERCENT_W))
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .text_right()
                                .child(format!("{percent}%")),
                        )
                        .child(
                            Button::new("volume-reset")
                                .debug_selector(|| "volume-reset".into())
                                .accessibility_label("Reset volume to 100%")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::RotateCcw))
                                .tooltip("Reset to 100%")
                                .disabled(!off_default)
                                .when_some(reset, |b, reset| {
                                    b.on_click(move |_, window, cx| reset(window, cx))
                                }),
                        ),
                ),
        )
        .child(
            div()
                .id("volume-slider")
                .debug_selector(|| "volume-slider".into())
                .child(
                    Slider::new(slider)
                        .horizontal()
                        .accessibility_label("Volume"),
                )
                .test_support(),
        )
}

/// Change picture… with, while there is a picture, Remove at the line's end.
fn picture_line(model: &MemberPanelModel, actions: Option<&PanelActions>, cx: &App) -> AnyElement {
    let change =
        pressing(actions.map(|a| &a.change_picture), || ()).filter(|_| !model.picture_busy);
    let remove = pressing(actions.map(|a| &a.remove_picture), || ());
    let line = card_line("picture-change", copy::CHANGE_PICTURE, false, cx)
        .loading(model.picture_busy)
        .when_some(change, |b, change| {
            b.on_click(move |_, window, cx| change(window, cx))
        });
    let end = model.has_picture.then(|| {
        Button::new("picture-remove")
            .debug_selector(|| "picture-remove".into())
            .accessibility_label("Remove picture")
            .label(copy::REMOVE)
            .disabled(model.picture_busy)
            .when_some(remove, |b, remove| {
                b.on_click(move |_, window, cx| remove(window, cx))
            })
    });
    line_with_end(line, end, cx)
}

/// Do Not Disturb as a line whose end is the switch; the line is what takes the press.
fn dnd_line(dnd: bool, press: Option<Press>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    Button::new("presence-dnd")
        .debug_selector(|| "presence-dnd".into())
        .accessibility_label(copy::DO_NOT_DISTURB)
        .ghost()
        .rounded(device_in(LINE_RADIUS, cx))
        .w_full()
        .h(u(LINE_HEIGHT))
        .px_2()
        .text_color(theme.foreground)
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .text_sm()
                .font_weight(FontWeight::NORMAL)
                .child(copy::DO_NOT_DISTURB)
                .child(Switch::new("dnd-switch").checked(dnd)),
        )
        .when_some(press, |b, press| {
            b.on_click(move |_, window, cx| press(window, cx))
        })
}

fn own_lines(
    model: &MemberPanelModel,
    actions: Option<&PanelActions>,
    field: Option<&Entity<InputState>>,
    cx: &App,
) -> impl IntoElement {
    let dnd = model.dnd;
    div()
        .flex()
        .flex_col()
        .child(head(model, actions, field, cx))
        .child(separator(cx))
        .child(picture_line(model, actions, cx))
        .child(separator(cx))
        .child(dnd_line(
            dnd,
            pressing(actions.map(|a| &a.set_dnd), move || !dnd),
            cx,
        ))
        .when(model.in_voice, |d| {
            d.child(line_button(
                "voice-stats-open",
                copy::SHOW_VOICE_STATS,
                false,
                pressing(actions.map(|a| &a.voice_stats), || ()),
                cx,
            ))
        })
        .child(separator(cx))
        .child(line_button(
            "sign-out",
            copy::SIGN_OUT,
            false,
            pressing(actions.map(|a| &a.sign_out), || ()),
            cx,
        ))
}

fn others_lines(
    model: &MemberPanelModel,
    actions: Option<&PanelActions>,
    volume: Option<&Entity<SliderState>>,
    cx: &App,
) -> AnyElement {
    let name = &model.name;
    let ask = actions.map(|a| &a.ask);
    let pending = match model.step {
        PanelStep::Actions => None,
        PanelStep::Confirm(a) => Some((a, false)),
        PanelStep::Busy(a) => Some((a, true)),
    };
    let Some((action, busy)) = pending.filter(|_| model.manage) else {
        return div()
            .flex()
            .flex_col()
            .child(head(model, actions, None, cx))
            .children(watch_block(model.watch, actions, name))
            .children(volume.map(|slider| {
                volume_block(
                    slider,
                    pressing(actions.map(|a| &a.reset_volume), || ()),
                    cx,
                )
            }))
            .when(model.manage, |d| {
                d.child(separator(cx))
                    .child(line_button(
                        "make-owner",
                        copy::make_owner_line(name),
                        false,
                        pressing(ask, || Some(OwnerAction::MakeOwner)),
                        cx,
                    ))
                    .child(line_button(
                        "remove-member",
                        copy::remove_member_line(name),
                        true,
                        pressing(ask, || Some(OwnerAction::Remove)),
                        cx,
                    ))
            })
            .into_any_element();
    };
    let confirm = actions.map(|a| a.confirm.clone());
    let cancel = pressing(ask, || None);
    let (sentence, confirm_id, confirm_label) = match action {
        OwnerAction::Remove => (
            copy::remove_member_confirm(name),
            "confirm-remove",
            copy::REMOVE,
        ),
        OwnerAction::MakeOwner => (
            copy::make_owner_confirm(name),
            "confirm-make-owner",
            "Make owner",
        ),
    };
    div()
        .id("member-panel-confirm")
        .debug_selector(|| "member-panel-confirm".into())
        .flex()
        .flex_col()
        .child(text_line(sentence, cx))
        .child(
            div()
                .px_2()
                .pb_1()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("panel-cancel")
                        .debug_selector(|| "panel-cancel".into())
                        .small()
                        .ghost()
                        .label("Cancel")
                        .disabled(busy)
                        .when_some(cancel, |b, cancel| {
                            b.on_click(move |_, window, cx| cancel(window, cx))
                        }),
                )
                .child(
                    Button::new(confirm_id)
                        .debug_selector(move || confirm_id.into())
                        .small()
                        .label(confirm_label)
                        .map(|b| match action {
                            OwnerAction::Remove => b.danger(),
                            OwnerAction::MakeOwner => b.primary(),
                        })
                        .loading(busy)
                        .when_some(confirm, move |b, confirm| {
                            b.on_click(move |_, window, cx| confirm(action, window, cx))
                        }),
                ),
        )
        .into_any_element()
}

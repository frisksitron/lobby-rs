//! The auth screens reserve space for feedback, so it never moves what is below it.
//! Each step's content enters while the rail stays fixed. Narrow windows reduce the column's
//! side air down to [`SIDE_AIR`].

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, Div, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        spinner::Spinner,
        tooltip::Tooltip,
    },
    div, ease_out_quint,
    prelude::FluentBuilder as _,
};
use lobby_client::{Chip, ChipAt, Stage, copy};

use super::{Avatar, AvatarSize, Callback, ICON_SM, ICON_XS, PULSE_FPS, controls::step_button};
use crate::scale::u;

/// The default window (680) less 48 of side air each way.
const COLUMN_W: f32 = 584.;
/// The least side air, at the small window's 480.
const SIDE_AIR: f32 = 16.;
const TOP_AIR: f32 = 28.;
const TOP_AIR_MIN: f32 = 16.;
const QUESTION_GAP: f32 = 56.;
const QUESTION_GAP_MIN: f32 = 20.;
const QUESTION_SIZE: f32 = 28.;
const QUESTION_LINE: f32 = 34.;
const SUBTITLE_GAP: f32 = 6.;
const BODY_GAP: f32 = 20.;
const CONTROL_GAP: f32 = 8.;
/// Room for a whole email or name: a longer button goes under the field.
const FIELD_MIN_W: f32 = 260.;
/// Two lines of small text, reserved under the field so feedback never moves what is below it.
const FEEDBACK_HEIGHT: f32 = 40.;
const FEEDBACK_GAP: f32 = 8.;
const LINK_H: f32 = 28.;
const CHIP_H: f32 = 28.;
const CHIP_PAD_X: f32 = 10.;
const CHIP_TEXT: f32 = 13.;
const RAIL_GAP: f32 = 8.;
/// Tighter between wrapped rows: the small window's rail takes three of them.
const RAIL_ROW_GAP: f32 = 6.;
/// A chevron sits against the chip it leads into, so a wrapped row never ends on an arrow. The
/// small gap keeps the small window's longest email, with its chevron, on one line.
const CHEVRON_GAP: f32 = 4.;
/// The status mark above Waiting's and Closed's question.
pub const STEP_MARK: f32 = 20.;
const MARK_GAP: f32 = 6.;
/// What the waiting mark's ring adds to its disc, and so what a still mark reserves.
const MARK_RING: f32 = 6.;

/// A step's content entering: ease-out, short enough to never be waited on.
const ENTER: Duration = Duration::from_millis(220);
const ENTER_RISE: f32 = 8.;

pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feedback {
    Error(SharedString),
    Notice(SharedString),
    Info(SharedString),
    Hint(SharedString),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Success,
    Danger,
    Info,
}

pub fn tone_text(tone: Tone, cx: &App) -> Hsla {
    let theme = cx.theme();
    match tone {
        Tone::Success => theme.success,
        Tone::Danger => theme.danger,
        Tone::Info => theme.info,
    }
}

/// The reserved line under a step's field: what was said, with its tone's icon, or nothing.
pub fn feedback_line(line: Option<Feedback>, cx: &App) -> impl IntoElement {
    feedback_slot(line.map(|line| feedback_text(line, cx).into_any_element()))
}

/// The reserved line holding `content` (a fix's button beside its words, say) in its place.
pub fn feedback_slot(content: Option<AnyElement>) -> impl IntoElement {
    div()
        .id("error")
        .debug_selector(|| "auth-feedback".into())
        .w_full()
        .h(u(FEEDBACK_HEIGHT))
        .mt(u(FEEDBACK_GAP))
        .flex_shrink_0()
        .overflow_hidden()
        .flex()
        .items_start()
        .text_sm()
        .line_height(u(18.))
        .children(content)
        .test_support()
}

/// The reserved line while a step with no button of its own is checking: a spinner and `text`.
pub fn busy_line(text: &'static str, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    feedback_slot(Some(
        div()
            .w_full()
            .flex()
            .items_start()
            .gap_1p5()
            .text_color(muted)
            .child(
                div()
                    .flex_shrink_0()
                    .pt(u(2.))
                    .child(Spinner::new().small().color(muted)),
            )
            .child(text)
            .into_any_element(),
    ))
}

pub fn feedback_text(line: Feedback, cx: &App) -> Div {
    let toned = |icon: IconName, tone: Tone, text: SharedString| {
        let color = tone_text(tone, cx);
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_start()
            .gap_1p5()
            .text_color(color)
            .child(
                div()
                    .flex_shrink_0()
                    .pt(u(2.))
                    .child(Icon::new(icon).size(u(ICON_SM)).text_color(color)),
            )
            .child(
                div()
                    .debug_selector(|| "auth-feedback-text".into())
                    .flex_1()
                    .min_w_0()
                    .line_clamp(2)
                    .child(text),
            )
    };
    match line {
        Feedback::Error(m) => toned(IconName::CircleAlert, Tone::Danger, m),
        Feedback::Notice(m) => toned(IconName::CircleCheck, Tone::Success, m),
        Feedback::Info(m) => toned(IconName::Info, Tone::Info, m),
        Feedback::Hint(h) => div()
            .w_full()
            .min_w_0()
            .text_color(cx.theme().muted_foreground)
            .line_clamp(2)
            .child(h),
    }
}

/// The step's control row: the field takes the room its button leaves. When the button's words
/// are too long for both, it goes under the field instead of cutting them.
pub fn control_row(field: impl IntoElement, button: impl IntoElement) -> Div {
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(u(CONTROL_GAP))
        .child(div().flex_1().min_w(u(FIELD_MIN_W)).child(field))
        .child(div().flex_none().max_w_full().child(button))
}

pub fn inline_row(cells: impl IntoElement, button: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(u(CONTROL_GAP))
        .child(cells)
        .child(button)
}

/// The step's primary button: its words become the busy words with a spinner.
pub fn primary_button(
    id: &'static str,
    label: &str,
    busy_label: &str,
    busy: bool,
    cx: &App,
) -> Button {
    step_button(id, cx)
        .primary()
        .label(if busy { busy_label } else { label }.to_owned())
        .when(busy, |b| b.icon(Icon::new(IconName::LoaderCircle)))
        .loading(busy)
}

pub fn quiet_link(
    id: &'static str,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
        .ghost()
        .h(u(LINK_H))
        .px(u(8.))
        .ml(u(-8.))
        .label(label)
        .on_click(on_click)
}

/// The one look of every "go back" action on the step screens: a bordered quiet button with an
/// arrow, at the field's height.
pub fn back_button(id: &'static str, label: impl Into<SharedString>, cx: &App) -> Button {
    step_button(id, cx)
        .outline()
        .icon(Icon::new(IconName::ArrowLeft))
        .label(label)
}

/// Names a control for tests and screenshots (`debug_bounds`) without changing its layout.
pub fn tag(name: &'static str, el: impl IntoElement) -> Div {
    div().debug_selector(move || name.to_owned()).child(el)
}

/// Entry's rail, drawn. Only a done chip that says it goes back takes a press.
#[derive(IntoElement)]
pub struct StepRail {
    rail: Vec<Chip>,
    on_go: Option<Callback<Stage>>,
    lobby: Option<(SharedString, Hsla)>,
}

impl StepRail {
    pub fn new(rail: Vec<Chip>) -> Self {
        Self {
            rail,
            on_go: None,
            lobby: None,
        }
    }

    /// The Lobby's own mark, which the done Lobby chip leads with in place of its check.
    pub fn lobby_mark(mut self, name: impl Into<SharedString>, color: Hsla) -> Self {
        self.lobby = Some((name.into(), color));
        self
    }

    pub fn on_go(mut self, on_go: impl Fn(Stage, &mut Window, &mut App) + 'static) -> Self {
        self.on_go = Some(Rc::new(on_go));
        self
    }
}

fn stage_id(stage: Stage) -> &'static str {
    match stage {
        Stage::Lobby => "chip-lobby",
        Stage::Email => "chip-email",
        Stage::Code => "chip-code",
        Stage::Name => "chip-name",
    }
}

fn goes_back_to(stage: Stage) -> &'static str {
    match stage {
        Stage::Lobby => copy::CHANGE_LOBBY,
        Stage::Email | Stage::Code | Stage::Name => copy::USE_DIFFERENT_EMAIL,
    }
}

impl RenderOnce for StepRail {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, done, done_hover, current, current_fg) = (
            theme.foreground,
            theme.muted_foreground,
            theme.secondary,
            theme.secondary_hover,
            theme.sidebar_accent,
            theme.primary,
        );
        let lobby = self.lobby;
        let chips = self.rail.into_iter().enumerate().map(|(ix, chip)| {
            let id = stage_id(chip.stage);
            let label = copy::stage_label(chip.stage);
            let base = div()
                .id(id)
                .debug_selector(move || id.into())
                .min_h(u(CHIP_H))
                .max_w_full()
                .px(u(CHIP_PAD_X))
                .py(u(4.))
                .flex()
                .items_center()
                .gap(u(6.))
                .rounded(u(CHIP_H / 2.))
                .text_size(u(CHIP_TEXT))
                .line_height(u(18.))
                .font_weight(FontWeight::SEMIBOLD);
            let chip_el = match chip.at {
                ChipAt::Done { value, back } => {
                    let text: SharedString = value.unwrap_or_else(|| label.to_owned()).into();
                    let lead = match (&lobby, chip.stage) {
                        (Some((name, color)), Stage::Lobby) => Avatar::lobby(name.clone(), *color)
                            .size(AvatarSize::Chip)
                            .into_any_element(),
                        _ => Icon::new(IconName::Check)
                            .size(u(ICON_XS))
                            .text_color(fg)
                            .flex_shrink_0()
                            .into_any_element(),
                    };
                    base.bg(done)
                        .text_color(fg)
                        .child(lead)
                        .child(div().min_w_0().child(text))
                        .when_some(self.on_go.clone().filter(|_| back), |d, on_go| {
                            let stage = chip.stage;
                            let tip = goes_back_to(stage);
                            d.cursor_pointer()
                                .hover(|s| s.bg(done_hover))
                                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                                .on_click(move |_, window, cx| on_go(stage, window, cx))
                        })
                }
                ChipAt::Current => base.bg(current).text_color(current_fg).child(label),
                ChipAt::Ahead => base.text_color(muted).child(label),
            };
            div()
                .flex()
                .items_center()
                .gap(u(CHEVRON_GAP))
                .max_w_full()
                .when(ix > 0, |d| {
                    d.child(
                        Icon::new(IconName::ChevronRight)
                            .size(u(ICON_XS))
                            .text_color(muted)
                            .flex_shrink_0(),
                    )
                })
                .child(chip_el.test_support())
        });
        div()
            .debug_selector(|| "auth-rail".into())
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(u(RAIL_GAP))
            .gap_y(u(RAIL_ROW_GAP))
            .children(chips)
    }
}

#[derive(IntoElement)]
pub struct AuthScreen {
    rail: StepRail,
    lead: Option<AnyElement>,
    title: SharedString,
    subtitle: SharedString,
    body: Vec<AnyElement>,
    link: Option<AnyElement>,
    entered: Option<SharedString>,
}

impl AuthScreen {
    pub fn new(
        rail: StepRail,
        title: impl Into<SharedString>,
        subtitle: impl Into<SharedString>,
    ) -> Self {
        Self {
            rail,
            lead: None,
            title: title.into(),
            subtitle: subtitle.into(),
            body: Vec::new(),
            link: None,
            entered: None,
        }
    }

    /// Names the step shown: what is under the rail enters once whenever the name is a new one.
    pub fn entered(mut self, step: impl Into<SharedString>) -> Self {
        self.entered = Some(step.into());
        self
    }

    pub fn lead(mut self, lead: impl IntoElement) -> Self {
        self.lead = Some(lead.into_any_element());
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.body.push(child.into_any_element());
        self
    }

    pub fn link(mut self, link: impl IntoElement) -> Self {
        self.link = Some(link.into_any_element());
        self
    }
}

impl RenderOnce for AuthScreen {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, background) = (theme.foreground, theme.muted_foreground, theme.background);
        let question = div()
            .w_full()
            .flex()
            .flex_col()
            .items_start()
            .gap(u(MARK_GAP))
            .children(
                self.lead
                    .map(|lead| div().debug_selector(|| "auth-mark".into()).child(lead)),
            )
            .child(
                div()
                    .debug_selector(|| "auth-title".into())
                    .w_full()
                    .min_w_0()
                    .text_size(u(QUESTION_SIZE))
                    .line_height(u(QUESTION_LINE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .line_clamp(2)
                    .child(self.title),
            );
        let below = div()
            .w_full()
            .flex_grow(1.)
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(air(QUESTION_GAP_MIN, QUESTION_GAP))
            .child(question)
            .child(
                div()
                    .debug_selector(|| "auth-subtitle".into())
                    .mt(u(SUBTITLE_GAP))
                    .text_sm()
                    .line_height(u(20.))
                    .text_color(muted)
                    .child(self.subtitle),
            )
            .child(
                div()
                    .mt(u(BODY_GAP))
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_start()
                    .children(self.body),
            )
            .children(
                self.link
                    .map(|link| div().min_h(u(LINK_H)).flex().items_center().child(link)),
            )
            // What the steps leave free goes below them, once the gaps above are full.
            .child(div().flex_grow(1.));
        // The rail stays put. What is under it fades in and rises into the place it already has,
        // an offset that never takes part in the layout.
        let below = match self.entered {
            Some(step) => below
                .relative()
                .with_animation(
                    SharedString::from(format!("auth-enter-{step}")),
                    Animation::new(ENTER).with_easing(ease_out_quint()),
                    |d, t| d.top(u(ENTER_RISE * (1. - t))).opacity(t),
                )
                .into_any_element(),
            None => below.into_any_element(),
        };
        let column = div()
            .id("auth-column")
            .debug_selector(|| "auth-column".into())
            .w_full()
            .max_w(u(COLUMN_W))
            .flex_grow(1.)
            .flex()
            .flex_col()
            .child(self.rail)
            .child(below)
            .test_support();
        div()
            .id("auth")
            .debug_selector(|| "auth".into())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .bg(background)
            .child(
                div()
                    .w_full()
                    .min_h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .px(u(SIDE_AIR))
                    .pb(u(SIDE_AIR))
                    .child(air(TOP_AIR_MIN, TOP_AIR))
                    .child(column),
            )
            .test_support()
    }
}

/// A gap that is `max` tall when the window has the room and gives way to `min` when it is short,
/// so one layout fits the default window and the small one.
fn air(min: f32, max: f32) -> Div {
    div()
        .flex_grow(1.)
        .flex_shrink_0()
        .min_h(u(min))
        .max_h(u(max))
}

/// A mark that breathes while someone else is deciding (Waiting for a Vouch).
pub fn waiting_mark(icon: IconName, accent: Hsla, size: f32) -> impl IntoElement {
    use gpui_kit::pulsating_between;
    let ring = size + MARK_RING;
    div()
        .relative()
        .size(u(ring))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size(u(ring))
                .rounded_full()
                .border_2()
                .border_color(accent.opacity(0.35))
                .with_animation(
                    "waiting-ring",
                    Animation::new(Duration::from_millis(2400))
                        .repeat()
                        .with_max_fps(PULSE_FPS)
                        .with_easing(pulsating_between(0.25, 1.0)),
                    |ring, delta| ring.opacity(delta),
                ),
        )
        .child(super::IconDisc::new(icon, size).accent(accent))
}

/// A still status mark, the waiting mark's size, so the question beside it does not move.
pub fn status_mark(icon: IconName, accent: Hsla, size: f32) -> impl IntoElement {
    div()
        .size(u(size + MARK_RING))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(super::IconDisc::new(icon, size + MARK_RING).accent(accent))
}

pub fn busy_screen(id: &'static str, text: &'static str, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .text_sm()
        .text_color(muted)
        .child(Spinner::new().color(muted))
        .child(text)
}

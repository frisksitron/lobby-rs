//! The popover hangs from the title bar and never covers it: its height is capped at the window
//! below the bar, the header stays and the sections scroll. A handled request keeps its row for a
//! few seconds with what happened, then folds away, so the list never jumps under the pointer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Point, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    base::{
        ElementExt as _, POPUP_PRIORITY, Popover, Positioner, Scrollbar, ScrollbarAxis, v_flex,
    },
    canvas, deferred, div,
    prelude::FluentBuilder as _,
    relative,
};
use lobby_client::{
    Issue, IssueAction, IssueKind, LobbyState, Severity, Source, copy, local_date_time, local_time,
    time_ago,
};
use lobby_protocol::{AccessRequestId, UnixMs};

use crate::kit::ActiveTheme as _;
use crate::scale::{design_viewport, u};
use crate::ui::{
    self, Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle, Icon,
    motion::{Reveal, Side},
};
use crate::{
    components::{
        BAR_CONTROL_H, BarButton, Callback, ConnectionAction, ConnectionNotice, ConnectionTone,
        Detail, ICON_SM, IconDisc, LEAD_SIZE, Lead, NotificationEntry, TITLE_BAR_HEIGHT,
        auth::Tone, auth::tone_text, section_label, tone_color, voice_accent,
    },
    issues::IssueCenter,
};

const ENTRY_PAD: f32 = 8.;
const POPOVER_OFFSET: f32 = 4.;
pub const SETTLE_FOR: Duration = Duration::from_secs(4);
pub const FOLD_FOR: Duration = Duration::from_millis(180);
const CENTER_WIDTH: f32 = 380.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRow {
    pub issue: Issue,
    pub title: SharedString,
    pub message: SharedString,
    pub time: SharedString,
    pub unseen: bool,
    pub action: Option<RowAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowAction {
    pub label: &'static str,
    pub icon: IconName,
    pub enabled: bool,
    pub waits_for: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionContext {
    pub connected: bool,
    pub can_resend: bool,
}

/// The button of an Issue's action, enabled while the action can be done.
pub fn row_action(action: IssueAction, ctx: ActionContext) -> RowAction {
    match action {
        IssueAction::Rejoin => RowAction {
            label: "Rejoin",
            icon: IconName::Phone,
            enabled: ctx.connected,
            waits_for: (!ctx.connected).then_some("Available when the Lobby is back"),
        },
        IssueAction::SendAgain => RowAction {
            label: "Send again",
            icon: IconName::RotateCcw,
            enabled: ctx.can_resend,
            waits_for: (!ctx.can_resend).then_some("The Message is no longer in the composer"),
        },
        IssueAction::ShowInFolder => RowAction {
            label: "Show in folder",
            icon: IconName::FolderOpen,
            enabled: true,
            waits_for: None,
        },
    }
}

pub fn action_button(id: impl Into<SharedString>, action: RowAction) -> ui::Button {
    let button = ui::Button::new(id.into())
        .size(ui::Size::Word)
        .icon(action.icon)
        .label(action.label);
    match action.waits_for {
        Some(why) if !action.enabled => button
            .variant(ui::Variant::Outline)
            .disabled(true)
            .tooltip(why),
        _ => button
            .variant(ui::Variant::Primary)
            .disabled(!action.enabled),
    }
}

pub fn notification_icon(severity: Severity, source: Source, cx: &App) -> IconDisc {
    IconDisc::new(source_icon(source), LEAD_SIZE).accent(severity_color(severity, cx))
}

pub fn notification_rows(center: &IssueCenter, ctx: ActionContext) -> Vec<NotificationRow> {
    // The Issues are in order of raising, so the index breaks a tie within a millisecond.
    let mut rows: Vec<((UnixMs, usize), NotificationRow)> = center
        .issues()
        .iter()
        .enumerate()
        .map(|(ix, i)| {
            (
                (i.raised_at, ix),
                NotificationRow {
                    issue: i.clone(),
                    title: copy::issue_title(i.kind).into(),
                    message: i.message.to_string().into(),
                    time: local_time(i.raised_at).into(),
                    unseen: center.is_unseen(i.kind),
                    action: i.kind.action().map(|a| row_action(a, ctx)),
                },
            )
        })
        .collect();
    rows.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    rows.into_iter().map(|(_, r)| r).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    pub id: AccessRequestId,
    pub username: SharedString,
    pub label: SharedString,
    pub created_at: UnixMs,
    pub ago: SharedString,
    pub when: SharedString,
    pub stage: RowStage,
}

/// What a request's row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStage {
    /// Vouch and Decline, both disabled while the Lobby answers one of them.
    Waiting { deciding: bool },
    /// What happened to the request. A Decline can still be undone.
    Settled(Settled),
    /// The same, folding away: no Undo any more.
    Folding(Settled),
}

impl RowStage {
    pub fn settled(self) -> Option<Settled> {
        match self {
            Self::Waiting { .. } => None,
            Self::Settled(how) | Self::Folding(how) => Some(how),
        }
    }

    pub fn deciding(self) -> bool {
        matches!(self, Self::Waiting { deciding: true })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    VouchedIn,
    Declining,
}

impl DecisionRow {
    pub fn new(request: &lobby_protocol::AccessRequest, now: UnixMs) -> Self {
        Self {
            id: request.id.clone(),
            username: request.username.to_string().into(),
            label: request.requester_label.clone().into(),
            created_at: request.created_at,
            ago: time_ago(request.created_at, now).into(),
            when: local_date_time(request.created_at).into(),
            stage: RowStage::Waiting { deciding: false },
        }
    }
}

pub fn decision_rows(state: &LobbyState, now: UnixMs) -> Vec<DecisionRow> {
    state
        .access_requests
        .iter()
        .map(|r| DecisionRow {
            stage: RowStage::Waiting {
                deciding: state.deciding.contains(&r.id),
            },
            ..DecisionRow::new(r, now)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decide {
    Vouch,
    Decline,
    Undo,
}

pub type DecideHandler = Rc<dyn Fn(AccessRequestId, Decide, &mut Window, &mut App)>;

pub fn severity_color(severity: Severity, cx: &App) -> Hsla {
    let theme = cx.theme();
    match severity {
        Severity::Info => theme.info,
        Severity::Warning => theme.warning,
        Severity::Error => theme.danger,
    }
}

pub fn source_icon(source: Source) -> IconName {
    match source {
        Source::Connection => IconName::Unplug,
        Source::Message => IconName::MessageCircle,
        Source::Voice => IconName::AudioLines,
        Source::Presence => IconName::CircleDot,
        Source::Admission => IconName::UserPlus,
        Source::Device => IconName::HardDrive,
    }
}

#[derive(IntoElement)]
pub struct NotificationBell {
    rows: Vec<NotificationRow>,
    unseen: usize,
    on_dismiss: Callback<Option<IssueKind>>,
    open: bool,
    on_open_change: Callback<bool>,
    on_action: Callback<Issue>,
    decisions: Vec<DecisionRow>,
    on_decide: Option<DecideHandler>,
    connection: Option<(ConnectionNotice, Callback<ConnectionAction>)>,
}

impl NotificationBell {
    pub fn new(
        rows: Vec<NotificationRow>,
        unseen: usize,
        on_dismiss: Callback<Option<IssueKind>>,
        open: bool,
        on_open_change: Callback<bool>,
        on_action: Callback<Issue>,
    ) -> Self {
        Self {
            rows,
            unseen,
            on_dismiss,
            open,
            on_open_change,
            on_action,
            decisions: Vec::new(),
            on_decide: None,
            connection: None,
        }
    }

    /// The live connection entry, on top while the Lobby is not calm.
    pub fn connection(
        mut self,
        notice: Option<ConnectionNotice>,
        on_action: Callback<ConnectionAction>,
    ) -> Self {
        self.connection = notice.map(|n| (n, on_action));
        self
    }

    pub fn decisions(mut self, rows: Vec<DecisionRow>, on_decide: DecideHandler) -> Self {
        self.decisions = rows;
        self.on_decide = Some(on_decide);
        self
    }
}

impl RenderOnce for NotificationBell {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, warning, accent) = (theme.danger, theme.warning, voice_accent(theme));
        let waiting = self
            .decisions
            .iter()
            .any(|d| matches!(d.stage, RowStage::Waiting { .. }));
        // The dot says something wants a look: something went wrong (red), the Lobby is not
        // calm (its tone), or people wait to join (the app's accent).
        let dot = match &self.connection {
            _ if self.unseen > 0 => Some(danger),
            Some((notice, _)) => Some(match notice.tone {
                ConnectionTone::Danger => danger,
                _ => warning,
            }),
            None => waiting.then_some(accent),
        };
        let trigger = ui::Button::new("notification-bell")
            .size(ui::Size::Bar)
            .icon(IconName::Bell)
            .accessibility_label("Notifications")
            .tooltip("Notifications");
        // Hanging from the title bar, never over it: the window below the bar, less a margin, the
        // popover's padding and its offset from the bell.
        let max_h = (design_viewport(window).1 - TITLE_BAR_HEIGHT - 16. - 24. - 8.).max(120.);
        let (on_open_change, open) = (self.on_open_change, self.open);
        let reveal = Reveal::new("notification-center", open, window, cx);
        // Base's popover drops its content as it closes, so the closing fade draws the same list
        // where it last stood, inert. The two share the scroll and the row heights, which live
        // until the fade ends: the next opening starts at the top.
        let (scroll, heights) = if reveal.is_drawn() {
            let scroll = window
                .use_keyed_state("notification-sections-scroll", cx, |_, _| {
                    gpui_kit::ScrollHandle::new()
                })
                .read(cx)
                .clone();
            let heights = window
                .use_keyed_state("notification-row-heights", cx, |_, _| RowHeights::default())
                .read(cx)
                .clone();
            (scroll, heights)
        } else {
            (gpui_kit::ScrollHandle::new(), RowHeights::default())
        };
        let stood = window
            .use_keyed_state("notification-center-stood", cx, |_, _| {
                Rc::new(Cell::new(Point::default()))
            })
            .read(cx)
            .clone();
        let list = {
            let (rows, decisions, connection) = (self.rows, self.decisions, self.connection);
            let (on_dismiss, on_action, on_decide) =
                (self.on_dismiss, self.on_action, self.on_decide);
            move |cx: &App| {
                let list = render_list(
                    Contents {
                        connection: connection.as_ref().map(|(n, h)| (n, h)),
                        decisions: &decisions,
                        rows: &rows,
                    },
                    &on_dismiss,
                    &on_action,
                    on_decide.as_ref(),
                    Viewport {
                        max_h,
                        scroll: &scroll,
                        heights: &heights,
                    },
                    cx,
                );
                let surface = v_flex().id("content").occlude().tab_group();
                ui::popover_surface(surface, cx).p_3().child(list)
            }
        };
        let leaving = reveal.is_closing().then(|| {
            let surface = reveal.apply(list(cx), Side::Top);
            deferred(Positioner::corner(Anchor::TopLeft, stood.get()).child(surface))
                .with_priority(POPUP_PRIORITY)
        });
        let popover = Popover::new("notifications-popover")
            .anchor(Anchor::TopRight)
            .offset(u(POPOVER_OFFSET).to_pixels(window.rem_size()))
            .trigger(trigger)
            .open(open)
            .on_open_change(move |open, window, cx| on_open_change(*open, window, cx))
            .content(move |_, _, cx| {
                let surface = list(cx).on_prepaint(move |bounds, window, _| {
                    stood.set(bounds.origin - reveal.offset(Side::Top, window));
                });
                reveal.apply(surface, Side::Top)
            });
        // The wrapper, not the trigger, occludes: the popover listens on its own wrapper.
        BarButton::slot(popover, dot)
            .debug_selector(|| "notification-bell".into())
            .flex_shrink_0()
            .children(leaving)
    }
}

/// How tall each request's row stood while it was still, so folding starts from that height
/// however far its title and email wrapped.
#[derive(Clone, Default)]
pub struct RowHeights(Rc<RefCell<HashMap<String, Pixels>>>);

/// The time of an entry, with the whole date in a tooltip.
fn time_meta(id: String, ago: SharedString, when: SharedString, cx: &App) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(ago)
        .tooltip(move |_, cx| ui::tooltip(when.clone(), cx))
}

fn decision_row(
    row: &DecisionRow,
    on_decide: Option<&DecideHandler>,
    heights: &RowHeights,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, hover, success) = (theme.muted_foreground, theme.list_hover, theme.success);
    let key = row.id.as_str().to_owned();
    let decide = |what: Decide| {
        let (f, id) = (on_decide.cloned(), row.id.clone());
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
            if let Some(f) = &f {
                f(id.clone(), what, window, cx);
            }
        }
    };
    let name = row.username.clone();
    // The actions line holds Vouch and Decline, then how the request was settled, at one height.
    let settled_line = div().h(u(BAR_CONTROL_H)).flex().items_center().gap_1p5();
    let entry = NotificationEntry::new(
        format!("decision-{key}"),
        Lead::Member(name.clone()),
        copy::asks_to_join(&name),
    )
    .meta(time_meta(
        format!("decision-when-{key}"),
        row.ago.clone(),
        row.when.clone(),
        cx,
    ))
    .detail(Some(Detail::Wrapped(row.label.clone())));
    let folding = matches!(row.stage, RowStage::Folding(_));
    let entry = match row.stage {
        RowStage::Settled(outcome) | RowStage::Folding(outcome) => {
            let vouched = outcome == Settled::VouchedIn;
            let (icon, icon_color, text, color) = if vouched {
                (
                    IconName::CircleCheck,
                    success,
                    copy::DECISION_VOUCHED,
                    tone_text(Tone::Success, cx),
                )
            } else {
                (IconName::UserX, muted, copy::DECISION_DECLINED, muted)
            };
            entry.action(
                settled_line
                    .text_sm()
                    .text_color(color)
                    .child(Icon::new(icon).size(u(ICON_SM)).text_color(icon_color))
                    .child(text)
                    .when(outcome == Settled::Declining && !folding, |d| {
                        d.child(
                            ui::Button::new(SharedString::from(format!("undo-{key}")))
                                .size(ui::Size::Word)
                                .icon(IconName::Undo2)
                                .label("Undo")
                                .on_click(decide(Decide::Undo)),
                        )
                    }),
            )
        }
        RowStage::Waiting { deciding } => entry
            .action(
                ui::Button::new(SharedString::from(format!("vouch-{}", row.username)))
                    .variant(ui::Variant::Primary)
                    .size(ui::Size::Word)
                    .when(deciding, |b| b.icon(IconName::LoaderCircle))
                    .label("Vouch")
                    .loading(deciding)
                    .disabled(deciding)
                    .tooltip(format!("Let {name} in as a Member"))
                    .on_click(decide(Decide::Vouch)),
            )
            .action(
                ui::Button::new(SharedString::from(format!("decline-{}", row.username)))
                    .variant(ui::Variant::Outline)
                    .size(ui::Size::Word)
                    .label("Decline")
                    .disabled(deciding)
                    .on_click(decide(Decide::Decline)),
            ),
    };
    let settled = row.stage.settled().is_some();
    let outer = div()
        .id(SharedString::from(format!("decision-{key}")))
        .debug_selector({
            let name = name.clone();
            move || format!("decision-{name}")
        })
        .relative()
        .flex_shrink_0()
        .overflow_hidden()
        .child(
            div()
                // GPUI keeps hover state by id: without one, entering the row never redraws it.
                .id(SharedString::from(format!("decision-hover-{key}")))
                .px_2()
                .py(u(ENTRY_PAD))
                .rounded_md()
                .when(!settled, |d| d.hover(move |s| s.bg(hover)))
                .child(entry),
        );
    if folding {
        let from = heights.0.borrow().get(&key).copied();
        outer
            .with_animation(
                SharedString::from(format!("fold-{key}")),
                Animation::new(FOLD_FOR),
                move |row, t| {
                    let row = row.opacity(1. - t);
                    match from {
                        Some(h) => row.h(h * (1. - t)),
                        None => row,
                    }
                },
            )
            .into_any_element()
    } else {
        let heights = heights.0.clone();
        outer
            .child(
                canvas(
                    move |bounds, _, _| {
                        heights.borrow_mut().insert(key, bounds.size.height);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }
}

/// The live connection entry: a Notification's shape without a time, an unseen dot or a dismiss,
/// since it is not an Issue and leaves by itself.
fn connection_entry(
    notice: &ConnectionNotice,
    on_action: &Callback<ConnectionAction>,
    cx: &App,
) -> AnyElement {
    let accent = tone_color(notice.tone, cx);
    let button = |action: ConnectionAction, primary: bool, enabled: bool| {
        let on_action = on_action.clone();
        ui::Button::new(SharedString::from(format!("connection-{}", action.id())))
            .size(ui::Size::Word)
            .label(action.label())
            .when(action != ConnectionAction::ChangeLobby, |b| {
                b.icon(IconName::RotateCw)
            })
            .variant(if primary {
                ui::Variant::Primary
            } else {
                ui::Variant::Outline
            })
            .disabled(!enabled)
            .loading(primary && !enabled)
            .on_click(move |_, window, cx| on_action(action, window, cx))
    };
    div()
        .id("connection-entry")
        .debug_selector(|| "connection-entry".into())
        .flex_shrink_0()
        .p(u(ENTRY_PAD))
        .child(
            NotificationEntry::new(
                "connection",
                Lead::Icon(
                    IconDisc::new(source_icon(Source::Connection), LEAD_SIZE).accent(accent),
                ),
                notice.title.clone(),
            )
            .detail(Some(Detail::Wrapped(notice.detail.clone())))
            .when_some(notice.action, |e, a| {
                e.action(button(a, true, notice.action_enabled))
            })
            .when_some(notice.secondary, |e, a| e.action(button(a, false, true))),
        )
        .test_support()
        .into_any_element()
}

pub struct Viewport<'a> {
    pub max_h: f32,
    pub scroll: &'a gpui_kit::ScrollHandle,
    pub heights: &'a RowHeights,
}

/// What the list shows, top to bottom.
pub struct Contents<'a> {
    pub connection: Option<(&'a ConnectionNotice, &'a Callback<ConnectionAction>)>,
    pub decisions: &'a [DecisionRow],
    pub rows: &'a [NotificationRow],
}

pub fn render_list(
    contents: Contents<'_>,
    on_dismiss: &Callback<Option<IssueKind>>,
    on_action: &Callback<Issue>,
    on_decide: Option<&DecideHandler>,
    viewport: Viewport<'_>,
    cx: &App,
) -> AnyElement {
    let Viewport {
        max_h,
        scroll,
        heights,
    } = viewport;
    let Contents {
        connection,
        decisions,
        rows,
    } = contents;
    let theme = cx.theme();
    let (fg, border, hover) = (theme.foreground, theme.border, theme.list_hover);
    let header = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .pb_2()
        .border_b_1()
        .border_color(border)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(fg)
                .child("Notifications"),
        )
        .when(!rows.is_empty(), |d| {
            let clear = on_dismiss.clone();
            d.child(
                ui::Button::new("notifications-clear")
                    .size(ui::Size::Word)
                    .label("Clear all")
                    .on_click(move |_, window, cx| clear(None, window, cx)),
            )
        });
    let waiting = (!decisions.is_empty()).then(|| {
        div()
            .id("decisions")
            .flex()
            .flex_col()
            .child(section_label("Requests to join", cx).pt_2().pb_1().px_2())
            .children(
                decisions
                    .iter()
                    .map(|d| decision_row(d, on_decide, heights, cx)),
            )
    });
    let live = connection.map(|(notice, on_action)| connection_entry(notice, on_action, cx));
    let body: AnyElement = if rows.is_empty() && (!decisions.is_empty() || live.is_some()) {
        div().into_any_element()
    } else if rows.is_empty() {
        div()
            .id("notifications-empty")
            .py_6()
            .child(
                Empty::new().p_0().gap_0().header(
                    EmptyHeader::new()
                        .media(
                            EmptyMedia::new()
                                .mb_0()
                                .child(IconDisc::new(IconName::BellOff, 36.)),
                        )
                        .title(
                            EmptyTitle::new()
                                .font_weight(gpui_kit::FontWeight::NORMAL)
                                .child("You're all caught up"),
                        )
                        .description(
                            EmptyDescription::new()
                                .text_xs()
                                .line_height(relative(1.4))
                                .child(
                                    "Requests to join and anything that goes wrong show up here.",
                                ),
                        ),
                ),
            )
            .into_any_element()
    } else {
        div()
            .id("notifications-list")
            .flex()
            .flex_col()
            .gap_1()
            .pt_2()
            .children(rows.iter().map(|row| {
                let accent = severity_color(row.issue.severity(), cx);
                let dismiss = on_dismiss.clone();
                let act = on_action.clone();
                let kind = row.issue.kind;
                let key = kind.key();
                let issue = row.issue.clone();
                let meta = div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .when(row.unseen, |d| {
                        d.child(div().size(u(6.)).flex_shrink_0().rounded_full().bg(accent))
                    })
                    .child(time_meta(
                        format!("notification-time-{key}"),
                        row.time.clone(),
                        local_date_time(row.issue.raised_at).into(),
                        cx,
                    ))
                    .child(
                        div()
                            .opacity(0.6)
                            .group_hover("notification-row", |s| s.opacity(1.))
                            .child(
                                ui::Button::new(SharedString::from(format!("dismiss-{key}")))
                                    .icon(IconName::X)
                                    .accessibility_label("Dismiss")
                                    .size(ui::Size::Corner)
                                    .tooltip("Dismiss")
                                    .on_click(move |_, window, cx| dismiss(Some(kind), window, cx)),
                            ),
                    );
                div()
                    .id(SharedString::from(format!("notification-{key}")))
                    .group("notification-row")
                    .p(u(ENTRY_PAD))
                    .rounded_md()
                    .hover(move |s| s.bg(hover))
                    .child(
                        NotificationEntry::new(
                            format!("notification-{key}"),
                            Lead::Icon(notification_icon(
                                row.issue.severity(),
                                row.issue.source(),
                                cx,
                            )),
                            row.title.clone(),
                        )
                        .meta(meta)
                        .detail(Some(Detail::Wrapped(row.message.clone())))
                        .when_some(row.action, |e, a| {
                            e.action(
                                action_button(format!("notification-action-{key}"), a)
                                    .on_click(move |_, window, cx| act(issue.clone(), window, cx)),
                            )
                        }),
                    )
                    .test_support()
            }))
            .into_any_element()
    };
    div()
        .id("notification-center")
        .debug_selector(|| "notification-center".into())
        .relative()
        .w(u(CENTER_WIDTH))
        .max_h(u(max_h))
        .flex()
        .flex_col()
        .child(header)
        .child(
            div()
                .id("notification-sections")
                .debug_selector(|| "notification-sections".into())
                .min_h_0()
                .flex()
                .flex_col()
                .pr_2()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .children(live)
                .children(waiting)
                .when(!decisions.is_empty() && !rows.is_empty(), |d| {
                    d.child(section_label("Recent", cx).pt_2().pb_1().px_2())
                })
                .child(body),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .child(Scrollbar::new(scroll).axis(ScrollbarAxis::Vertical)),
        )
        .test_support()
        .into_any_element()
}

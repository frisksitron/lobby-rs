use gpui_kit::{
    AnyElement, App, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, component::ActiveTheme as _, div,
    prelude::FluentBuilder as _,
};

use super::{Avatar, AvatarSize, IconDisc, truncated_text};
use crate::scale::u;

pub const LEAD_SIZE: f32 = AvatarSize::Row.px();
/// A title line, so the meta beside it centres on the first line however far the title wraps.
pub const TITLE_LINE: f32 = 20.;
const LEAD_GAP: f32 = 10.;
const DETAIL_GAP: f32 = 2.;
const ACTIONS_GAP: f32 = 8.;
/// The kit draws a toast's close button over its top right on hover (an extra-small 20 px button
/// 4 px in, past the toast's own 12 px padding), so the title keeps that corner free.
const CLOSE_ROOM: f32 = 4. + 20. - 12. + 4.;

pub enum Lead {
    Icon(IconDisc),
    Member(SharedString),
}

/// The muted text under the title.
pub enum Detail {
    /// In full, over as many lines as it needs.
    Wrapped(SharedString),
    /// One line that gives way at the edge, with the whole text in a tooltip.
    Line(SharedString),
}

#[derive(IntoElement)]
pub struct NotificationEntry {
    id: SharedString,
    lead: Lead,
    title: SharedString,
    meta: Option<AnyElement>,
    close_room: bool,
    detail: Option<Detail>,
    actions: Vec<AnyElement>,
}

impl NotificationEntry {
    pub fn new(id: impl Into<SharedString>, lead: Lead, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            lead,
            title: title.into(),
            meta: None,
            close_room: false,
            detail: None,
            actions: Vec::new(),
        }
    }

    /// What sits at the title's top right: the time and the dismiss X of a Center entry.
    pub fn meta(mut self, meta: impl IntoElement) -> Self {
        self.meta = Some(meta.into_any_element());
        self
    }

    /// Keeps the title off the corner where a toast's close button appears.
    pub fn beside_close_button(mut self) -> Self {
        self.close_room = true;
        self
    }

    pub fn detail(mut self, detail: Option<Detail>) -> Self {
        self.detail = detail;
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl RenderOnce for NotificationEntry {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let lead = match self.lead {
            Lead::Icon(disc) => disc.into_any_element(),
            Lead::Member(name) => Avatar::member(name).into_any_element(),
        };
        let title = div()
            .id(SharedString::from(format!("{}-title", self.id)))
            .flex_1()
            .min_w_0()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(fg)
            .when(self.close_room, |d| d.pr(u(CLOSE_ROOM)))
            .child(self.title);
        let detail = self.detail.map(|detail| {
            let detail_id = SharedString::from(format!("{}-detail", self.id));
            let style = |detail: Div| {
                detail
                    .mt(u(DETAIL_GAP))
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
            };
            match detail {
                Detail::Wrapped(text) => style(div()).id(detail_id).child(text).into_any_element(),
                Detail::Line(text) => style(truncated_text(detail_id, text)).into_any_element(),
            }
        });
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_start()
            .gap(u(LEAD_GAP))
            .child(div().flex_shrink_0().child(lead))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_2()
                            .child(title)
                            .children(self.meta.map(|meta| {
                                div()
                                    .h(u(TITLE_LINE))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .child(meta)
                            })),
                    )
                    .children(detail)
                    .when(!self.actions.is_empty(), |d| {
                        d.child(
                            div()
                                .mt(u(ACTIONS_GAP))
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1p5()
                                .children(self.actions),
                        )
                    }),
            )
    }
}

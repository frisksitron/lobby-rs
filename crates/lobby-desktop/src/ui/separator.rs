//! A hairline between two groups, across or down, with an optional label sitting on it.

use gpui_kit::{
    App, Axis, IntoElement, ParentElement as _, Refineable as _, RenderOnce, SharedString,
    StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _, px,
};

use crate::kit::ActiveTheme as _;

#[derive(IntoElement)]
pub struct Separator {
    axis: Axis,
    label: Option<SharedString>,
    style: StyleRefinement,
}

impl Separator {
    /// A line running across, between rows.
    pub fn horizontal() -> Self {
        Self {
            axis: Axis::Horizontal,
            label: None,
            style: StyleRefinement::default(),
        }
    }

    /// A line running down, between items of a row. It takes the row's height.
    pub fn vertical() -> Self {
        Self {
            axis: Axis::Vertical,
            ..Self::horizontal()
        }
    }

    /// Words on the line, such as a day.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Styled for Separator {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Separator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let line = div().absolute().bg(theme.border);
        let line = match self.axis {
            Axis::Horizontal => line.h(px(1.)).w_full(), // px: a hairline
            Axis::Vertical => line.w(px(1.)).h_full(),   // px: a hairline
        };
        let mut bar = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .when(self.axis == Axis::Vertical, |bar| bar.h_full());
        bar.style().refine(&self.style);
        bar.child(line).when_some(self.label, |bar, label| {
            bar.child(
                div()
                    .px_2()
                    .py_1()
                    .mx_auto()
                    .text_xs()
                    .bg(theme.tokens.background)
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
        })
    }
}

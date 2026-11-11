//! What a place shows when it has nothing to show: a header of media, title and description,
//! and below it what the person can do. Each part takes `Styled` for its call site's measure.
//!
//! ```ignore
//! Empty::new()
//!     .header(
//!         EmptyHeader::new()
//!             .media(EmptyMedia::new().child(icon))
//!             .title(EmptyTitle::new().child("Nothing here yet"))
//!             .description(EmptyDescription::new().child("What arrives shows up here.")),
//!     )
//!     .content(EmptyContent::new().child(button))
//! ```

use gpui_kit::{
    AnyElement, App, Div, IntoElement, ParentElement, RenderOnce, StyleRefinement, Styled, Window,
    base::StyledExt as _, div, prelude::FluentBuilder as _, relative, rems,
};

use crate::kit::ActiveTheme as _;

const MAX_W: f32 = 24.;

fn column() -> Div {
    div().flex().flex_col().w_full().min_w_0().items_center()
}

#[derive(Default, IntoElement)]
pub struct Empty {
    style: StyleRefinement,
    header: Option<EmptyHeader>,
    content: Option<EmptyContent>,
}

impl Empty {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn header(mut self, header: EmptyHeader) -> Self {
        self.header = Some(header);
        self
    }

    pub fn content(mut self, content: EmptyContent) -> Self {
        self.content = Some(content);
        self
    }
}

impl Styled for Empty {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Empty {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        column()
            .flex_1()
            .justify_center()
            .gap_4()
            .p_6()
            .rounded(theme.radius_tokens().xl)
            .text_center()
            .text_color(theme.foreground)
            .refine_style(&self.style)
            .when_some(self.header, |this, header| this.child(header))
            .when_some(self.content, |this, content| this.child(content))
    }
}

/// The media, title and description, in that order.
#[derive(Default, IntoElement)]
pub struct EmptyHeader {
    style: StyleRefinement,
    media: Option<EmptyMedia>,
    title: Option<EmptyTitle>,
    description: Option<EmptyDescription>,
}

impl EmptyHeader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn media(mut self, media: EmptyMedia) -> Self {
        self.media = Some(media);
        self
    }

    pub fn title(mut self, title: EmptyTitle) -> Self {
        self.title = Some(title);
        self
    }

    pub fn description(mut self, description: EmptyDescription) -> Self {
        self.description = Some(description);
        self
    }
}

impl Styled for EmptyHeader {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for EmptyHeader {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        column()
            .max_w(rems(MAX_W))
            .gap_2()
            .refine_style(&self.style)
            .when_some(self.media, |this, media| this.child(media))
            .when_some(self.title, |this, title| this.child(title))
            .when_some(self.description, |this, description| {
                this.child(description)
            })
    }
}

/// An icon, a picture or a mark above the words.
#[derive(Default, IntoElement)]
pub struct EmptyMedia {
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl EmptyMedia {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for EmptyMedia {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for EmptyMedia {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for EmptyMedia {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // A column keeps the width of a nested row, such as a group of avatars.
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .mb_2()
            .refine_style(&self.style)
            .children(self.children)
    }
}

#[derive(Default, IntoElement)]
pub struct EmptyTitle {
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl EmptyTitle {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for EmptyTitle {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for EmptyTitle {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for EmptyTitle {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .max_w_full()
            .min_w_0()
            .text_sm()
            .font_medium()
            .whitespace_normal()
            .refine_style(&self.style)
            .children(self.children)
    }
}

/// The words that say why it is empty, and what fills it.
#[derive(Default, IntoElement)]
pub struct EmptyDescription {
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl EmptyDescription {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for EmptyDescription {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for EmptyDescription {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for EmptyDescription {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .w_full()
            .min_w_0()
            .text_sm()
            .line_height(relative(1.625))
            .text_color(cx.theme().muted_foreground)
            .whitespace_normal()
            .refine_style(&self.style)
            .children(self.children)
    }
}

/// What the person can do about it, below the header.
#[derive(Default, IntoElement)]
pub struct EmptyContent {
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl EmptyContent {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for EmptyContent {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for EmptyContent {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for EmptyContent {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        column()
            .max_w(rems(MAX_W))
            .gap_2p5()
            .text_sm()
            .refine_style(&self.style)
            .children(self.children)
    }
}

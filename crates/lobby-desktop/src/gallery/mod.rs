//! The component gallery: every component of the app in its states, drawn from plain models. No
//! Lobby, no Tokio runtime and no Shell: the window takes the app's theme and nothing else. A story
//! is a row of [`STORIES`].

#[cfg(not(debug_assertions))]
compile_error!(
    "the `gallery` feature is for dev builds only: the component gallery is not part of the app. \
     Build releases without it."
);

mod controls;
mod fixtures;
mod stories;

use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Axis, Bounds, ClickEvent, Context, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TitlebarOptions, Window, WindowBounds,
    WindowOptions, base::Toolbar, div, px, size,
};
use lobby_client::Settings;

use crate::{
    appearance,
    kit::ActiveTheme as _,
    ui,
    window_bounds::{DEFAULT_SIZE, MIN_SIZE},
};

pub use fixtures::Held;
pub use stories::STORIES;

pub struct Story {
    pub name: &'static str,
    pub description: &'static str,
    pub usage: &'static str,
    pub render: fn(&mut Gallery, &mut Window, &mut Context<Gallery>) -> Vec<Section>,
}

pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub body: AnyElement,
}

impl Section {
    pub fn new(id: &'static str, title: &'static str, body: impl IntoElement) -> Self {
        Self {
            id,
            title,
            body: body.into_any_element(),
        }
    }
}

pub fn find(name: &str) -> Option<usize> {
    STORIES
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(name.trim()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    Fill,
    Default,
    Minimum,
}

impl Frame {
    const ALL: [Frame; 3] = [Frame::Fill, Frame::Default, Frame::Minimum];

    fn size(self) -> Option<(f32, f32)> {
        match self {
            Frame::Fill => None,
            Frame::Default => Some((
                f32::from(DEFAULT_SIZE.width),
                f32::from(DEFAULT_SIZE.height),
            )),
            Frame::Minimum => Some((f32::from(MIN_SIZE.width), f32::from(MIN_SIZE.height))),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Frame::Fill => "Fill",
            Frame::Default => "680 × 480",
            Frame::Minimum => "480 × 400",
        }
    }
}

const SIDEBAR_W: f32 = 210.;

pub struct Gallery {
    active: usize,
    frame: Frame,
    pub held: Held,
}

impl Gallery {
    pub fn open(
        initial: Option<usize>,
        cx: &mut App,
    ) -> gpui_kit::Result<(AnyWindowHandle, Entity<Gallery>)> {
        crate::fonts::install(cx);
        appearance::apply(&Settings::default(), cx);
        let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Lobby components".into()),
                ..TitlebarOptions::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(720.), px(520.))),
            ..WindowOptions::default()
        };
        gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| Gallery::new(initial.unwrap_or(0), window, cx))
        })
    }

    pub fn new(active: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            active,
            frame: Frame::Fill,
            held: Held::new(window, cx),
        }
    }

    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.active = index;
        cx.notify();
    }

    pub fn set_frame(&mut self, frame: Frame, cx: &mut Context<Self>) {
        self.frame = frame;
        cx.notify();
    }

    pub fn active(&self) -> &'static Story {
        &STORIES[self.active]
    }

    /// The story list: a button per story, and the arrow keys move along it as along a toolbar.
    fn stories(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fill, edge, muted) = (theme.sidebar, theme.sidebar_border, theme.muted_foreground);
        div()
            .w(px(SIDEBAR_W))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .py_2()
            .bg(fill)
            .border_r_1()
            .border_color(edge)
            .child(
                div()
                    .px_4()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Lobby"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Components and their states"),
                    ),
            )
            .child(
                Toolbar::new("gallery-stories")
                    .axis(Axis::Vertical)
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_2()
                    .overflow_y_scroll()
                    .children(STORIES.iter().enumerate().map(|(ix, story)| {
                        ui::Button::new(SharedString::from(format!("story-{}", story.name)))
                            .size(ui::Size::Bar)
                            .w_full()
                            .accessibility_label(story.name)
                            .selected(ix == self.active)
                            .child(div().w_full().text_left().child(story.name))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.select(ix, cx);
                            }))
                    })),
            )
    }

    fn header(&self, story: &Story, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let frames = div()
            .flex()
            .items_center()
            .gap_1()
            .children(Frame::ALL.map(|frame| {
                ui::Button::new(SharedString::from(format!("frame-{}", frame.label())))
                    .size(ui::Size::Inline)
                    .label(frame.label())
                    .selected(self.frame == frame)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_frame(frame, cx)))
            }));
        div()
            .id("gallery-header")
            .flex_shrink_0()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .flex()
            .items_start()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(story.name),
                    )
                    .child(div().text_sm().text_color(muted).child(story.description)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(frames),
            )
    }
}

fn usage(code: &'static str, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("gallery-usage")
        .w_full()
        .p_3()
        .rounded_md()
        .bg(theme.muted)
        .text_color(theme.foreground)
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .overflow_x_scroll()
        .child(code)
}

fn section(s: Section, cx: &App) -> AnyElement {
    let id = s.id;
    div()
        .id(SharedString::from(format!("gallery-section-{id}")))
        .debug_selector(move || format!("gallery-section-{id}"))
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(s.title.to_uppercase()),
        )
        .child(s.body)
        .into_any_element()
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let story = self.active();
        stories::start_page();
        let sections = (story.render)(self, window, cx);
        let slug = story.name.to_lowercase().replace(' ', "-").replace(':', "");
        let page = div()
            .id(SharedString::from(format!("gallery-page-{slug}")))
            .debug_selector(move || format!("gallery-page-{slug}"))
            .size_full()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_6()
            .child(usage(story.usage, cx))
            .children(sections.into_iter().map(|s| section(s, cx)));
        let theme = cx.theme();
        let (background, border, muted) = (theme.background, theme.border, theme.muted);
        let area = match self.frame.size() {
            None => div().flex_1().min_h_0().child(page),
            Some((w, h)) => div().flex_1().min_h_0().p_4().bg(muted.opacity(0.4)).child(
                div()
                    .id("gallery-frame")
                    .debug_selector(|| "gallery-frame".into())
                    .w(px(w))
                    .h(px(h))
                    .flex_shrink_0()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .overflow_hidden()
                    .child(page),
            ),
        };
        div()
            .size_full()
            .flex()
            .bg(background)
            .text_color(theme.foreground)
            .child(self.stories(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.header(story, cx))
                    .child(area),
            )
    }
}

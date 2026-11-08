//! The Screen Share source picker: the kit dialog's body, listing screens as tiles and other apps'
//! windows as rows. Every source that can be picked is a tab stop and the first takes focus when
//! the list arrives, so Tab, Enter and Esc work without the mouse. A window that blocks capture is
//! greyed, says so, and cannot be picked. The model is plain data, so tests check it without
//! rendering.
//!
//! Each source reserves its picture box from the first frame, with its icon in it; a thumbnail
//! that arrives later fills the box, so nothing moves. A source that has none keeps its icon.
//! Every thumbnail is a `RenderImage`, a GPU atlas tile that nothing frees by itself, so the view
//! drops them when it replaces one, lists new sources and is released.

use std::{collections::HashMap, rc::Rc, sync::Arc};

use gpui_kit::{
    App, Context, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ObjectFit, ParentElement as _, Render, RenderImage, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, TestSupportExt as _, WeakEntity, Window,
    assets::IconName,
    component::{ActiveTheme as _, Icon, spinner::Spinner},
    div, img, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _,
};
use image::{Frame as ImageFrame, RgbaImage};
use lobby_client::{ScreenSource, SourceId, SourceKind, Thumbnail, copy};
use lobby_protocol::{Quality, encoder::QUALITIES};

use super::{ICON_MD, IconDisc, Segmented, live_red, picture_backdrop, section_label};
use crate::scale::{design_viewport, u};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerModel {
    Loading,
    Sources(Vec<ScreenSource>),
    Failed(String),
}

impl PickerModel {
    pub fn of(&self, kind: SourceKind) -> Vec<&ScreenSource> {
        match self {
            PickerModel::Sources(s) => s.iter().filter(|s| s.kind() == kind).collect(),
            _ => Vec::new(),
        }
    }
}

pub type PickHandler = Rc<dyn Fn(&ScreenSource, Quality, &mut Window, &mut App)>;

type Pick = Rc<dyn Fn(&ScreenSource, &mut Window, &mut App)>;

/// The picker's body inside the kit dialog. An entity, so the list re-renders by itself when the
/// sources arrive.
pub struct PickerView {
    pub model: PickerModel,
    quality: Quality,
    quality_focus: FocusHandle,
    on_pick: PickHandler,
    focus: Vec<FocusHandle>,
    thumbs: HashMap<SourceId, Arc<RenderImage>>,
}

impl PickerView {
    pub fn new(on_pick: PickHandler, quality: Quality, cx: &mut Context<Self>) -> Self {
        cx.on_release(|view: &mut PickerView, cx| view.forget_thumbnails(cx))
            .detach();
        Self {
            model: PickerModel::Loading,
            quality,
            quality_focus: cx.focus_handle().tab_stop(true),
            on_pick,
            focus: Vec::new(),
            thumbs: HashMap::new(),
        }
    }

    fn choose(&mut self, quality: Quality, cx: &mut Context<Self>) {
        self.quality = quality;
        cx.notify();
    }

    pub fn set_model(&mut self, model: PickerModel, cx: &mut Context<Self>) {
        let count = match &model {
            PickerModel::Sources(s) => s.iter().filter(|s| !s.blocked).count(),
            _ => 0,
        };
        self.focus = (0..count)
            .map(|_| cx.focus_handle().tab_stop(true))
            .collect();
        self.forget_thumbnails(cx);
        self.model = model;
        cx.notify();
    }

    /// Fills the listed source's picture box. A thumbnail of a source the list no longer holds is
    /// ignored.
    pub fn set_thumbnail(&mut self, id: &SourceId, thumbnail: Thumbnail, cx: &mut Context<Self>) {
        let listed =
            matches!(&self.model, PickerModel::Sources(s) if s.iter().any(|s| s.id == *id));
        if !listed {
            return;
        }
        let Thumbnail {
            width,
            height,
            bgra,
        } = thumbnail;
        let Some(buffer) = RgbaImage::from_raw(width, height, bgra) else {
            tracing::warn!(width, height, "a thumbnail of the wrong size");
            return;
        };
        let image = Arc::new(RenderImage::new([ImageFrame::new(buffer)]));
        if let Some(old) = self.thumbs.insert(id.clone(), image) {
            cx.drop_image(old, None);
        }
        cx.notify();
    }

    pub fn has_thumbnail(&self, id: &SourceId) -> bool {
        self.thumbs.contains_key(id)
    }

    fn forget_thumbnails(&mut self, cx: &mut App) {
        for (_, image) in self.thumbs.drain() {
            cx.drop_image(image, None);
        }
    }

    pub fn focus_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(first) = self.focus.first() {
            first.focus(window, cx);
        }
    }
}

fn picks(e: &KeyDownEvent) -> bool {
    matches!(e.keystroke.key.as_str(), "enter" | "space")
}

fn icon_for(kind: SourceKind) -> IconName {
    match kind {
        SourceKind::Screen => IconName::Monitor,
        SourceKind::Window => IconName::AppWindow,
        SourceKind::TestPattern => IconName::FlaskConical,
    }
}

fn section(label: &'static str, cx: &App) -> impl IntoElement {
    section_label(label, cx).pt_3().pb_1p5()
}

/// A screen tile's picture box height, and a window row's box, in design pixels.
const TILE_PREVIEW_H: f32 = 96.;
const ROW_PREVIEW: (f32, f32) = (56., 36.);

/// The box a source's picture is reserved: the caller sizes it, the icon holds the place and the
/// thumbnail covers it when it arrives. `hook` names the picture, which exists only once it does.
fn preview(
    hook: ElementId,
    image: Option<&Arc<RenderImage>>,
    kind: SourceKind,
    icon: f32,
    cx: &App,
) -> gpui_kit::Div {
    let theme = cx.theme();
    let glyph = theme.muted_foreground;
    div()
        .rounded_md()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(picture_backdrop(theme))
        .map(|d| match image {
            Some(image) => d.child(
                div()
                    .id(hook)
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(image.clone())
                            .h_full()
                            .max_w_full()
                            .object_fit(ObjectFit::Contain),
                    )
                    .test_support(),
            ),
            None => d.child(Icon::new(icon_for(kind)).size(u(icon)).text_color(glyph)),
        })
}

fn screen_tile(
    id: ElementId,
    hook: ElementId,
    source: &ScreenSource,
    image: Option<&Arc<RenderImage>>,
    focus: Option<&FocusHandle>,
    on_pick: &Pick,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (border, hover, muted, fg) = (
        theme.border,
        theme.list_hover,
        theme.muted_foreground,
        theme.foreground,
    );
    let accent = live_red(theme);
    let pick = on_pick.clone();
    let chosen = source.clone();
    let (key_pick, key_chosen) = (on_pick.clone(), source.clone());
    div()
        .id(id)
        .when_some(focus, |d, f| d.track_focus(f))
        .on_key_down(move |e, window, cx| {
            if picks(e) {
                key_pick(&key_chosen, window, cx);
            }
        })
        .focus_visible(move |s| s.border_color(accent))
        .flex_1()
        .min_w(u(180.))
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .hover(move |s| s.bg(hover).border_color(accent))
        .on_click(move |_, window, cx| pick(&chosen, window, cx))
        .child(preview(hook, image, source.kind(), 36., cx).h(u(TILE_PREVIEW_H)))
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(fg)
                .truncate()
                .child(source.name.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .truncate()
                .child(source.detail.clone()),
        )
        .test_support()
}

fn window_row(
    id: ElementId,
    hook: ElementId,
    source: &ScreenSource,
    image: Option<&Arc<RenderImage>>,
    focus: Option<&FocusHandle>,
    on_pick: &Pick,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (hover, muted, fg) = (theme.list_hover, theme.muted_foreground, theme.foreground);
    let pick = on_pick.clone();
    let chosen = source.clone();
    let (key_pick, key_chosen) = (on_pick.clone(), source.clone());
    // A window that blocks capture is listed so its absence puzzles nobody, and says why it
    // cannot be picked in place of the app's name.
    let detail = if source.blocked {
        copy::BLOCKS_SHARING.to_owned()
    } else {
        source.detail.clone()
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded_md()
        .map(|d| {
            if source.blocked {
                return d.opacity(0.5);
            }
            d.when_some(focus, |d, f| d.track_focus(f))
                .on_key_down(move |e, window, cx| {
                    if picks(e) {
                        key_pick(&key_chosen, window, cx);
                    }
                })
                .focus_visible(move |s| s.bg(hover))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |_, window, cx| pick(&chosen, window, cx))
        })
        .child(
            preview(hook, image, source.kind(), ICON_MD, cx)
                .w(u(ROW_PREVIEW.0))
                .h(u(ROW_PREVIEW.1))
                .flex_shrink_0(),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .text_color(fg)
                        .truncate()
                        .child(source.name.clone()),
                )
                .when(!detail.is_empty(), |d| {
                    d.child(div().text_xs().text_color(muted).truncate().child(detail))
                }),
        )
        .test_support()
}

/// What the dialog's margins, the picker's header and the quality row take from the window's
/// height, besides the list.
const LIST_CHROME: f32 = 240.;

const QUALITY_CHOICE_W: f32 = 180.;

fn quality_row(
    quality: Quality,
    focus: &FocusHandle,
    this: WeakEntity<PickerView>,
    cx: &App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let mut choice = Segmented::new("share-quality").focus(focus);
    for q in QUALITIES.map(|row| row.quality) {
        let this = this.clone();
        choice = choice.word(copy::quality_label(q), q == quality, move |_, cx| {
            let _ = this.update(cx, |view, cx| view.choose(q, cx));
        });
    }
    div()
        .id("share-quality-row")
        .flex()
        .items_center()
        .gap_3()
        .px_5()
        .pb_2()
        .child(div().w(u(QUALITY_CHOICE_W)).flex_shrink_0().child(choice))
        .child(
            div()
                .id("share-quality-effect")
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .text_xs()
                .text_color(muted)
                .children(copy::quality_effect(quality).map(|line| div().truncate().child(line)))
                .test_support(),
        )
}

impl Render for PickerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The dialog fits the window at every size: the list takes what the header and margins
        // leave.
        let list_max = (design_viewport(window).1 - LIST_CHROME).clamp(120., 440.);
        let quality = self.quality;
        let handler = self.on_pick.clone();
        let pick: Pick = Rc::new(move |source, window, cx| handler(source, quality, window, cx));
        let quality_row = quality_row(quality, &self.quality_focus, cx.weak_entity(), cx);
        let theme = cx.theme();
        let (muted, fg, danger, popover) = (
            theme.muted_foreground,
            theme.foreground,
            theme.danger,
            theme.popover,
        );
        let live = live_red(theme);
        let mut body: Vec<gpui_kit::AnyElement> = Vec::new();
        match &self.model {
            PickerModel::Loading => body.push(
                div()
                    .py_10()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .text_sm()
                    .text_color(muted)
                    .child(Spinner::new())
                    .child("Looking for screens and windows…")
                    .into_any_element(),
            ),
            PickerModel::Failed(e) => body.push(
                div()
                    .id("picker-error")
                    .py_6()
                    .text_sm()
                    .text_color(danger)
                    .child(e.clone())
                    .into_any_element(),
            ),
            PickerModel::Sources(_) => {
                let screens = self.model.of(SourceKind::Screen);
                let windows = self.model.of(SourceKind::Window);
                let tests = self.model.of(SourceKind::TestPattern);
                // Only what can be picked is a tab stop.
                let mut stops = self.focus.iter();
                let mut stop = |s: &ScreenSource| if s.blocked { None } else { stops.next() };
                if !screens.is_empty() {
                    body.push(section("Screens", cx).into_any_element());
                    let tiles: Vec<_> = screens
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            screen_tile(
                                ("picker-screen", i).into(),
                                ("picker-screen-thumbnail", i).into(),
                                s,
                                self.thumbs.get(&s.id),
                                stop(s),
                                &pick,
                                cx,
                            )
                        })
                        .collect();
                    body.push(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_3()
                            .children(tiles)
                            .into_any_element(),
                    );
                }
                if !windows.is_empty() {
                    body.push(section("Windows", cx).into_any_element());
                    for (i, s) in windows.iter().enumerate() {
                        body.push(
                            window_row(
                                ("picker-window", i).into(),
                                ("picker-window-thumbnail", i).into(),
                                s,
                                self.thumbs.get(&s.id),
                                stop(s),
                                &pick,
                                cx,
                            )
                            .into_any_element(),
                        );
                    }
                }
                if !tests.is_empty() {
                    body.push(section("Test (LOBBY_FAKE_MEDIA)", cx).into_any_element());
                    for (i, s) in tests.iter().enumerate() {
                        body.push(
                            window_row(
                                ("picker-test", i).into(),
                                ("picker-test-thumbnail", i).into(),
                                s,
                                self.thumbs.get(&s.id),
                                stop(s),
                                &pick,
                                cx,
                            )
                            .into_any_element(),
                        );
                    }
                }
                if screens.is_empty() && windows.is_empty() && tests.is_empty() {
                    body.push(
                        div()
                            .py_6()
                            .text_sm()
                            .text_color(muted)
                            .child("Nothing to share was found.")
                            .into_any_element(),
                    );
                }
            }
        }
        div()
            .id("source-picker")
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_3()
                    .px_5()
                    .pb_2()
                    .child(IconDisc::new(IconName::ScreenShare, 36.).accent(live))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(fg)
                                    .child("Share your screen"),
                            )
                            .child(div().text_sm().text_color(muted).child(
                                "Pick a screen or a window. Everyone in Voice can watch it.",
                            )),
                    ),
            )
            .child(quality_row)
            .child(
                div()
                    .relative()
                    .child(
                        div()
                            .id("source-picker-list")
                            .max_h(u(list_max))
                            .overflow_y_scroll()
                            .px_5()
                            .pb_4()
                            .children(body)
                            .test_support(),
                    )
                    .child(div().absolute().bottom_0().left_0().right_0().h(u(28.)).bg(
                        linear_gradient(
                            180.,
                            linear_color_stop(popover.opacity(0.), 0.),
                            linear_color_stop(popover, 1.),
                        ),
                    )),
            )
            .test_support()
    }
}

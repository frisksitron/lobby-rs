//! The Screen Share source picker: the kit dialog's body. The header holds the title, the quality
//! select and Close (the dialog's own is off, so the three share one line), under it the screens,
//! other apps' windows and the test patterns are each a grid of 16:9 tiles: the picture over one
//! line of name.
//! Every source that can be picked is a tab stop and the first takes focus when the list arrives,
//! so Tab, Enter and Esc work without the mouse. A window that blocks capture stays in the list,
//! dimmed, says so on its line and cannot be picked. The model is plain data, so tests check it
//! without rendering.
//!
//! Each source reserves its picture box from the first frame, with its icon in it; a thumbnail
//! that arrives later fills the box, so nothing moves. A source that has none keeps its icon.
//! Every thumbnail is a `RenderImage`, a GPU atlas tile that nothing frees by itself, so the view
//! drops them when it replaces one, lists new sources and is released.

use std::{collections::HashMap, rc::Rc, sync::Arc};

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit, ParentElement as _, Render,
    RenderImage, ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, TestSupportExt as _, Window,
    assets::IconName,
    base::actions::Cancel,
    component::{
        ActiveTheme as _, Icon,
        button::ButtonVariants as _,
        scroll::{Scrollbar, ScrollbarAxis},
        searchable_list::{SearchableListItem, SearchableVec},
        select::SelectState,
        spinner::Spinner,
        tooltip::Tooltip,
    },
    div, img,
    prelude::FluentBuilder as _,
};
use image::{Frame as ImageFrame, RgbaImage};
use lobby_client::{ScreenSource, SourceId, SourceKind, Thumbnail, copy};
use lobby_protocol::{Quality, encoder::QUALITIES};

use super::{page_icon_button, page_select, picture_backdrop, section_label, truncated_text};
use crate::scale::{design_viewport, u};

/// The dialog's width in design pixels; the window's margins can make it narrower.
pub const PICKER_W: f32 = 620.;

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

/// A quality's line in the select: its name, then what it needs, so the choice carries its cost.
#[derive(Clone)]
struct QualityLine(Quality);

type Qualities = SearchableVec<QualityLine>;

impl QualityLine {
    fn id(&self) -> &'static str {
        match self.0 {
            Quality::UpTo1080 => "share-quality-1080p",
            Quality::UpTo1440 => "share-quality-1440p",
            Quality::UpTo2160 => "share-quality-4k",
        }
    }
}

impl SearchableListItem for QualityLine {
    type Value = Quality;

    fn title(&self) -> SharedString {
        copy::quality_label(self.0).into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .id(self.id())
            .flex()
            .items_center()
            .gap_2()
            .whitespace_nowrap()
            .child(copy::quality_label(self.0))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(copy::quality_need(self.0)),
            )
            .test_support()
    }

    fn value(&self) -> &Quality {
        &self.0
    }
}

/// The picker's body inside the kit dialog. An entity, so the list re-renders by itself when the
/// sources arrive.
pub struct PickerView {
    pub model: PickerModel,
    quality: Entity<SelectState<Qualities>>,
    on_pick: PickHandler,
    focus: Vec<FocusHandle>,
    thumbs: HashMap<SourceId, Arc<RenderImage>>,
    scroll: ScrollHandle,
}

impl PickerView {
    pub fn new(
        on_pick: PickHandler,
        quality: Quality,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|view: &mut PickerView, cx| view.forget_thumbnails(cx))
            .detach();
        let lines = QUALITIES.map(|row| QualityLine(row.quality)).to_vec();
        let select = cx.new(|cx| {
            let mut select = SelectState::new(Qualities::new(lines), None, window, cx);
            select.set_selected_value(&quality, window, cx);
            select
        });
        Self {
            model: PickerModel::Loading,
            quality: select,
            on_pick,
            focus: Vec::new(),
            thumbs: HashMap::new(),
            scroll: ScrollHandle::new(),
        }
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

/// What a kind of source names its elements by: the pickable item, its picture and its name.
struct Ids {
    item: &'static str,
    thumbnail: &'static str,
    name: &'static str,
}

const SCREEN: Ids = Ids {
    item: "picker-screen",
    thumbnail: "picker-screen-thumbnail",
    name: "picker-screen-name",
};
const WINDOW: Ids = Ids {
    item: "picker-window",
    thumbnail: "picker-window-thumbnail",
    name: "picker-window-name",
};
const TEST: Ids = Ids {
    item: "picker-test",
    thumbnail: "picker-test-thumbnail",
    name: "picker-test-name",
};

/// A source as the list draws it: its place among its kind, its picture so far and its focus.
struct Entry<'a> {
    n: usize,
    source: &'a ScreenSource,
    image: Option<&'a Arc<RenderImage>>,
    focus: Option<&'a FocusHandle>,
}

/// The entries of one kind, taking the tab stops in turn. Only what can be picked has one.
fn entries<'a>(
    sources: &[&'a ScreenSource],
    thumbs: &'a HashMap<SourceId, Arc<RenderImage>>,
    stops: &mut std::slice::Iter<'a, FocusHandle>,
) -> Vec<Entry<'a>> {
    sources
        .iter()
        .enumerate()
        .map(|(n, &source)| Entry {
            n,
            source,
            image: thumbs.get(&source.id),
            focus: if source.blocked { None } else { stops.next() },
        })
        .collect()
}

/// A picture's corners; the tile's own are this plus its inset, so the two curves run even.
const THUMB_RADIUS: f32 = 6.;
const TILE_RADIUS: f32 = THUMB_RADIUS + 8.;
/// A tile's border is 1 px and clear until it has the focus, so the focus moves nothing.
const TILE_PAD: f32 = 7.;
/// A pickable tile's picture and name sit this far in from its edge, and so do the section labels,
/// so everything in the list has one left edge.
const ITEM_INSET: f32 = 8.;

const TILE_GAP: f32 = 8.;
/// The picture and the name in a tile.
const TILE_INNER_GAP: f32 = 6.;
const PREVIEW_ICON: f32 = 32.;
/// A name's line. Fixed, so a blocked source's line, which clips to it, is as tall as the rest.
const NAME_LINE_H: f32 = 20.;
/// A label over its tiles, which stand a further 4 to 8 px in from their wash.
const LABEL_GAP: f32 = 4.;
const SECTION_GAP: f32 = 12.;
/// The narrowest a tile goes before the grid drops a column.
const MIN_TILE_W: f32 = 176.;
/// A screen's picture is a whole desktop, which needs more room to read than a window's does.
const SCREEN_MAX_COLUMNS: usize = 2;

/// The box a source's picture is reserved: the caller sizes it, the icon holds the place and the
/// thumbnail covers it when it arrives. `hook` names the picture, which exists only once it does.
fn preview(hook: ElementId, image: Option<&Arc<RenderImage>>, kind: SourceKind, cx: &App) -> Div {
    let theme = cx.theme();
    let glyph = theme.muted_foreground;
    div()
        .rounded(u(THUMB_RADIUS))
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
            None => d.child(
                Icon::new(icon_for(kind))
                    .size(u(PREVIEW_ICON))
                    .text_color(glyph),
            ),
        })
}

/// The one line a source is named by, cut with an ellipsis (and the whole name in a tooltip) when
/// it does not fit.
fn name_line(ids: &Ids, e: &Entry, cx: &App) -> Div {
    truncated_text((ids.name, e.n), e.source.name.clone().into())
        .text_sm()
        .line_height(u(NAME_LINE_H))
        .text_color(cx.theme().foreground)
}

/// A tile's one line. A source that blocks capture says so after its name, in the muted colour.
/// When the two do not fit, the name wraps out of a line with room for one that keeps its last
/// row, so the line reads only "can't be shared" and the tooltip holds the name.
fn caption(ids: &Ids, e: &Entry, cx: &App) -> AnyElement {
    if !e.source.blocked {
        return name_line(ids, e, cx).into_any_element();
    }
    let theme = cx.theme();
    let whole: SharedString = e.source.name.clone().into();
    let shown = whole.clone();
    div()
        .id((ids.name, e.n))
        .h(u(NAME_LINE_H))
        .overflow_hidden()
        .flex()
        .flex_wrap()
        .content_end()
        .gap_x(u(TILE_INNER_GAP))
        .text_sm()
        .line_height(u(NAME_LINE_H))
        .whitespace_nowrap()
        .child(
            div()
                .flex_shrink_0()
                .text_color(theme.foreground)
                .child(shown),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(copy::CANT_SHARE),
        )
        .tooltip(move |window, cx| Tooltip::new(whole.clone()).build(window, cx))
        .into_any_element()
}

/// What a source in the list does: picked by a click, Enter or Space, washed on hover and press,
/// ringed on keyboard focus. One that blocks capture is dimmed and does nothing.
fn item(ids: &Ids, e: &Entry, on_pick: &Pick, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, ring, press) = (theme.list_hover, theme.ring, theme.foreground.opacity(0.09));
    let base = div()
        .id((ids.item, e.n))
        .flex()
        .border_1()
        .border_color(theme.transparent)
        .rounded(u(TILE_RADIUS))
        .p(u(TILE_PAD));
    if e.source.blocked {
        return base.opacity(0.5);
    }
    let (pick, chosen) = (on_pick.clone(), e.source.clone());
    let (key_pick, key_chosen) = (on_pick.clone(), e.source.clone());
    base.when_some(e.focus, |d, f| d.track_focus(f))
        .on_key_down(move |ev, window, cx| {
            if picks(ev) {
                key_pick(&key_chosen, window, cx);
            }
        })
        .focus_visible(move |s| s.bg(hover).border_color(ring))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(press))
        .on_click(move |_, window, cx| pick(&chosen, window, cx))
}

/// A source: its picture over one line.
fn tile(ids: &Ids, e: &Entry, on_pick: &Pick, cx: &App) -> AnyElement {
    item(ids, e, on_pick, cx)
        .flex_1()
        .min_w_0()
        .flex_col()
        .gap(u(TILE_INNER_GAP))
        .child(
            preview((ids.thumbnail, e.n).into(), e.image, e.source.kind(), cx)
                .w_full()
                .aspect_ratio(16. / 9.),
        )
        .child(caption(ids, e, cx))
        .test_support()
        .into_any_element()
}

/// A kind's tiles in rows of `columns`; empty cells fill the last row, so every tile is as wide as
/// the others. A cell's border and padding count in its share of the row, so the empty ones have
/// the tile's.
fn grid(listed: &[Entry], ids: &Ids, columns: usize, pick: &Pick, cx: &App) -> Div {
    let mut tiles: Vec<AnyElement> = listed.iter().map(|e| tile(ids, e, pick, cx)).collect();
    let mut grid = div().flex().flex_col().gap(u(TILE_GAP));
    while !tiles.is_empty() {
        let mut row: Vec<AnyElement> = tiles.drain(..columns.min(tiles.len())).collect();
        while row.len() < columns {
            row.push(
                div()
                    .flex_1()
                    .min_w_0()
                    .border_1()
                    .p(u(TILE_PAD))
                    .into_any_element(),
            );
        }
        grid = grid.child(div().flex().gap(u(TILE_GAP)).children(row));
    }
    grid
}

/// A labelled group of the list.
fn section(label: &'static str, body: impl IntoElement, cx: &App) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(u(LABEL_GAP))
        .child(section_label(label, cx).px(u(ITEM_INSET)))
        .child(body)
        .into_any_element()
}

/// What the dialog keeps around its list besides the header: its own padding and the window's
/// margins, and the lane the list leaves for its scrollbar.
const DIALOG_MARGIN: f32 = 16.;
const DIALOG_PAD: f32 = 16.;
const SCROLL_LANE: f32 = 8.;

/// Tiles a row holds: as many as fit side by side in the dialog at the narrowest tile.
fn columns(viewport_w: f32) -> usize {
    let list_w = PICKER_W.min(viewport_w - DIALOG_MARGIN * 2.) - DIALOG_PAD * 2. - SCROLL_LANE;
    (((list_w + TILE_GAP) / (MIN_TILE_W + TILE_GAP)) as usize).max(1)
}

/// What the dialog's margins, its padding and the picker's header take from the window's height,
/// besides the list.
const LIST_CHROME: f32 = 160.;

const QUALITY_W: f32 = 104.;
const QUALITY_MENU_W: f32 = 232.;

impl Render for PickerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (viewport_w, viewport_h) = design_viewport(window);
        // The dialog fits the window at every size: the list takes what the header and margins
        // leave.
        let list_max = (viewport_h - LIST_CHROME).clamp(120., 440.);
        let handler = self.on_pick.clone();
        let quality = self.quality.clone();
        let pick: Pick = Rc::new(move |source, window, cx| {
            if let Some(quality) = quality.read(cx).selected_value().copied() {
                handler(source, quality, window, cx);
            }
        });
        let theme = cx.theme();
        let (muted, fg, danger) = (theme.muted_foreground, theme.foreground, theme.danger);
        let mut body: Vec<AnyElement> = Vec::new();
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
                    .px(u(ITEM_INSET))
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
                let screen_entries = entries(&screens, &self.thumbs, &mut stops);
                let window_entries = entries(&windows, &self.thumbs, &mut stops);
                let test_entries = entries(&tests, &self.thumbs, &mut stops);
                let columns = columns(viewport_w);
                for (label, ids, listed, columns) in [
                    (
                        "Screens",
                        &SCREEN,
                        &screen_entries,
                        columns.min(SCREEN_MAX_COLUMNS),
                    ),
                    ("Windows", &WINDOW, &window_entries, columns),
                    ("Test patterns", &TEST, &test_entries, columns),
                ] {
                    if !listed.is_empty() {
                        body.push(section(label, grid(listed, ids, columns, &pick, cx), cx));
                    }
                }
                if screens.is_empty() && windows.is_empty() && tests.is_empty() {
                    body.push(
                        div()
                            .py_6()
                            .px(u(ITEM_INSET))
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
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .pl(u(ITEM_INSET))
                    .pr(u(SCROLL_LANE))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_lg()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child("Share your screen"),
                    )
                    .child(
                        div()
                            .id("share-quality")
                            .w(u(QUALITY_W))
                            .flex_shrink_0()
                            .child(
                                page_select(&self.quality, "Quality").menu_width(u(QUALITY_MENU_W)),
                            )
                            .test_support(),
                    )
                    .child(
                        page_icon_button("picker-close", IconName::Close, cx)
                            .debug_selector(|| "picker-close".into())
                            .accessibility_label("Close")
                            .ghost()
                            .tooltip("Close")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .child(
                        div()
                            .id("source-picker-list")
                            .max_h(u(list_max))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .pr(u(SCROLL_LANE))
                            .child(div().flex().flex_col().gap(u(SECTION_GAP)).children(body))
                            .test_support(),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .child(Scrollbar::new(&self.scroll).axis(ScrollbarAxis::Vertical)),
                    ),
            )
            .test_support()
    }
}

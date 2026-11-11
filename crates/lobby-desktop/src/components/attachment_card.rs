//! A Message's attachments in the feed: a picture in the box its preview's size reserves, or a
//! compact file card. The box is known before any byte arrives, so nothing moves when the image
//! lands. The file card is also the composer's draft chip, which shows its upload as it goes.

use std::{f32::consts::TAU, rc::Rc, time::Duration};

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, ClickEvent, Context, ElementId, Entity,
    FontWeight, Hsla, ImageSource, InteractiveElement as _, IntoElement, MouseButton, ObjectFit,
    ParentElement, PathBuilder, Pixels, Refineable as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, StyledImage as _,
    TestSupportExt as _, Window,
    assets::IconName,
    base::{
        Progress, Transition,
        plot::shape::{Arc, ArcData},
        transition, v_flex,
    },
    black, canvas, div, img, point,
    prelude::FluentBuilder as _,
    relative, white,
};
use lobby_client::{Variant, copy, files::open_allowed, picture};
use lobby_protocol::{Attachment, BlobId, Preview};

use super::{
    BORDER,
    context_menu::{menu_frame, menu_item},
    feed::ChangeMessage,
    media_images::{MediaImages, PictureOf, Slot},
    nested_radius,
};
use crate::kit::{
    ActiveTheme as _, Colorize as _,
    menu::{ContextMenuExt as _, PopupMenu},
};
use crate::scale::u;
use crate::ui::{self, ClickHandler, Icon, Skeleton};

/// A picture's frame; the picture inside its border has the nested radius.
const PICTURE_RADIUS: f32 = 8.;
pub const PICTURE_MAX_W: f32 = 360.;
pub const PICTURE_MAX_H: f32 = 240.;
pub const PICTURE_MIN: f32 = 48.;
pub const CARD_GAP: f32 = 6.;

/// What a card asks of an attachment: the preview dialog, or straight to one of its actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardAction {
    View,
    Open,
    Save,
}

pub type OpenAttachment = Rc<dyn Fn(&Attachment, CardAction, &mut Window, &mut App)>;

/// The preview picture a card shows for a blob.
pub type CardImages = Rc<dyn Fn(&BlobId, &mut App) -> Slot>;

/// The Lobby's previews, loaded through its [`MediaImages`].
pub fn previews(images: Entity<MediaImages>) -> CardImages {
    Rc::new(move |blob, cx| images.update(cx, |m, cx| m.slot(blob, Variant::Preview, cx)))
}

/// What a Message row draws pictures from (its attachments' previews and its author's avatar)
/// and what its menus act through.
#[derive(Clone)]
pub struct Cards {
    pub images: CardImages,
    pub avatars: PictureOf,
    pub max_w: f32,
    pub on_open: OpenAttachment,
    pub on_change: ChangeMessage,
}

pub fn picture_box(p: Preview, max_w: f32) -> (f32, f32) {
    let (w, h) = (p.width.max(1) as f32, p.height.max(1) as f32);
    let max_w = max_w.clamp(PICTURE_MIN, PICTURE_MAX_W);
    let scale = (max_w / w).min(PICTURE_MAX_H / h).min(1.);
    (
        (w * scale).floor().max(PICTURE_MIN),
        (h * scale).floor().max(PICTURE_MIN),
    )
}

pub fn size_label(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.1} GB", b / (KB * KB * KB))
    }
}

pub fn file_icon(mime: &str) -> IconName {
    match mime.split('/').next().unwrap_or_default() {
        "image" => IconName::FileImage,
        "audio" => IconName::FileMusic,
        "video" => IconName::FilePlay,
        "text" => IconName::FileText,
        _ if mime == "application/pdf" => IconName::FileText,
        _ if mime.contains("zip") || mime.contains("compressed") => IconName::FileArchive,
        _ => IconName::File,
    }
}

pub fn kind_label(name: &str, mime: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 5 => {
            ext.to_uppercase()
        }
        _ => mime.rsplit('/').next().unwrap_or(mime).to_uppercase(),
    }
}

pub fn card_id(row: &str, j: usize) -> SharedString {
    format!("attachment-{row}-{j}").into()
}

pub fn attachment_cards(
    row: &SharedString,
    attachments: &[Attachment],
    cards: &Cards,
    cx: &mut App,
) -> AnyElement {
    let children: Vec<AnyElement> = attachments
        .iter()
        .enumerate()
        .map(|(j, a)| match picture(a) {
            Some(p) => picture_card(card_id(row, j), a, p, cards, cx),
            None => file_card(card_id(row, j), a, cards),
        })
        .collect();
    div()
        .w_full()
        .min_w_0()
        .pt_1()
        .flex()
        .flex_wrap()
        .gap(u(CARD_GAP))
        .children(children)
        .into_any_element()
}

fn picture_card(
    id: SharedString,
    a: &Attachment,
    p: Preview,
    cards: &Cards,
    cx: &mut App,
) -> AnyElement {
    let (w, h) = picture_box(p, cards.max_w);
    let slot = (cards.images)(&a.id, cx);
    let theme = cx.theme();
    let (border, muted, muted_fg, ring) = (
        theme.border,
        theme.muted,
        theme.muted_foreground,
        theme.ring,
    );
    let inner_radius = u(nested_radius(PICTURE_RADIUS, BORDER));
    let inner: AnyElement = match slot {
        Slot::Ready(image) => img(image)
            .size_full()
            .rounded(inner_radius)
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        Slot::Loading => Skeleton::new()
            .size_full()
            .rounded(inner_radius)
            .into_any_element(),
        Slot::Failed => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(IconName::ImageOff)
                    .size(u(20.))
                    .text_color(muted_fg),
            )
            .into_any_element(),
    };
    let open = cards.on_open.clone();
    let attachment = a.clone();
    let name: SharedString = a.name.clone().into();
    div()
        .id(id)
        .flex_shrink_0()
        .w(u(w))
        .h(u(h))
        .rounded(u(PICTURE_RADIUS))
        .overflow_hidden()
        .border_1()
        .border_color(border)
        .bg(muted)
        .cursor_pointer()
        .hover(move |s| s.border_color(ring))
        .aria_label(name.clone())
        .tooltip(move |_, cx| ui::tooltip(name.clone(), cx))
        .on_click({
            let (open, attachment) = (open.clone(), attachment.clone());
            move |_, window, cx| open(&attachment, CardAction::View, window, cx)
        })
        .child(inner)
        .test_support()
        .context_menu(card_menu(attachment, open))
        .into_any_element()
}

/// Open only for what the preview dialog lets Open: the same allowlist.
fn card_menu(
    attachment: Attachment,
    act: OpenAttachment,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |menu, window, _| {
        let mut menu = menu_frame(menu, window);
        if open_allowed(&attachment.name, &attachment.mime_type) {
            let (attachment, act) = (attachment.clone(), act.clone());
            menu = menu.item(
                menu_item("menu-open", copy::OPEN)
                    .on_click(move |_, window, cx| act(&attachment, CardAction::Open, window, cx)),
            );
        }
        let (attachment, act) = (attachment.clone(), act.clone());
        menu.item(
            menu_item("menu-save", copy::SAVE_FILE)
                .on_click(move |_, window, cx| act(&attachment, CardAction::Save, window, cx)),
        )
    }
}

fn file_card(id: SharedString, a: &Attachment, cards: &Cards) -> AnyElement {
    let open = cards.on_open.clone();
    let attachment = a.clone();
    let menu = card_menu(a.clone(), open.clone());
    let detail = format!(
        "{} · {}",
        kind_label(&a.name, &a.mime_type),
        size_label(a.size)
    );
    div()
        .id(id.clone())
        .flex_shrink_0()
        .max_w_full()
        .min_w_0()
        .aria_label(SharedString::from(a.name.clone()))
        .child(
            FileCard::new(
                SharedString::from(format!("{id}-card")),
                CardSize::Feed,
                file_icon(&a.mime_type),
                a.name.clone(),
                detail,
            )
            .on_click(move |_, window, cx| open(&attachment, CardAction::View, window, cx)),
        )
        .test_support()
        .context_menu(menu)
        .into_any_element()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardSize {
    Feed,
    Draft,
}

struct Metrics {
    height: f32,
    width: f32,
    media: f32,
    glyph: f32,
    pad_start: f32,
    pad_end: f32,
    gap: f32,
    title: f32,
    detail: f32,
    ring: f32,
}

impl CardSize {
    fn metrics(self) -> Metrics {
        match self {
            CardSize::Feed => Metrics {
                height: 48.,
                width: 200.,
                media: 32.,
                glyph: 16.,
                pad_start: 6.,
                pad_end: 8.,
                gap: 8.,
                title: 12.,
                detail: 11.,
                ring: 20.,
            },
            CardSize::Draft => Metrics {
                height: 40.,
                width: 176.,
                media: 28.,
                glyph: 14.,
                pad_start: 4.,
                pad_end: 6.,
                gap: 6.,
                title: 11.,
                detail: 10.,
                ring: 14.,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Upload {
    Waiting,
    /// This far, in percent.
    Sending(f32),
    Done,
    Failed,
}

/// A file as a card: its picture or icon, its name and one line of detail, and what it can do.
/// `Styled` dresses the card itself, for its corners and its width.
#[derive(IntoElement)]
pub struct FileCard {
    id: SharedString,
    size: CardSize,
    upload: Upload,
    icon: IconName,
    picture: Option<ImageSource>,
    media_radius: Option<Pixels>,
    title: SharedString,
    detail: SharedString,
    actions: Vec<AnyElement>,
    on_click: Option<ClickHandler>,
    tooltip: Option<SharedString>,
    style: StyleRefinement,
}

impl FileCard {
    pub fn new(
        id: impl Into<SharedString>,
        size: CardSize,
        icon: IconName,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            size,
            upload: Upload::Done,
            icon,
            picture: None,
            media_radius: None,
            title: title.into(),
            detail: detail.into(),
            actions: Vec::new(),
            on_click: None,
            tooltip: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn upload(mut self, upload: Upload) -> Self {
        self.upload = upload;
        self
    }

    /// A preview picture in place of the icon.
    pub fn picture(mut self, picture: impl Into<ImageSource>) -> Self {
        self.picture = Some(picture.into());
        self
    }

    pub fn media_radius(mut self, radius: Pixels) -> Self {
        self.media_radius = Some(radius);
        self
    }

    /// Controls at the card's end. They take their own presses, never the card's.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }
}

impl Styled for FileCard {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for FileCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let radii = tokens.radius;
        let m = self.size.metrics();
        let failed = self.upload == Upload::Failed;
        let clickable = self.on_click.is_some();
        let c = tokens.colors;
        let (border, destructive, background, foreground, muted, muted_fg, primary) = (
            c.border,
            c.destructive,
            c.background,
            c.foreground,
            c.muted,
            c.muted_foreground,
            c.primary,
        );
        let (radius, media_radius) = match self.size {
            CardSize::Feed => (radii.lg, radii.md),
            CardSize::Draft => (radii.md, radii.sm),
        };
        let radius = self
            .style
            .corner_radii
            .top_left
            .map_or(radius, |r| r.to_pixels(window.rem_size()));
        let media_radius = self.media_radius.unwrap_or(media_radius);
        let percent = match self.upload {
            Upload::Sending(percent) => Some(percent.clamp(0., 100.)),
            _ => None,
        };
        let ring_id = ElementId::from((ElementId::from(self.id.clone()), "progress"));
        let busy = |color: Hsla, window: &mut Window, cx: &mut App| {
            percent.map(|p| ring(ring_id.clone(), p, u(m.ring), color, window, cx))
        };

        let has_picture = self.picture.is_some();
        let glyph = |color: Option<Hsla>| {
            Icon::new(IconName::Ban)
                .size(u(m.glyph))
                .when_some(color, |i, c| i.text_color(c))
                .into_any_element()
        };
        let slot_content = match (&self.picture, self.upload) {
            (Some(_), _) => None,
            (None, Upload::Sending(_)) => busy(primary, window, cx),
            (None, Upload::Failed) => Some(glyph(None)),
            (None, _) => Some(Icon::new(self.icon).into_any_element()),
        };
        let scrim = match self.upload {
            _ if !has_picture => None,
            Upload::Sending(_) => busy(white(), window, cx).map(|ring| (PROGRESS_SCRIM, ring)),
            Upload::Failed => Some((FAILED_SCRIM, glyph(Some(white())))),
            _ => None,
        };
        let failed_media = failed && !has_picture;
        let media = div()
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .size(u(m.media))
            .text_size(u(m.glyph))
            .rounded(media_radius)
            .bg(if failed_media {
                destructive.opacity(0.1)
            } else {
                muted
            })
            .text_color(if failed_media {
                destructive
            } else {
                foreground
            })
            // gpui clips square, so the picture and its scrim round their own corners.
            .when_some(self.picture, |slot, picture| {
                slot.child(
                    img(picture)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .rounded(media_radius)
                        .object_fit(ObjectFit::Cover),
                )
            })
            .children(slot_content)
            .when_some(scrim, |slot, (opacity, control)| {
                slot.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(media_radius)
                        .bg(black().opacity(opacity))
                        .child(control),
                )
            });

        let detail = match percent {
            Some(p) => format!("{} · {}%", self.detail, p.round() as u32).into(),
            None => self.detail,
        };
        let title = div()
            .max_w_full()
            .min_w_0()
            .truncate()
            .font_weight(FontWeight::MEDIUM)
            .map(|t| match percent {
                Some(_) => t.child(shimmer(self.title.clone(), cx)),
                None => t.child(self.title),
            });
        let content = v_flex()
            .max_w_full()
            .min_w_0()
            .flex_1()
            .gap_0p5()
            .line_height(relative(1.25))
            .child(title)
            .child(
                div()
                    .max_w_full()
                    .min_w_0()
                    .truncate()
                    .text_size(u(m.detail))
                    .line_height(relative(1.25))
                    .text_color(if failed { destructive } else { muted_fg })
                    .child(detail),
            );

        let id = self.id;
        let mut card = div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .max_w_full()
            .min_w_0()
            .w(u(m.width))
            .h(u(m.height))
            .gap(u(m.gap))
            .pl(u(m.pad_start))
            .pr(u(m.pad_end))
            .rounded(radius)
            .border_1()
            .border_color(if failed { destructive } else { border })
            .when(self.upload == Upload::Waiting, |c| c.border_dashed())
            .bg(background)
            .text_color(foreground)
            // Registered either way: a hover style added only sometimes stays cached.
            .hover(move |s| {
                if clickable {
                    s.bg(muted.opacity(0.5))
                } else {
                    s
                }
            })
            .line_height(relative(1.25))
            .text_size(u(m.title))
            .child(media)
            .child(content)
            .when_some(percent, |c, p| c.child(upload_bar(p, radius, primary)))
            .when_some(self.on_click, |c, on_click| {
                c.child(
                    div()
                        .id(ElementId::from(id.clone()))
                        .absolute()
                        .inset_0()
                        .on_click(move |event, window, cx| on_click(event, window, cx)),
                )
            })
            .when(!self.actions.is_empty(), |c| {
                c.child(
                    div()
                        .relative()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_1()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(self.actions),
                )
            });
        card.style().refine(&self.style);
        match self.tooltip {
            Some(text) => card
                .id((ElementId::from(id), "card"))
                .tooltip(move |_, cx| ui::tooltip(text.clone(), cx))
                .into_any_element(),
            None => card.into_any_element(),
        }
    }
}

const PROGRESS_SCRIM: f32 = 0.35;
const FAILED_SCRIM: f32 = 0.55;

fn ring(
    id: ElementId,
    percent: f32,
    size: gpui_kit::Rems,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let motion = cx.theme().motion_tokens();
    let policy = Transition::new(motion.duration_normal).easing(motion.easing_move.clone());
    let key = ElementId::from((id.clone(), "value"));
    let shown = transition(key, percent, policy, window, cx).clamp(0., 100.);
    Progress::new(id)
        .value(percent)
        .flex()
        .items_center()
        .justify_center()
        .line_height(relative(1.))
        .size(size)
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| paint_ring(bounds, shown, color, window),
            )
            .absolute()
            .size_full(),
        )
        .into_any_element()
}

fn paint_ring(bounds: Bounds<Pixels>, percent: f32, color: Hsla, window: &mut Window) {
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let stroke = (f32::from(bounds.size.width) * 0.15).min(5.);
    let middle = (side - stroke) / 2.;
    let arc = Arc::new()
        .inner_radius(middle - stroke / 2.)
        .outer_radius(middle + stroke / 2.);
    arc.paint(
        &ArcData::new(&(), 0, 100., 0., TAU),
        color.opacity(0.2),
        &bounds,
        window,
    );
    if percent > 0. {
        let end = percent / 100. * TAU;
        arc.paint(
            &ArcData::new(&(), 1, percent, 0., end),
            color,
            &bounds,
            window,
        );
    }
}

const BAR_THICKNESS: f32 = 2.;

/// gpui clips square, so the bar is a path whose ends follow the card's inner corners.
fn upload_bar(percent: f32, radius: Pixels, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            if let Some(path) = bar_path(bounds, percent, f32::from(radius) - BORDER) {
                window.paint_path(path, color);
            }
        },
    )
    .absolute()
    .inset_0()
}

fn bar_path(bounds: Bounds<Pixels>, percent: f32, radius: f32) -> Option<gpui_kit::Path<Pixels>> {
    const ROWS: usize = 6;
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let radius = radius.clamp(0., w.min(h) / 2.);
    let end = w * (percent / 100.).clamp(0., 1.);
    let top = h - BAR_THICKNESS;
    if end <= 0. || top <= 0. {
        return None;
    }
    let inset = |y: f32| {
        let below = y - (h - radius);
        if radius > 0. && below > 0. {
            radius - (radius * radius - below * below).max(0.).sqrt()
        } else {
            0.
        }
    };
    let at = |x: f32, y: f32| {
        point(
            bounds.origin.x + Pixels::from(x),
            bounds.origin.y + Pixels::from(y),
        )
    };
    let ys: Vec<f32> = (0..=ROWS)
        .map(|i| top + BAR_THICKNESS * i as f32 / ROWS as f32)
        .collect();
    let mut path = PathBuilder::fill();
    path.move_to(at(inset(top), top));
    for &y in &ys {
        path.line_to(at((w - inset(y)).min(end), y));
    }
    for &y in ys.iter().rev() {
        path.line_to(at(inset(y), y));
    }
    path.build().ok()
}

const SHIMMER_SWEEP: Duration = Duration::from_secs(2);
const SHIMMER_HALF_WIDTH: f32 = 0.3;
const SHIMMER_LAYERS: usize = 12;

fn shimmer(text: SharedString, cx: &App) -> AnyElement {
    let plain = div().relative().min_w_0().child(text.clone());
    if cx.reduce_motion() {
        return plain.into_any_element();
    }
    let theme = cx.theme();
    let dark = theme.is_dark();
    let (target, other) = if dark {
        (theme.foreground, theme.background)
    } else {
        (theme.background, theme.foreground)
    };
    let ink = theme.foreground;
    let target = if (target.l - ink.l).abs() < 0.1 {
        other
    } else {
        target
    };
    let peak: f32 = if dark { 0.6 } else { 0.75 };
    let layer_alpha = 1. - (1. - peak).powf(1. / SHIMMER_LAYERS as f32);
    let band = ink.mix_oklab(target, 0.2).opacity(layer_alpha);
    plain
        .with_animation(
            ElementId::from(text.clone()),
            Animation::new(SHIMMER_SWEEP).repeat_synced(),
            move |el, phase| {
                el.children(
                    (0..SHIMMER_LAYERS)
                        .filter_map(|layer| shimmer_layer(text.clone(), phase, layer, band)),
                )
            },
        )
        .into_any_element()
}

fn shimmer_layer(
    text: SharedString,
    phase: f32,
    layer: usize,
    color: Hsla,
) -> Option<gpui_kit::Div> {
    let margin = SHIMMER_HALF_WIDTH + 0.05;
    let middle = phase * (1. + 2. * margin) - margin;
    let half = SHIMMER_HALF_WIDTH * (1. - layer as f32 / SHIMMER_LAYERS as f32);
    let (left, right) = ((middle - half).max(0.), (middle + half).min(1.));
    let width = right - left;
    (width > 0.).then(|| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(relative(left))
            .w(relative(width))
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(relative(-left / width))
                    .w(relative(1. / width))
                    .text_color(color)
                    .child(text),
            )
    })
}

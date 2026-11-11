//! The composer's attachments before Send: one row of file cards that scrolls sideways, so ten
//! files never wrap and the composer grows by one fixed row only while drafts exist.

use std::time::Duration;

use gpui_kit::{
    AnyElement, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window,
    assets::IconName,
    base::{InteractiveElementExt as _, ScrollableMask, Transition, h_flex, transition},
    div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _,
};
use lobby_client::{DraftAttachment, DraftId, DraftState, Variant, picture};

use super::{
    BORDER, Callback, SURFACE_INNER_RADIUS, SURFACE_PAD,
    attachment_card::{CardSize, FileCard, Upload, file_icon, size_label},
    media_images::{MediaImages, Slot},
    nested_radius,
};
use crate::scale::{device_in, u};
use crate::ui;

const CHIP_H: f32 = 40.;
const ROW_AIR: f32 = 4.;
const CHIP_PAD: f32 = 4.;

const TOP_AIR: f32 = SURFACE_PAD - ROW_AIR;
const CHIP_RADIUS: f32 = SURFACE_INNER_RADIUS;
const MEDIA_RADIUS: f32 = nested_radius(CHIP_RADIUS, BORDER + CHIP_PAD);

pub const DRAFT_STRIP_H: f32 = TOP_AIR + ROW_AIR + CHIP_H + ROW_AIR;

const FAILED_CHIP_MAX_W: f32 = 232.;

const EDGE_FADE_W: f32 = 24.;
const EDGE_FADE: Duration = Duration::from_millis(200);

fn guessed_mime(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") => "image/*",
        Some("mp3" | "wav" | "ogg" | "flac" | "m4a") => "audio/*",
        Some("mp4" | "webm" | "mov" | "mkv") => "video/*",
        Some("txt" | "md" | "csv" | "log") => "text/*",
        Some("pdf") => "application/pdf",
        Some("zip" | "7z" | "rar" | "gz") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// Where a draft's upload is, and the line under its name: its size, or why it failed.
pub fn draft_status(d: &DraftAttachment) -> (Upload, SharedString) {
    match &d.state {
        DraftState::Queued => (Upload::Waiting, size_label(d.size).into()),
        DraftState::Uploading { sent, .. } => {
            let pct = if d.size == 0 {
                0.
            } else {
                (*sent as f32 / d.size as f32 * 100.).min(100.)
            };
            (Upload::Sending(pct), size_label(d.size).into())
        }
        DraftState::Ready(_) => (Upload::Done, size_label(d.size).into()),
        DraftState::Failed { reason, .. } => (Upload::Failed, reason.clone().into()),
    }
}

pub fn draft_strip(
    drafts: &[DraftAttachment],
    images: &Entity<MediaImages>,
    background: Hsla,
    retry: Callback<DraftId>,
    remove: Callback<DraftId>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let chips: Vec<AnyElement> = drafts
        .iter()
        .enumerate()
        .map(|(i, d)| chip(i, d, images, retry.clone(), remove.clone(), cx))
        .collect();
    div()
        .id("draft-strip")
        .w_full()
        .h(u(DRAFT_STRIP_H))
        .pt(u(TOP_AIR))
        .flex_shrink_0()
        .child(scroll_row(chips, background, window, cx))
        .test_support()
        .into_any_element()
}

fn scroll_row(
    chips: Vec<AnyElement>,
    background: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    // The scroll extent is known only after the first layout, so the fades wait one more frame.
    let scroll = window.use_keyed_state("draft-strip-scroll", cx, |_, _| RowScroll {
        handle: ScrollHandle::new(),
        primed: false,
    });
    let view = window.current_view();
    if !scroll.read(cx).primed {
        scroll.update(cx, |s, _| s.primed = true);
        window.on_next_frame(move |_, cx| cx.notify(view));
    }
    let handle = scroll.read(cx).handle.clone();
    let max = f32::from(handle.max_offset().x);
    // Scrolling right makes the offset negative.
    let offset = f32::from(handle.offset().x);
    let scrolls = max > 1.;
    let fade = |key: &'static str, hides: bool, window: &mut Window, cx: &mut App| {
        let target = if hides { 1. } else { 0. };
        transition(key, target, Transition::new(EDGE_FADE), window, cx)
    };
    let leading = fade("draft-strip-leading", scrolls && offset < -1., window, cx);
    let trailing = fade(
        "draft-strip-trailing",
        scrolls && offset > 1. - max,
        window,
        cx,
    );
    let edge = |opacity: f32, leading: bool| {
        let (from, to) = if leading {
            (background, background.opacity(0.))
        } else {
            (background.opacity(0.), background)
        };
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .when(leading, |d| d.left_0())
            .when(!leading, |d| d.right_0())
            .w(u(EDGE_FADE_W))
            .opacity(opacity)
            .bg(linear_gradient(
                90.,
                linear_color_stop(from, 0.),
                linear_color_stop(to, 1.),
            ))
    };
    div()
        .relative()
        .w_full()
        .min_w_0()
        .child(
            h_flex()
                .id("draft-strip-row")
                .w_full()
                .min_w_0()
                .gap_2()
                .py(u(ROW_AIR))
                .overflow_x_scroll()
                .lock_scroll_axis()
                .track_scroll(&handle)
                .on_scroll(move |_, _, cx| cx.notify(view))
                .children(chips),
        )
        .child(ScrollableMask::vertical_wheel(&handle))
        .when(leading > 0., |d| d.child(edge(leading, true)))
        .when(trailing > 0., |d| d.child(edge(trailing, false)))
}

struct RowScroll {
    handle: ScrollHandle,
    primed: bool,
}

fn chip(
    i: usize,
    d: &DraftAttachment,
    images: &Entity<MediaImages>,
    retry: Callback<DraftId>,
    remove: Callback<DraftId>,
    cx: &mut App,
) -> AnyElement {
    let (upload, line) = draft_status(d);
    let mime = d
        .attachment()
        .map_or_else(|| guessed_mime(&d.name), |a| a.mime_type.as_str());
    let thumb = d
        .attachment()
        .filter(|a| picture(a).is_some())
        .and_then(
            |a| match images.update(cx, |m, cx| m.slot(&a.id, Variant::Preview, cx)) {
                Slot::Ready(image) => Some(image),
                _ => None,
            },
        );
    let id = d.id;
    let can_retry = matches!(
        d.state,
        DraftState::Failed {
            retryable: true,
            ..
        }
    );
    let failed = upload == Upload::Failed;
    let mut card = FileCard::new(
        format!("draft-{i}-card"),
        CardSize::Draft,
        file_icon(mime),
        d.name.clone(),
        line.clone(),
    )
    .upload(upload)
    .media_radius(device_in(MEDIA_RADIUS, cx))
    .rounded(device_in(CHIP_RADIUS, cx))
    .when_some(thumb, |c, image| c.picture(image));
    if can_retry {
        card = card.action(
            ui::Button::new(SharedString::from(format!("draft-{i}-retry")))
                .size(ui::Size::Corner)
                .icon(IconName::RefreshCw)
                .accessibility_label("Retry")
                .tooltip("Retry")
                .on_click(move |_, window, cx| retry(id, window, cx)),
        );
    }
    card = card.action(
        ui::Button::new(SharedString::from(format!("draft-{i}-remove")))
            .size(ui::Size::Corner)
            .icon(IconName::Close)
            .accessibility_label("Remove")
            .tooltip("Remove")
            .on_click(move |_, window, cx| remove(id, window, cx)),
    );
    if failed {
        card = card.tooltip(line).w_auto().max_w(u(FAILED_CHIP_MAX_W));
    }
    div()
        .id(SharedString::from(format!("draft-{i}")))
        .flex_shrink_0()
        .child(card)
        .test_support()
        .into_any_element()
}

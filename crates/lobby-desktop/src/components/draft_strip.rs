//! The composer's attachments before Send: one row of kit attachment chips that scrolls sideways,
//! so ten files never wrap and the composer grows by one fixed row only while drafts exist.

use gpui_kit::{
    AnyElement, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, TestSupportExt as _,
    assets::IconName,
    component::{
        Icon, Sizable as _,
        attachment::{
            Attachment as KitAttachment, AttachmentActions, AttachmentContent,
            AttachmentDescription, AttachmentGroup, AttachmentMedia, AttachmentStatus,
            AttachmentTitle,
        },
        button::{Button, ButtonVariants as _},
    },
    div,
};
use lobby_client::{DraftAttachment, DraftId, DraftState, Variant, picture};

use super::{
    BORDER, Callback, SURFACE_INNER_RADIUS, SURFACE_PAD,
    attachment_card::{file_icon, size_label},
    media_images::{MediaImages, Slot},
    nested_radius,
};
use crate::scale::{device_in, u};

/// The kit's extra-small chip is this high, with this much air above and under it in its group,
/// and this much padding before its picture.
const CHIP_H: f32 = 40.;
const GROUP_AIR: f32 = 4.;
const CHIP_PAD: f32 = 4.;

/// The first chip sits in the composer's corner as the Voice foot sits in the sheet's: as far
/// from the top as from the left, so it rounds like it, and its picture inside it. The top
/// edge needs this much more air than the group gives.
const TOP_AIR: f32 = SURFACE_PAD - GROUP_AIR;
const CHIP_RADIUS: f32 = SURFACE_INNER_RADIUS;
const MEDIA_RADIUS: f32 = nested_radius(CHIP_RADIUS, BORDER + CHIP_PAD);

pub const DRAFT_STRIP_H: f32 = TOP_AIR + GROUP_AIR + CHIP_H + GROUP_AIR;

/// A failed chip grows to show its reason up to this: beside one other chip
/// it still fits the default window's composer.
const FAILED_CHIP_MAX_W: f32 = 232.;

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

pub fn draft_status(d: &DraftAttachment) -> (AttachmentStatus, SharedString, Option<f32>) {
    match &d.state {
        DraftState::Queued => (AttachmentStatus::Pending, size_label(d.size).into(), None),
        DraftState::Uploading { sent, .. } => {
            let pct = if d.size == 0 {
                0.
            } else {
                (*sent as f32 / d.size as f32 * 100.).min(100.)
            };
            (
                AttachmentStatus::Uploading,
                size_label(d.size).into(),
                Some(pct),
            )
        }
        DraftState::Ready(_) => (AttachmentStatus::Complete, size_label(d.size).into(), None),
        DraftState::Failed { reason, .. } => {
            (AttachmentStatus::Failed, reason.clone().into(), None)
        }
    }
}

pub fn draft_strip(
    drafts: &[DraftAttachment],
    images: &Entity<MediaImages>,
    background: Hsla,
    retry: Callback<DraftId>,
    remove: Callback<DraftId>,
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
        .child(
            AttachmentGroup::new("draft-strip-row")
                .with_edge_fade(background)
                .gap_2()
                .children(chips),
        )
        .test_support()
        .into_any_element()
}

fn chip(
    i: usize,
    d: &DraftAttachment,
    images: &Entity<MediaImages>,
    retry: Callback<DraftId>,
    remove: Callback<DraftId>,
    cx: &mut App,
) -> AnyElement {
    let (status, line, progress) = draft_status(d);
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
    let media = match thumb {
        Some(image) => AttachmentMedia::new().src(image),
        None => AttachmentMedia::new().child(Icon::new(file_icon(mime))),
    }
    .rounded(device_in(MEDIA_RADIUS, cx));
    let id = d.id;
    let can_retry = matches!(
        d.state,
        DraftState::Failed {
            retryable: true,
            ..
        }
    );
    let mut actions = AttachmentActions::new();
    if can_retry {
        actions = actions.child(
            Button::new(SharedString::from(format!("draft-{i}-retry")))
                .xsmall()
                .ghost()
                .icon(Icon::new(IconName::RefreshCw))
                .accessibility_label("Retry")
                .tooltip("Retry")
                .on_click(move |_, window, cx| retry(id, window, cx)),
        );
    }
    actions = actions.child(
        Button::new(SharedString::from(format!("draft-{i}-remove")))
            .xsmall()
            .ghost()
            .icon(Icon::new(IconName::Close))
            .accessibility_label("Remove")
            .tooltip("Remove")
            .on_click(move |_, window, cx| remove(id, window, cx)),
    );
    let mut card = KitAttachment::new()
        .id(SharedString::from(format!("draft-{i}-card")))
        .xsmall()
        .rounded(device_in(CHIP_RADIUS, cx))
        .status(status)
        .media(media)
        .content(
            AttachmentContent::new()
                .title(AttachmentTitle::new(d.name.clone()))
                .description(AttachmentDescription::new(line.clone())),
        )
        .actions(actions);
    if let Some(pct) = progress {
        card = card.progress(pct);
    }
    if status.is_failed() {
        card = card.tooltip(line).w_auto().max_w(u(FAILED_CHIP_MAX_W));
    }
    div()
        .id(SharedString::from(format!("draft-{i}")))
        .flex_shrink_0()
        .child(card)
        .test_support()
        .into_any_element()
}

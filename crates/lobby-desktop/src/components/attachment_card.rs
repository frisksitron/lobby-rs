//! A Message's attachments in the feed: a picture in the box its preview's size reserves, or a
//! compact file card. The box is known before any byte arrives, so nothing moves when the image
//! lands.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, TestSupportExt as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, Sizable as _,
        attachment::{
            Attachment as KitAttachment, AttachmentContent, AttachmentDescription, AttachmentMedia,
            AttachmentTitle,
        },
        menu::{ContextMenuExt as _, PopupMenu},
        skeleton::Skeleton,
        tooltip::Tooltip,
    },
    div, img,
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
use crate::scale::u;

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
        .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
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
            KitAttachment::new()
                .id(SharedString::from(format!("{id}-card")))
                .small()
                .on_click(move |_, window, cx| open(&attachment, CardAction::View, window, cx))
                .media(AttachmentMedia::new().child(Icon::new(file_icon(&a.mime_type))))
                .content(
                    AttachmentContent::new()
                        .title(AttachmentTitle::new(a.name.clone()))
                        .description(AttachmentDescription::new(detail)),
                ),
        )
        .test_support()
        .context_menu(menu)
        .into_any_element()
}

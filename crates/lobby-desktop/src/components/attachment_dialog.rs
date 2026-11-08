//! The preview dialog of an attachment. "Save" always goes through the Save dialog with the
//! sanitized name. "Open" exists only for the allowlist, through a Mark-of-the-Web copy.

use std::path::PathBuf;

use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, Render, SharedString, Styled as _, StyledImage as _, Task,
    TestSupportExt as _, WeakEntity, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _, Icon, button::ButtonVariants as _,
        dialog::DialogFooter, skeleton::Skeleton,
    },
    div, img,
};
use lobby_client::{
    Issue, IssueKind, MediaCacheError, Variant, copy,
    files::{open_allowed, sanitize_file_name},
    picture,
};
use lobby_protocol::Attachment;

use super::{
    attachment_card::{file_icon, kind_label, size_label},
    controls::page_button,
    media_images::{MediaImages, Slot},
};
use crate::scale::{design_viewport, u};
use crate::{files, lobby_view::LobbyView};

pub fn dialog_width(window_w: f32) -> f32 {
    (window_w - 32.).clamp(280., 760.)
}

pub fn dialog_picture_box(
    aspect: (u32, u32),
    room: (f32, f32),
    full: Option<(u32, u32)>,
) -> (f32, f32) {
    let (w, h) = (aspect.0.max(1) as f32, aspect.1.max(1) as f32);
    let (mut max_w, mut max_h) = (room.0.max(48.), room.1.max(48.));
    if let Some((fw, fh)) = full {
        max_w = max_w.min(fw.max(1) as f32);
        max_h = max_h.min(fh.max(1) as f32);
    }
    let scale = (max_w / w).min(max_h / h);
    ((w * scale).floor(), (h * scale).floor())
}

/// What the dialog's title, footer and padding take from the window's height.
const CHROME_H: f32 = 190.;
const PAD_X: f32 = 24.;

pub struct AttachmentDialog {
    pub attachment: Attachment,
    images: Entity<MediaImages>,
    lobby: WeakEntity<LobbyView>,
    pub saved: Option<PathBuf>,
    busy: Option<Task<()>>,
}

impl AttachmentDialog {
    pub fn new(
        attachment: Attachment,
        images: Entity<MediaImages>,
        lobby: WeakEntity<LobbyView>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        Self {
            attachment,
            images,
            lobby,
            saved: None,
            busy: None,
        }
    }

    pub fn can_open(&self) -> bool {
        open_allowed(&self.attachment.name, &self.attachment.mime_type)
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suggested = sanitize_file_name(&self.attachment.name);
        let pick = files::save_path(&suggested, cx);
        let media = self.images.read(cx).media().clone();
        let attachment = self.attachment.clone();
        self.busy = Some(cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(dest)) = pick.await else {
                let _ = this.update(cx, |d, cx| {
                    d.busy = None;
                    cx.notify();
                });
                return;
            };
            let saved = media.save_as(&attachment, dest).await;
            let _ = this.update_in(cx, |d, window, cx| {
                d.busy = None;
                match saved.unwrap_or(Err(MediaCacheError::Decode)) {
                    Ok(path) => d.saved = Some(path),
                    Err(e) => d.failed(IssueKind::SaveFailed, e, window, cx),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let opened = self.images.read(cx).media().open_copy(&self.attachment);
        self.busy = Some(cx.spawn_in(window, async move |this, cx| {
            let opened = opened.await;
            let _ = this.update_in(cx, |d, window, cx| {
                d.busy = None;
                match opened.unwrap_or(Err(MediaCacheError::Decode)) {
                    Ok(path) => files::open_path(&path, cx),
                    Err(e) => d.failed(IssueKind::OpenFailed, e, window, cx),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn footer(this: &Entity<Self>, cx: &App) -> DialogFooter {
        let d = this.read(cx);
        let muted_fg = cx.theme().muted_foreground;
        let a = &d.attachment;
        let busy = d.busy.is_some();
        let meta = format!(
            "{} · {}",
            kind_label(&a.name, &a.mime_type),
            size_label(a.size)
        );
        let act = |f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let this = this.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                this.update(cx, |d, cx| f(d, window, cx))
            }
        };
        let save = page_button("preview-save", cx)
            .primary()
            .icon(Icon::new(IconName::Download))
            .label(copy::SAVE_FILE)
            .loading(busy && d.saved.is_none())
            .disabled(busy)
            .on_click(act(Self::save));
        let open = d.can_open().then(|| {
            page_button("preview-open", cx)
                .outline()
                .icon(Icon::new(IconName::ExternalLink))
                .label(copy::OPEN)
                .disabled(busy)
                .on_click(act(Self::open))
        });
        let reveal = d.saved.is_some().then(|| {
            page_button("preview-reveal", cx)
                .ghost()
                .icon(Icon::new(IconName::FolderOpen))
                .label("Show in folder")
                .on_click(act(|d, _, cx| d.reveal(cx)))
        });
        DialogFooter::new()
            .w_full()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(meta),
            )
            .children(reveal)
            .children(open)
            .child(save)
    }

    pub fn reveal(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = &self.saved {
            files::reveal_path(path, cx);
        }
    }

    fn failed(
        &self,
        kind: IssueKind,
        e: MediaCacheError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::warn!(blob = %self.attachment.id, error = %e, kind = kind.key(), "attachment action failed");
        let issue = Issue::new(kind, e.to_string());
        let _ = self.lobby.update(cx, |v, cx| v.notify(issue, window, cx));
    }

    fn picture(&mut self, room: (f32, f32), cx: &mut Context<Self>) -> Option<AnyElement> {
        let preview = picture(&self.attachment)?;
        let id = self.attachment.id.clone();
        let size = self.attachment.size;
        let (full, stand_in) = self.images.update(cx, |m, cx| {
            let full = m.slot(&id, Variant::Full { size }, cx);
            let stand_in = match full {
                Slot::Ready(_) => None,
                _ => Some(m.slot(&id, Variant::Preview, cx)),
            };
            (full, stand_in)
        });
        let full_size = match &full {
            Slot::Ready(image) => {
                let s = image.size(0);
                Some((s.width.0.max(0) as u32, s.height.0.max(0) as u32))
            }
            _ => None,
        };
        let (w, h) = dialog_picture_box((preview.width, preview.height), room, full_size);
        let muted = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let shown = match (full, stand_in) {
            (Slot::Ready(image), _) | (_, Some(Slot::Ready(image))) => img(image)
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            (Slot::Failed, Some(Slot::Failed)) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::ImageOff)
                        .size(u(28.))
                        .text_color(muted_fg),
                )
                .into_any_element(),
            _ => Skeleton::new().size_full().into_any_element(),
        };
        Some(
            div()
                .id("preview-picture")
                .w(u(w))
                .h(u(h))
                .rounded_md()
                .overflow_hidden()
                .bg(muted)
                .child(shown)
                .test_support()
                .into_any_element(),
        )
    }
}

impl Render for AttachmentDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (viewport_w, viewport_h) = design_viewport(window);
        let room = (dialog_width(viewport_w) - 2. * PAD_X, viewport_h - CHROME_H);
        let theme = cx.theme();
        let (muted, muted_fg) = (theme.muted, theme.muted_foreground);
        let picture = self.picture(room, cx);
        let a = &self.attachment;
        let body = match picture {
            Some(picture) => div()
                .w_full()
                .flex()
                .justify_center()
                .child(picture)
                .into_any_element(),
            None => div()
                .id("preview-file")
                .w_full()
                .h(u(140.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded_md()
                .bg(muted.opacity(0.5))
                .child(
                    Icon::new(file_icon(&a.mime_type))
                        .size(u(44.))
                        .text_color(muted_fg),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(muted_fg)
                        .child(SharedString::from(kind_label(&a.name, &a.mime_type))),
                )
                .test_support()
                .into_any_element(),
        };
        div()
            .id("attachment-dialog")
            .w_full()
            .child(body)
            .test_support()
    }
}

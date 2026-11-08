//! Decoded attachment images as GPU images: one byte-budgeted LRU keyed by blob and variant.
//! Nothing frees an atlas tile by itself, so an image leaving the LRU is released with
//! `drop_image`, at the next render of the Lobby view (`release`) or when this entity goes.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::{App, Context, Entity, ImageSource, RenderImage, Task, Window};
use image::{Frame, RgbaImage};
use lobby_client::{DECODED_BUDGET_BYTES, DecodedImage, Lru, MediaCache, MediaCacheError, Variant};
use lobby_protocol::BlobId;

type Key = (BlobId, Variant);

#[derive(Clone)]
pub enum Slot {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

/// A Member's avatar or the Lobby's icon, as the picture an avatar draws once it is decoded.
/// Until then, and when it cannot be shown, `None` leaves the initials.
pub type PictureOf = Rc<dyn Fn(Option<&BlobId>, &mut App) -> Option<ImageSource>>;

pub fn pictures(images: Entity<MediaImages>) -> PictureOf {
    Rc::new(move |blob, cx| picture(&images, blob, cx))
}

pub fn picture(
    images: &Entity<MediaImages>,
    blob: Option<&BlobId>,
    cx: &mut App,
) -> Option<ImageSource> {
    let blob = blob?;
    match images.update(cx, |m, cx| m.slot(blob, Variant::Square, cx)) {
        Slot::Ready(image) => Some(ImageSource::Render(image)),
        Slot::Loading | Slot::Failed => None,
    }
}

pub struct MediaImages {
    media: MediaCache,
    lru: Lru<Key, Arc<RenderImage>>,
    loading: HashMap<Key, Task<()>>,
    failed: HashSet<Key>,
    evicted: Vec<Arc<RenderImage>>,
    released: usize,
}

impl MediaImages {
    pub fn new(media: MediaCache, cx: &mut Context<Self>) -> Self {
        Self::with_budget(media, DECODED_BUDGET_BYTES, cx)
    }

    pub fn with_budget(media: MediaCache, budget: usize, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this: &mut MediaImages, cx| {
            let held = this.lru.clear();
            for image in this.evicted.drain(..).chain(held) {
                cx.drop_image(image, None);
            }
        })
        .detach();
        Self {
            media,
            lru: Lru::new(budget),
            loading: HashMap::new(),
            failed: HashSet::new(),
            evicted: Vec::new(),
            released: 0,
        }
    }

    pub fn media(&self) -> &MediaCache {
        &self.media
    }

    /// Pictures that failed (a 429, the Lobby down for a moment) are fetched again when drawn.
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }

    /// A miss starts its load once. The arrival only re-renders, as every card already has its
    /// final size.
    pub fn slot(&mut self, blob: &BlobId, variant: Variant, cx: &mut Context<Self>) -> Slot {
        let key = (blob.clone(), variant);
        if let Some(image) = self.lru.get(&key) {
            return Slot::Ready(image.clone());
        }
        if self.failed.contains(&key) {
            return Slot::Failed;
        }
        if !self.loading.contains_key(&key) {
            let task = self.media.image(blob, variant);
            let at = key.clone();
            let load = cx.spawn(async move |this, cx| {
                let decoded = task.await.unwrap_or(Err(MediaCacheError::Decode));
                let _ = this.update(cx, |images, cx| images.arrived(at, decoded, cx));
            });
            self.loading.insert(key, load);
        }
        Slot::Loading
    }

    pub fn shown(&self, blob: &BlobId, variant: Variant) -> bool {
        self.lru.contains(&(blob.clone(), variant))
    }

    pub fn released(&self) -> usize {
        self.released
    }

    pub fn release(&mut self, window: &mut Window) {
        for image in self.evicted.drain(..) {
            let _ = window.drop_image(image);
            self.released += 1;
        }
    }

    fn arrived(
        &mut self,
        key: Key,
        decoded: Result<DecodedImage, MediaCacheError>,
        cx: &mut Context<Self>,
    ) {
        self.loading.remove(&key);
        match decoded.map(render_image) {
            Ok(Some((image, weight))) => {
                let out = self.lru.insert(key, image, weight);
                self.evicted.extend(out);
            }
            Ok(None) => {
                tracing::warn!(blob = %key.0, variant = ?key.1, "a decoded image of the wrong size");
                self.failed.insert(key);
            }
            Err(error) => {
                tracing::warn!(blob = %key.0, variant = ?key.1, %error, "an image could not be shown");
                self.failed.insert(key);
            }
        }
        cx.notify();
    }
}

/// Owned BGRA moves into the frame without a copy: GPUI's `RenderImage` holds BGRA in an
/// `RgbaImage`.
fn render_image(d: DecodedImage) -> Option<(Arc<RenderImage>, usize)> {
    let weight = d.bytes();
    let buffer = RgbaImage::from_raw(d.width, d.height, d.bgra)?;
    Some((Arc::new(RenderImage::new([Frame::new(buffer)])), weight))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::{AppContext as _, TestAppContext};
    use lobby_client::{Client, HttpPolicy, LobbyOrigin, MemoryTokenStore, Paths};

    use super::*;

    fn decoded(side: u32) -> DecodedImage {
        DecodedImage {
            width: side,
            height: side,
            bgra: vec![255; (side * side * 4) as usize],
        }
    }

    fn blob(n: u8) -> BlobId {
        format!("blb_{n:024x}").parse().unwrap()
    }

    #[gpui_kit::test]
    fn an_image_falling_out_of_the_budget_leaves_the_atlas_at_the_next_render(
        cx: &mut TestAppContext,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let client = Client::new(
            rt.handle().clone(),
            Arc::new(MemoryTokenStore::new()),
            HttpPolicy::LoopbackOnly,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let origin = LobbyOrigin::parse("http://127.0.0.1:9", HttpPolicy::LoopbackOnly).unwrap();
        let paths = Paths {
            data_dir: dir.path().to_owned(),
        };
        let media = client.media(&origin, &paths);
        // Room for one 4x4 image (64 bytes), not two.
        let images = cx.new(|cx| MediaImages::with_budget(media, 100, cx));
        images.update(cx, |m, cx| {
            m.arrived((blob(1), Variant::Preview), Ok(decoded(4)), cx);
            m.arrived((blob(2), Variant::Preview), Ok(decoded(4)), cx);
        });
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            images.update(cx, |m, _| {
                assert!(
                    !m.shown(&blob(1), Variant::Preview),
                    "the older one was evicted"
                );
                assert!(m.shown(&blob(2), Variant::Preview));
                assert_eq!(m.released(), 0, "nothing is freed outside a render");
                m.release(window);
                assert_eq!(m.released(), 1, "the evicted image left the atlas");
                m.release(window);
                assert_eq!(m.released(), 1, "once");
            });
        });
    }
}

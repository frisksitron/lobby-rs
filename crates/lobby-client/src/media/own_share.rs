//! The Sharer's own share, drawn without the network: a BGRA copy of the capture's pictures,
//! about 30 times a second, into a [`VideoFeed`] like the one a Viewer gets. The copy is
//! made on the encoder's thread between two pictures and never waits: with nobody looking, or the
//! UI behind, it is skipped.

#[cfg(windows)]
use std::{num::NonZeroUsize, sync::Arc, time::Instant};
use std::{
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use tokio::sync::mpsc;
#[cfg(windows)]
use tracing::warn;

use super::video::{VideoFeed, VideoFrame};
#[cfg(windows)]
use super::{
    MediaError,
    gpu::{Blitter, GpuPicture, Lease, Readback},
    video::{I420, i420_to_bgra},
};

/// Under the 33 ms of 30 fps, so a 30 fps share is copied whole when a picture comes a little
/// early, and a 60 fps one every other picture.
#[cfg(windows)]
pub const OWN_SHARE_GAP: Duration = Duration::from_millis(25);
/// Sharp in the panel, and in fullscreen up to a 1280 px wide window. The Sharer has the screen
/// itself in front of them, so a full 1080p copy would cost the encoder for little.
#[cfg(windows)]
pub const OWN_SHARE_MAX: (u32, u32) = (1280, 720);

/// How the share kept up over `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareLoad {
    pub window: Duration,
    /// Pictures the capture made, whether or not the encoder had room for them.
    pub offered: u32,
    /// New pictures out of the encoder; repeats of the last are not counted.
    pub encoded: u32,
    /// A game covered the shared source in exclusive fullscreen when the window ended.
    pub exclusive_fullscreen: bool,
}

impl ShareLoad {
    pub fn offered_fps(&self) -> u32 {
        per_second(self.offered, self.window)
    }

    pub fn encoded_fps(&self) -> u32 {
        per_second(self.encoded, self.window)
    }
}

fn per_second(count: u32, window: Duration) -> u32 {
    (f64::from(count) / window.as_secs_f64().max(f64::EPSILON)).round() as u32
}

/// Where the copies go: the feed of the one view that shows the share now, if there is one. The
/// Session opens a feed each time the share is shown, which closes the one before, and closes the
/// last when the share ends.
#[derive(Default)]
pub(crate) struct OwnShareFeed(Mutex<Option<mpsc::Sender<VideoFrame>>>);

impl OwnShareFeed {
    pub fn open(&self) -> VideoFeed {
        let (tx, _, feed) = VideoFeed::channel();
        *self.slot() = Some(tx);
        feed
    }

    pub fn close(&self) {
        *self.slot() = None;
    }

    fn slot(&self) -> MutexGuard<'_, Option<mpsc::Sender<VideoFrame>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The feed, when someone reads it and it has room. The encoder only tries the lock.
    fn ready(&self) -> Option<mpsc::Sender<VideoFrame>> {
        let slot = self.0.try_lock().ok()?;
        slot.as_ref()
            .filter(|tx| !tx.is_closed() && tx.capacity() > 0)
            .cloned()
    }
}

#[cfg(windows)]
pub(crate) struct OwnShareTap {
    feed: Arc<OwnShareFeed>,
    gpu: Option<GpuCopy>,
    last: Option<Instant>,
    sent: u64,
    /// The picture last copied, by its capture time, and the feed it went to: a feed opened
    /// again gets the same picture, a still screen is not copied again.
    shown: Option<(Instant, mpsc::Sender<VideoFrame>)>,
}

/// The small draw of a GPU picture, on the picture's device, and its readback. The draw's
/// texture is held only while it is read.
#[cfg(windows)]
struct GpuCopy {
    blitter: Blitter,
    readback: Readback,
    picture: I420,
}

#[cfg(windows)]
impl OwnShareTap {
    pub fn new(feed: Arc<OwnShareFeed>) -> OwnShareTap {
        OwnShareTap {
            feed,
            gpu: None,
            last: None,
            sent: 0,
            shown: None,
        }
    }

    /// The capture's newest picture. Skipped when the last copy is younger than
    /// [`OWN_SHARE_GAP`], nobody shows the share, the UI still holds the copies already sent, or
    /// this feed already has this picture.
    pub fn offer(&mut self, picture: &GpuPicture, now: Instant) {
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < OWN_SHARE_GAP)
        {
            return;
        }
        let Some(out) = self.feed.ready() else {
            return;
        };
        let captured = picture.captured;
        if self
            .shown
            .as_ref()
            .is_some_and(|(at, to)| *at == captured && to.same_channel(&out))
        {
            return;
        }
        match self.frame(&picture.lease) {
            Ok(frame) => {
                // This tap is the feed's only sender, so the room found above is still there.
                if out.try_send(frame).is_ok() {
                    self.last = Some(now);
                    self.sent += 1;
                    self.shown = Some((captured, out));
                }
            }
            Err(error) => warn!(%error, "the copy of the Sharer's own share could not be made"),
        }
    }

    /// `lease`'s picture, drawn at most [`OWN_SHARE_MAX`] on its GPU and read back from there.
    fn frame(&mut self, lease: &Lease) -> Result<VideoFrame, MediaError> {
        let mut copy = match self.gpu.take() {
            Some(copy) if copy.blitter.device() == lease.device() => copy,
            _ => GpuCopy {
                blitter: Blitter::new(lease.device(), 1)?,
                readback: Readback::default(),
                picture: I420::empty(),
            },
        };
        let small = copy
            .blitter
            .blit(lease.texture(), OWN_SHARE_MAX)?
            .ok_or_else(|| MediaError::Video("the own share's texture is held".into()))?;
        copy.readback.read(&small, &mut copy.picture)?;
        drop(small);
        let picture = &copy.picture;
        let frame = i420_to_bgra(&picture.planes(), NonZeroUsize::MIN).map(|bgra| VideoFrame {
            width: picture.width,
            height: picture.height,
            bgra,
            source: (picture.width, picture.height),
            decoded: self.sent + 1,
            sharer: None,
        });
        self.gpu = Some(copy);
        frame
    }
}

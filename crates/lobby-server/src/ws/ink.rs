//! A connection's ink queue. Ink is lossy, so it never shares the reliable queue, whose overflow
//! kicks the connection: a full ink queue drops its oldest batch, and a batch that waited too long
//! is dropped on the way out. The writer sends ink only when no reliable frame is waiting.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::extract::ws::Utf8Bytes;
use tokio::{sync::Notify, time::Instant};

/// Batches a connection may have waiting. At the per-sender budget, this is under a second of a
/// single drawer.
pub const INK_QUEUE_BATCHES: usize = 32;
/// A batch older than this is not worth drawing: the board fades a stroke in 3 s.
pub const INK_MAX_AGE: Duration = Duration::from_millis(750);

#[derive(Clone)]
pub struct InkQueue(Arc<Shared>);

struct Shared {
    capacity: usize,
    frames: Mutex<VecDeque<(Instant, Utf8Bytes)>>,
    ready: Notify,
}

impl InkQueue {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Shared {
            capacity,
            frames: Mutex::new(VecDeque::with_capacity(capacity)),
            ready: Notify::new(),
        }))
    }

    /// Never fails and never waits: the oldest batch makes room.
    pub fn push(&self, frame: Utf8Bytes, now: Instant) {
        {
            let mut frames = self.0.frames.lock().unwrap_or_else(PoisonError::into_inner);
            if frames.len() >= self.0.capacity {
                frames.pop_front();
            }
            frames.push_back((now, frame));
        }
        self.0.ready.notify_one();
    }

    /// The oldest batch that is still fresh.
    pub fn try_pop(&self) -> Option<Utf8Bytes> {
        let now = Instant::now();
        let mut frames = self.0.frames.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some((queued_at, frame)) = frames.pop_front() {
            if now.saturating_duration_since(queued_at) <= INK_MAX_AGE {
                return Some(frame);
            }
        }
        None
    }

    pub async fn pop(&self) -> Utf8Bytes {
        loop {
            if let Some(frame) = self.try_pop() {
                return frame;
            }
            self.0.ready.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: usize) -> Utf8Bytes {
        Utf8Bytes::from(n.to_string())
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_drops_its_oldest_and_an_old_batch_is_dropped_on_the_way_out() {
        let q = InkQueue::new(3);
        for n in 0..5 {
            q.push(frame(n), Instant::now());
        }
        assert_eq!(q.try_pop().as_deref(), Some("2"));
        tokio::time::advance(INK_MAX_AGE + Duration::from_millis(1)).await;
        q.push(frame(9), Instant::now());
        assert_eq!(q.try_pop().as_deref(), Some("9"), "3 and 4 grew old");
        assert_eq!(q.try_pop(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn pop_waits_for_a_batch() {
        let q = InkQueue::new(3);
        let waiting = tokio::spawn({
            let q = q.clone();
            async move { q.pop().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        q.push(frame(1), Instant::now());
        assert_eq!(waiting.await.unwrap().as_str(), "1");
    }
}

//! The only code that touches a connection's sink: queued frames, pings, a deadline per write,
//! and the close frame.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes};
use futures_util::{Sink, SinkExt as _};
use lobby_protocol::close;
use tokio::{
    sync::mpsc,
    time::{Instant, MissedTickBehavior, interval_at, timeout, timeout_at},
};

use crate::lobby::{CloseReason, InkQueue, Kick};

/// Once the connection is ending, the queued frames, the close frame and the closing handshake
/// share this budget. Far below `write_timeout`, so a peer that stopped reading cannot hold
/// shutdown for the whole write deadline.
pub(crate) const CLOSE_BUDGET: Duration = Duration::from_secs(1);
/// Frames written before one flush, so a long burst still reaches the peer in parts.
const WRITE_BATCH: usize = 64;

enum End {
    Close { code: u16, flush: bool },
    Drop,
}

pub(crate) async fn run<W>(
    mut sink: W,
    mut queue: mpsc::Receiver<Utf8Bytes>,
    ink: InkQueue,
    kick: Kick,
    ping_every: Duration,
    write_timeout: Duration,
) where
    W: Sink<Message> + Unpin,
{
    let mut ping = interval_at(Instant::now() + ping_every, ping_every);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let end = loop {
        let out = tokio::select! {
            biased;
            _ = kick.kicked() => break kicked(&kick),
            frame = queue.recv() => match frame {
                Some(text) => Message::Text(text),
                None => break End::Close { code: close::NORMAL, flush: false },
            },
            _ = ping.tick() => Message::Ping(Default::default()),
            text = ink.pop() => Message::Text(text),
        };
        // The write is polled before the kick, so a frame dequeued just before a kick is at
        // least handed to the socket; a kick then ends a write the peer is not reading.
        let sent = tokio::select! {
            biased;
            sent = timeout(write_timeout, write_batch(&mut sink, out, &mut queue)) => sent,
            _ = kick.kicked() => break kicked(&kick),
        };
        match sent {
            Ok(Ok(())) => {}
            Ok(Err(_)) => break End::Drop,
            Err(_) => {
                kick.kick(CloseReason::SlowConsumer);
                break End::Drop;
            }
        }
    };
    kick.stop();
    let deadline = Instant::now() + CLOSE_BUDGET;
    if let End::Close { code, flush } = end {
        if flush {
            // Frames queued before the kick (the ERROR that explains it) go out first.
            while let Ok(text) = queue.try_recv() {
                if !matches!(
                    timeout_at(deadline, sink.send(Message::Text(text))).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        }
        let frame = CloseFrame {
            code,
            reason: Utf8Bytes::from_static(""),
        };
        let _ = timeout_at(deadline, sink.send(Message::Close(Some(frame)))).await;
    }
    let _ = timeout_at(deadline, sink.close()).await;
}

/// `first`, then what else is queued already, up to [`WRITE_BATCH`] frames, with one flush: a
/// burst (READY's catch-up, a fan-out) goes to the socket in one write, not one per frame.
async fn write_batch<W>(
    sink: &mut W,
    first: Message,
    queue: &mut mpsc::Receiver<Utf8Bytes>,
) -> Result<(), W::Error>
where
    W: Sink<Message> + Unpin,
{
    sink.feed(first).await?;
    for _ in 1..WRITE_BATCH {
        let Ok(text) = queue.try_recv() else { break };
        sink.feed(Message::Text(text)).await?;
    }
    sink.flush().await
}

fn kicked(kick: &Kick) -> End {
    let reason = kick.reason();
    End::Close {
        code: reason.code(),
        flush: reason != CloseReason::SlowConsumer,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::lobby::INK_QUEUE_BATCHES;

    struct Stalled;

    impl Sink<Message> for Stalled {
        type Error = axum::Error;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
        fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
    }

    /// Takes every frame at once and counts the frames and the flushes.
    #[derive(Clone, Default)]
    struct Counting {
        frames: Arc<AtomicUsize>,
        flushes: Arc<AtomicUsize>,
    }

    impl Sink<Message> for Counting {
        type Error = axum::Error;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
            self.frames.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    /// A burst already queued (a fan-out, READY's catch-up) goes to the socket in one write.
    #[tokio::test(start_paused = true)]
    async fn a_queued_burst_goes_out_with_one_flush() {
        let kick = Kick::child_of(&CancellationToken::new());
        let (tx, rx) = mpsc::channel(16);
        for _ in 0..10 {
            tx.try_send(Utf8Bytes::from_static("chat")).unwrap();
        }
        let sink = Counting::default();
        let writer = tokio::spawn(run(
            sink.clone(),
            rx,
            InkQueue::new(4),
            kick.clone(),
            Duration::from_secs(10),
            Duration::from_secs(10),
        ));
        while sink.frames.load(Ordering::SeqCst) < 10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(sink.flushes.load(Ordering::SeqCst), 1);
        kick.kick(CloseReason::Shutdown);
        writer.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn reliable_frames_go_out_before_waiting_ink() {
        let kick = Kick::child_of(&CancellationToken::new());
        let (tx, rx) = mpsc::channel(4);
        let ink = InkQueue::new(4);
        ink.push(Utf8Bytes::from_static("ink"), Instant::now());
        tx.try_send(Utf8Bytes::from_static("chat")).unwrap();
        let (wire, mut sent) = mpsc::channel(8);
        let writer = tokio::spawn(run(
            tokio_util::sync::PollSender::new(wire),
            rx,
            ink,
            kick.clone(),
            Duration::from_secs(10),
            Duration::from_secs(10),
        ));
        let mut texts = Vec::new();
        for _ in 0..2 {
            let Some(Message::Text(text)) = sent.recv().await else {
                panic!("a text frame")
            };
            texts.push(text.to_string());
        }
        assert_eq!(texts, ["chat", "ink"]);
        kick.kick(CloseReason::Shutdown);
        writer.await.unwrap();
    }

    /// A Viewer who reads one frame every 10 ms while 200 batches of ink arrive in 400 ms, with a
    /// Chat message among them: the old ink is dropped, the newest comes through, Chat is not
    /// held behind ink, and the connection is not kicked.
    #[tokio::test(start_paused = true)]
    async fn a_viewer_who_reads_slowly_loses_old_ink_and_is_never_kicked() {
        let kick = Kick::child_of(&CancellationToken::new());
        let (tx, rx) = mpsc::channel(4);
        let ink = InkQueue::new(INK_QUEUE_BATCHES);
        let (wire, mut sent) = mpsc::channel(1);
        let writer = tokio::spawn(run(
            tokio_util::sync::PollSender::new(wire),
            rx,
            ink.clone(),
            kick.clone(),
            Duration::from_secs(10),
            Duration::from_secs(10),
        ));
        let reader = tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(Message::Text(text)) = sent.recv().await {
                got.push(text.to_string());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            got
        });
        for n in 0..200 {
            ink.push(Utf8Bytes::from(format!("ink{n}")), Instant::now());
            if n == 150 {
                tx.send(Utf8Bytes::from_static("chat")).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            timeout(Duration::ZERO, kick.kicked()).await.is_err(),
            "a full ink queue kicks nobody"
        );
        kick.stop();
        writer.await.unwrap();
        let got = reader.await.unwrap();

        let numbers: Vec<usize> = got
            .iter()
            .filter_map(|t| t.strip_prefix("ink")?.parse().ok())
            .collect();
        assert!(
            numbers.len() < 150,
            "old ink was dropped: {} of 200",
            numbers.len()
        );
        assert!(
            numbers.windows(2).all(|w| w[0] < w[1]),
            "in order: {numbers:?}"
        );
        assert_eq!(numbers.last(), Some(&199), "the newest came through");
        let chat = got.iter().position(|t| t == "chat").expect("Chat arrived");
        let last_before: usize = got[chat - 1].strip_prefix("ink").unwrap().parse().unwrap();
        assert!(
            last_before < 155 && chat < got.len() - 1,
            "Chat went out ahead of the ink queued behind it: {got:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_shutdown_kick_ends_a_stalled_write_within_the_close_budget() {
        let parent = CancellationToken::new();
        let kick = Kick::child_of(&parent);
        let (tx, rx) = mpsc::channel(4);
        tx.try_send(Utf8Bytes::from_static("frame")).unwrap();
        let write_timeout = Duration::from_secs(10);
        let started = Instant::now();
        let writer = tokio::spawn(run(
            Stalled,
            rx,
            InkQueue::new(4),
            kick.clone(),
            Duration::from_secs(10),
            write_timeout,
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        parent.cancel();
        writer.await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed <= Duration::from_millis(50) + CLOSE_BUDGET,
            "{elapsed:?}"
        );
        assert_eq!(kick.reason(), CloseReason::Shutdown);
        drop(tx);
    }
}

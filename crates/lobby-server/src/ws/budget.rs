//! The budgets of a WebSocket: unidentified sockets per client and in total (an RAII permit,
//! released once the socket identifies or ends), and the per-connection command budgets that live
//! on the reader's stack.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::time::Instant;

use crate::{client_ip::ClientIp, limits::SlidingWindow};

/// IDENTIFY, re-IDENTIFYs included: over budget ⇒ `RATE_LIMITED` and close 4029.
pub const IDENTIFY_BUDGET: (usize, Duration) = (5, Duration::from_secs(30));
/// `RTC_*` ⇒ `SIGNALING_RATE_LIMITED{retryAt}`, checked before anything reaches the SFU queue.
pub const RTC_BUDGET: (usize, Duration) = (300, Duration::from_secs(10));
/// All `SCREEN_SHARE_*` commands and `DRAWING_SET`, together, likewise.
pub const SHARE_BUDGET: (usize, Duration) = (40, Duration::from_secs(10));
/// Excess dropped (a `rid` gets `RATE_LIMITED`).
pub const PRESENCE_BUDGET: (usize, Duration) = (10, Duration::from_secs(10));
/// `TYPING`: excess dropped silently (the client sends one per 3 s while composing, and one at
/// once after a send). An accepted `MESSAGE_SEND` starts it afresh.
pub const TYPING_BUDGET: (usize, Duration) = (1, Duration::from_secs(2));

/// `INK`: excess dropped silently. A drawer sends about 30 batches a second.
pub const INK_BUDGET: (usize, Duration) = (40, Duration::from_secs(1));

fn window((limit, window): (usize, Duration)) -> SlidingWindow {
    SlidingWindow::new(limit, window)
}

/// One connection's command budgets. A reconnect starts them afresh; what must survive one (the
/// send throttle, the voice cooldowns) is per Member, in the `Lobby`.
pub(crate) struct Budgets {
    pub identify: SlidingWindow,
    pub rtc: SlidingWindow,
    pub share: SlidingWindow,
    pub presence: SlidingWindow,
    pub typing: SlidingWindow,
    pub ink: SlidingWindow,
}

impl Budgets {
    pub fn new() -> Self {
        Self {
            identify: window(IDENTIFY_BUDGET),
            rtc: window(RTC_BUDGET),
            share: window(SHARE_BUDGET),
            presence: window(PRESENCE_BUDGET),
            typing: window(TYPING_BUDGET),
            ink: window(INK_BUDGET),
        }
    }

    /// `false`: this batch is over the sender's budget and is dropped.
    pub fn take_ink(&mut self, now: Instant) -> bool {
        self.ink.try_acquire(now).is_ok()
    }

    /// A `MESSAGE_SEND` was accepted: the Message ended the typing the others saw, so the client's
    /// next `TYPING` announces a new one and must not fall into the old one's 2 s.
    pub fn message_sent(&mut self) {
        self.typing = window(TYPING_BUDGET);
    }
}

#[derive(Clone)]
pub struct PreAuthBudget {
    inner: Arc<Mutex<PreAuth>>,
}

struct PreAuth {
    per_ip: usize,
    global: usize,
    by_ip: HashMap<IpAddr, usize>,
    total: usize,
}

pub struct PreAuthPermit {
    budget: PreAuthBudget,
    ip: IpAddr,
}

impl PreAuthBudget {
    pub fn new(per_ip: usize, global: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PreAuth {
                per_ip,
                global,
                by_ip: HashMap::new(),
                total: 0,
            })),
        }
    }

    pub fn try_acquire(&self, client: ClientIp) -> Option<PreAuthPermit> {
        let ip = client.key();
        let mut s = self.inner.lock().expect("pre-auth budget poisoned");
        let mine = s.by_ip.get(&ip).copied().unwrap_or(0);
        if (s.per_ip != 0 && mine >= s.per_ip) || (s.global != 0 && s.total >= s.global) {
            return None;
        }
        *s.by_ip.entry(ip).or_insert(0) += 1;
        s.total += 1;
        Some(PreAuthPermit {
            budget: self.clone(),
            ip,
        })
    }
}

impl Drop for PreAuthPermit {
    fn drop(&mut self) {
        let mut s = self.budget.inner.lock().expect("pre-auth budget poisoned");
        s.total -= 1;
        if let Some(n) = s.by_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                s.by_ip.remove(&self.ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_sender_over_40_batches_a_second_has_the_excess_dropped() {
        let mut budgets = Budgets::new();
        let taken = (0..200)
            .filter(|_| budgets.take_ink(Instant::now()))
            .count();
        assert_eq!(taken, INK_BUDGET.0);
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(!budgets.take_ink(Instant::now()));
        tokio::time::advance(Duration::from_millis(2)).await;
        assert!(budgets.take_ink(Instant::now()), "the window moved on");
    }
}

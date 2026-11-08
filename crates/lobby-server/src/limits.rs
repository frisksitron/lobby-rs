//! Rate-limit primitives: sliding windows, the per-route REST limits per client IP, and the
//! Magic Codes mailed per email and client. Each takes `now` as an argument, so tests use
//! synthetic instants.

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use lobby_protocol::{Email, ErrorBody, ErrorCode, Rid, limits::MAGIC_CODE_SEND_WINDOW};
use tokio::time::Instant;

use crate::client_ip::ClientIp;

pub fn retry_after_ms(retry_at: Instant, now: Instant) -> u64 {
    u64::try_from(retry_at.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)
}

pub fn retry_error(
    code: ErrorCode,
    message: &str,
    rid: Option<Rid>,
    retry_at: Instant,
    now: Instant,
) -> ErrorBody {
    ErrorBody {
        retry_after_ms: Some(retry_after_ms(retry_at, now)),
        ..ErrorBody::new(code, message, rid)
    }
}

/// A bucket of `burst` events that refills one per `every`. It is kept as the instant the bucket
/// is full again, so it needs no timer.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    every: Duration,
    /// How far ahead of now `full_at` may run: the room for `burst - 1` more events.
    slack: Duration,
    full_at: Option<Instant>,
}

impl TokenBucket {
    pub fn new(every: Duration, burst: u32) -> Self {
        Self {
            every,
            slack: every * burst.saturating_sub(1),
            full_at: None,
        }
    }

    pub fn try_acquire(&mut self, now: Instant) -> Result<(), Instant> {
        let full_at = self.full_at.map_or(now, |at| at.max(now));
        if full_at > now + self.slack {
            return Err(full_at - self.slack);
        }
        self.full_at = Some(full_at + self.every);
        Ok(())
    }
}

/// At most `limit` accepted events in any `window`. A refused event is not counted.
#[derive(Debug, Clone)]
pub struct SlidingWindow {
    limit: usize,
    window: Duration,
    events: VecDeque<Instant>,
}

impl SlidingWindow {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            events: VecDeque::with_capacity(limit.min(64)),
        }
    }

    /// `Err(retry_at)`: the instant the oldest counted event leaves the window.
    pub fn try_acquire(&mut self, now: Instant) -> Result<(), Instant> {
        self.prune(now);
        if self.events.len() >= self.limit {
            return Err(self.retry_at(now));
        }
        self.events.push_back(now);
        Ok(())
    }

    pub fn retry_at(&self, now: Instant) -> Instant {
        if self.events.len() < self.limit {
            return now;
        }
        self.events
            .front()
            .map_or(now, |oldest| (*oldest + self.window).max(now))
    }

    /// Drops what left the window; `true` if nothing is left (the window can be forgotten).
    pub fn is_idle(&mut self, now: Instant) -> bool {
        self.prune(now);
        self.events.is_empty()
    }

    fn prune(&mut self, now: Instant) {
        while self
            .events
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            self.events.pop_front();
        }
    }
}

/// The window of every per-IP route limit; the limits are in `router::table`.
pub const ROUTE_WINDOW: Duration = Duration::from_secs(60);

/// The per-IP counters of the REST routes, one sliding window per route and IP, behind the
/// `http.rate_limits` switch. The same switch turns off the per-Member send throttle
/// ([`RouteLimits::enabled`]).
pub struct RouteLimits {
    enabled: bool,
    windows: Mutex<HashMap<(&'static str, IpAddr), SlidingWindow>>,
}

impl RouteLimits {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            windows: Mutex::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Counts one request to `route` from `client`, which may make `limit` of them per window.
    pub fn try_acquire(
        &self,
        route: &'static str,
        limit: usize,
        client: ClientIp,
        now: Instant,
    ) -> Result<(), Instant> {
        if !self.enabled {
            return Ok(());
        }
        self.windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry((route, client.key()))
            .or_insert_with(|| SlidingWindow::new(limit, ROUTE_WINDOW))
            .try_acquire(now)
    }

    pub fn prune(&self, now: Instant) -> usize {
        let mut windows = self.windows.lock().unwrap_or_else(PoisonError::into_inner);
        windows.retain(|_, w| !w.is_idle(now));
        windows.len()
    }
}

pub const CODE_SEND_DAY: Duration = Duration::from_secs(24 * 3600);

/// The Magic Codes mailed per email and client, `short` per [`MAGIC_CODE_SEND_WINDOW`] and
/// `daily` per [`CODE_SEND_DAY`], and per client to any emails, `per_client` per
/// [`CODE_SEND_DAY`]. Nothing is counted per email alone, so a stranger's requests from another
/// address never stop a code reaching the Member. One behind the Member's own address can, for a
/// day: telling Members apart there would tell who is one.
pub struct CodeSends {
    short: usize,
    daily: usize,
    per_client: usize,
    sent: Mutex<Sent>,
}

#[derive(Default)]
struct Sent {
    to_email: HashMap<(Email, IpAddr), [SlidingWindow; 2]>,
    by_client: HashMap<IpAddr, SlidingWindow>,
}

impl CodeSends {
    pub fn new(short: usize, daily: usize, per_client: usize) -> Self {
        Self {
            short,
            daily,
            per_client,
            sent: Mutex::default(),
        }
    }

    /// Counts one send. `Err(retry_at)`, counting nothing, while any window is full.
    pub fn try_acquire(
        &self,
        email: &Email,
        client: ClientIp,
        now: Instant,
    ) -> Result<(), Instant> {
        let (short, daily, per_client) = (self.short, self.daily, self.per_client);
        let site = client.site_key();
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        let Sent {
            to_email,
            by_client,
        } = &mut *sent;
        let [to_short, to_daily] = to_email.entry((email.clone(), site)).or_insert_with(|| {
            [
                SlidingWindow::new(short, MAGIC_CODE_SEND_WINDOW),
                SlidingWindow::new(daily, CODE_SEND_DAY),
            ]
        });
        let from_client = by_client
            .entry(site)
            .or_insert_with(|| SlidingWindow::new(per_client, CODE_SEND_DAY));
        let windows = [to_short, to_daily, from_client];
        let retry_at = windows.iter().map(|w| w.retry_at(now)).max().unwrap_or(now);
        if retry_at > now {
            return Err(retry_at);
        }
        for w in windows {
            let _ = w.try_acquire(now);
        }
        Ok(())
    }

    pub fn prune(&self, now: Instant) -> usize {
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        // The day window holds every send the short one does.
        sent.to_email.retain(|_, [_, daily]| !daily.is_idle(now));
        sent.by_client.retain(|_, w| !w.is_idle(now));
        sent.to_email.len() + sent.by_client.len()
    }
}

/// The voice cooldowns: every attempt counts, and the `limit`-th one within `window` is refused and
/// starts `cooldown`, during which everything is refused. The history is cleared when the cooldown
/// starts.
#[derive(Debug, Clone)]
pub struct Cooldown {
    limit: usize,
    window: Duration,
    cooldown: Duration,
    attempts: VecDeque<Instant>,
    until: Option<Instant>,
}

impl Cooldown {
    pub fn new(limit: usize, window: Duration, cooldown: Duration) -> Self {
        Self {
            limit,
            window,
            cooldown,
            attempts: VecDeque::new(),
            until: None,
        }
    }

    pub fn try_acquire(&mut self, now: Instant) -> Result<(), Instant> {
        if let Some(until) = self.until {
            if now < until {
                return Err(until);
            }
            self.until = None;
        }
        while self
            .attempts
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            self.attempts.pop_front();
        }
        self.attempts.push_back(now);
        if self.attempts.len() >= self.limit {
            self.attempts.clear();
            let until = now + self.cooldown;
            self.until = Some(until);
            return Err(until);
        }
        Ok(())
    }
}

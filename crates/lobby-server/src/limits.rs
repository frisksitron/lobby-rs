//! Rate-limit primitives: sliding windows, the per-route REST limits per client IP, and the
//! Magic Codes mailed per email and client. Each takes `now` as an argument, so tests use
//! synthetic instants.

use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    net::IpAddr,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use lobby_protocol::{Email, ErrorBody, ErrorCode, Rid, limits::MAGIC_CODE_SEND_WINDOW};
use tokio::time::Instant;

use crate::{client_ip::ClientIp, config::MagicCodeSends};

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

/// A bucket of `burst` tokens that refills one per `every`. It is kept as the instant the bucket
/// is full again, so it needs no timer.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    every: Duration,
    /// How far ahead of now `full_at` may run: the whole bucket.
    capacity: Duration,
    full_at: Option<Instant>,
}

impl TokenBucket {
    pub fn new(every: Duration, burst: u32) -> Self {
        Self {
            every,
            capacity: every * burst,
            full_at: None,
        }
    }

    pub fn try_acquire(&mut self, now: Instant) -> Result<(), Instant> {
        self.try_spend(1, now)
    }

    /// Takes `tokens` at once, or none and `Err(retry_at)`: when there are enough again.
    pub fn try_spend(&mut self, tokens: u32, now: Instant) -> Result<(), Instant> {
        let full_at = self.full_at.map_or(now, |at| at.max(now)) + self.every * tokens;
        if full_at > now + self.capacity {
            return Err(full_at - self.capacity);
        }
        self.full_at = Some(full_at);
        Ok(())
    }
}

/// `limit` accepted events in any `window`. A refused event is not counted; `acquire` can count
/// one past the limit.
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

    /// Counts an event even past the limit, for the caller that has decided it goes ahead.
    fn acquire(&mut self, now: Instant) {
        self.prune(now);
        self.events.push_back(now);
    }

    /// When the window has room again: `now` if it has room.
    pub fn retry_at(&self, now: Instant) -> Instant {
        // Events are in time order: room comes when the `limit`-th newest leaves the window.
        self.events
            .len()
            .checked_sub(self.limit)
            .and_then(|at| self.events.get(at))
            .map_or(now, |t| (*t + self.window).max(now))
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

/// One sliding window per key, each of `limit` events in `span`.
struct Windows<K> {
    limit: usize,
    span: Duration,
    windows: HashMap<K, SlidingWindow>,
}

impl<K: Eq + Hash> Windows<K> {
    fn new(limit: u32, span: Duration) -> Self {
        Self {
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
            span,
            windows: HashMap::new(),
        }
    }

    /// When `key`'s window has room again: `now` if it has room.
    fn retry_at(&self, key: &K, now: Instant) -> Instant {
        self.windows.get(key).map_or(now, |w| w.retry_at(now))
    }

    /// Counts an event for `key`, whether or not its window has room.
    fn acquire(&mut self, key: K, now: Instant) {
        self.windows
            .entry(key)
            .or_insert_with(|| SlidingWindow::new(self.limit, self.span))
            .acquire(now);
    }

    /// Forgets the windows nothing is left in; how many are kept.
    fn prune(&mut self, now: Instant) -> usize {
        self.windows.retain(|_, w| !w.is_idle(now));
        self.windows.len()
    }
}

/// Who a requested Magic Code would reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipient {
    /// A Member, or the bootstrap email of a Lobby that has none yet.
    Member,
    Stranger,
}

/// What becomes of a request for a Magic Code that none of its client's caps stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeSend {
    Mail,
    /// Over a cap all clients share, for a [`Recipient::Stranger`]. Everything but the mail
    /// happens as usual, so no answer, now or later, tells who is a Member.
    Withhold,
}

/// The Magic Codes requested, under [`MagicCodeSends`]. A client's own caps (per email and
/// client, per client) hold for every email and count every request. The caps all clients share
/// (per mailbox, per Lobby) count the mail that goes out and never stop a
/// [`Recipient::Member`]'s, so strangers together cannot lock a Member out.
pub struct CodeSends {
    sent: Mutex<Sent>,
}

struct Sent {
    to_email_short: Windows<(Email, IpAddr)>,
    to_email_daily: Windows<(Email, IpAddr)>,
    by_client: Windows<IpAddr>,
    by_mailbox: Windows<Email>,
    in_all: Windows<()>,
}

impl CodeSends {
    pub fn new(caps: MagicCodeSends) -> Self {
        Self {
            sent: Mutex::new(Sent {
                to_email_short: Windows::new(caps.per_email_per_10m, MAGIC_CODE_SEND_WINDOW),
                to_email_daily: Windows::new(caps.per_email_per_24h, CODE_SEND_DAY),
                by_client: Windows::new(caps.per_client_per_24h, CODE_SEND_DAY),
                by_mailbox: Windows::new(caps.per_mailbox_per_24h, CODE_SEND_DAY),
                in_all: Windows::new(caps.per_lobby_per_24h, CODE_SEND_DAY),
            }),
        }
    }

    /// One request for a code to `email`. `Err(retry_at)` while one of the client's caps is
    /// full; a refused request leaves no state behind.
    pub fn try_acquire(
        &self,
        email: &Email,
        client: ClientIp,
        recipient: Recipient,
        now: Instant,
    ) -> Result<CodeSend, Instant> {
        let site = client.site_key();
        let to_email = (email.clone(), site);
        let mailbox = email.without_tag();
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        let retry_at = sent
            .to_email_short
            .retry_at(&to_email, now)
            .max(sent.to_email_daily.retry_at(&to_email, now))
            .max(sent.by_client.retry_at(&site, now));
        if retry_at > now {
            return Err(retry_at);
        }
        sent.to_email_short.acquire(to_email.clone(), now);
        sent.to_email_daily.acquire(to_email, now);
        sent.by_client.acquire(site, now);
        let shared_full =
            sent.by_mailbox.retry_at(&mailbox, now) > now || sent.in_all.retry_at(&(), now) > now;
        if shared_full && recipient == Recipient::Stranger {
            return Ok(CodeSend::Withhold);
        }
        sent.by_mailbox.acquire(mailbox, now);
        sent.in_all.acquire((), now);
        Ok(CodeSend::Mail)
    }

    pub fn prune(&self, now: Instant) -> usize {
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        sent.to_email_short.prune(now)
            + sent.to_email_daily.prune(now)
            + sent.by_client.prune(now)
            + sent.by_mailbox.prune(now)
            + sent.in_all.prune(now)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn a_withheld_code_counts_for_its_client_so_no_answer_tells_who_is_a_member() {
        let caps = MagicCodeSends {
            per_lobby_per_24h: 1,
            ..Config::default().auth.magic_code_sends
        };
        let per_email = caps.per_email_per_10m;
        let sends = CodeSends::new(caps);
        let (client, now) = (ClientIp::this_machine(), Instant::now());
        let email = |n: u32| Email::parse(&format!("a{n}@lobby.test")).unwrap();
        let ask = |n: u32, who: Recipient| sends.try_acquire(&email(n), client, who, now);
        let mail = Ok(CodeSend::Mail);
        assert_eq!(ask(0, Recipient::Stranger), mail);
        for _ in 0..per_email {
            assert_eq!(ask(1, Recipient::Stranger), Ok(CodeSend::Withhold));
        }
        assert!(ask(1, Recipient::Stranger).is_err());
        for _ in 0..per_email {
            assert_eq!(ask(2, Recipient::Member), mail);
        }
        assert!(ask(2, Recipient::Member).is_err());
    }
}

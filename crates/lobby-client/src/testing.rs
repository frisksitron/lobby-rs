//! Test doubles (feature `test-support`). Exempt from the no-`pub async fn` rule: tests run
//! these on Tokio.
//!
//! - [`inspect`] reads a running Session between two of its driver's rounds: what only a test
//!   asks of the media.
//! - [`fake_connection`] is a [`Connection`](crate::Connection) with no actor: a test injects
//!   events and reads the frames the Session sent, synchronously.
//! - [`FakeLobby`] is a scripted Lobby on loopback that records what the client did (close
//!   codes, refreshes, uploads), which the real server cannot observe.
//! - [`FakeReleases`] is a release feed on loopback for the updater.
//! - [`ChaosProxy`] sits between a client and a real server and breaks the path on demand: it
//!   freezes connections (a half-open socket, as after a pulled cable or a sleep) or cuts them.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use lobby_protocol::{AccessToken, RefreshToken, SessionTokens, UnixMs};
use tokio::sync::watch;

use crate::{Client, Clock, HttpPolicy, ReconnectPolicy, UiEffects};

mod chaos;
mod fake_conn;
mod fake_lobby;
mod fake_releases;
pub mod frames;
mod http;
mod stamp;

pub use chaos::ChaosProxy;
pub use fake_conn::{FakeConn, fake_connection};
pub use fake_lobby::{FakeBehavior, FakeLobby, LobbyInfoReply, Observed, RefreshReply};
pub use fake_releases::FakeReleases;
pub use stamp::{ScrollSource, Stamp, busy_region, luma_psnr, luma_psnr_in, read_stamp};

pub use crate::media::pattern::unix_micros;
pub use crate::media::{CaptureChain, CodecLoop, Onset, PlayedBlock, PulseShape, Pulses};
pub use crate::session::Session;

const WAIT: Duration = Duration::from_secs(5);

/// A clock a test moves by hand: what lives by it waits until it is moved past its deadline.
pub struct ManualClock {
    base: Instant,
    offset: watch::Sender<Duration>,
}

impl ManualClock {
    pub fn new() -> Arc<ManualClock> {
        Arc::new(ManualClock {
            base: Instant::now(),
            offset: watch::Sender::new(Duration::ZERO),
        })
    }

    pub fn advance(&self, by: Duration) {
        self.offset.send_modify(|offset| *offset += by);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.borrow()
    }

    fn sleep_until(&self, at: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let (base, mut offset) = (self.base, self.offset.subscribe());
        Box::pin(async move {
            while base + *offset.borrow_and_update() < at {
                if offset.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        })
    }
}

pub fn tokens_for_tests(tag: &str) -> SessionTokens {
    SessionTokens {
        access_token: AccessToken::new(format!("access-{tag}")),
        access_expires_at: UnixMs::now().saturating_add(Duration::from_secs(600)),
        refresh_token: RefreshToken::new(format!("refresh-{tag}")),
        refresh_expires_at: UnixMs::now().saturating_add(Duration::from_secs(86_400)),
    }
}

/// A Session whose WebSocket is a [`FakeConn`] and whose REST and media are real, run by its
/// driver on the runtime's clock: REST goes to `lobby`, signed in with test tokens, and the
/// events the test emits on the [`FakeConn`] reach it as the connection's would.
pub fn scripted_handle(lobby: &FakeLobby) -> (crate::SessionHandle, crate::Updates, FakeConn) {
    use crate::TokenStore as _;
    let store = crate::MemoryTokenStore::new();
    let origin = lobby.origin();
    store
        .save(&origin, &tokens_for_tests("scripted"))
        .expect("memory store");
    let (conn, mut fake) = fake_connection();
    let client = test_client(&store);
    let session = client.session_over(&origin, conn, fake.sender());
    let (handle, updates) = client.drive(session, fake.take_events());
    (handle, updates, fake)
}

/// Dispatches `intent` and returns its number with what the Session raised as it took it: its
/// refusal is an Issue among them. They come out with the updates as well.
pub async fn act(handle: &mut crate::SessionHandle, intent: crate::Intent) -> (u64, UiEffects) {
    let (n, own) = handle.act(intent);
    (n, own.await.expect("the Session's driver is running"))
}

/// `read` of the running Session, after every Intent dispatched before it and with what came
/// out before it published.
pub async fn inspect<T: Send + 'static>(
    handle: &crate::SessionHandle,
    read: impl FnOnce(&Session) -> T + Send + 'static,
) -> T {
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle.inspect(Box::new(move |s| {
        let _ = tx.send(read(s));
    }));
    rx.await.expect("the Session's driver is running")
}

pub fn test_client(store: &crate::MemoryTokenStore) -> Client {
    Client::new(
        tokio::runtime::Handle::current(),
        Arc::new(store.clone()),
        HttpPolicy::LoopbackOnly,
    )
    .expect("client builds")
}

/// The default policy's shape with every wait 1000 times shorter (2, 5, 10, 30 ms), no jitter.
pub fn fast_reconnects() -> ReconnectPolicy {
    ReconnectPolicy {
        tiers: [2, 5, 10, 30].map(Duration::from_millis),
        max_attempts: 10,
        jitter: 0.0,
    }
}

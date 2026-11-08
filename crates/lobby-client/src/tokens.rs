//! Per-Lobby tokens. The OS keychain in production; memory in tests and with
//! `LOBBY_INSECURE_TOKENS=1`.

use std::{
    collections::HashMap,
    future::Future,
    panic::AssertUnwindSafe,
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use lobby_protocol::SessionTokens;
use sha2::{Digest as _, Sha256};
use tokio::sync::oneshot;

use crate::LobbyOrigin;

/// A keychain call slower than this counts as unavailable.
const STORE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenStoreError {
    #[error("Secure token storage unavailable: {0}")]
    Unavailable(String),
}

/// Synchronous; it runs on the thread of one [`Keychain`].
pub trait TokenStore: Send + Sync + 'static {
    fn load(&self, origin: &LobbyOrigin) -> Result<Option<SessionTokens>, TokenStoreError>;
    fn save(&self, origin: &LobbyOrigin, tokens: &SessionTokens) -> Result<(), TokenStoreError>;
    fn clear(&self, origin: &LobbyOrigin) -> Result<(), TokenStoreError>;
}

/// Windows Credential Manager (and the platform equivalents): one entry per Lobby origin under
/// the service `lobby-rs/{profile}`, so two dev instances never share or overwrite tokens.
pub struct KeyringTokenStore {
    service: String,
}

impl KeyringTokenStore {
    pub fn new(profile: &str) -> Self {
        Self {
            service: format!("lobby-rs/{profile}"),
        }
    }

    /// `LOBBY_INSTANCE_ID` if set, else the first 12 hex of `sha256(data_dir)` when
    /// `LOBBY_USER_DATA_DIR` is set, else `default`.
    pub fn profile(instance_id: Option<&str>, user_data_dir: Option<&Path>) -> String {
        if let Some(id) = instance_id {
            return id.to_owned();
        }
        if let Some(dir) = user_data_dir {
            let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
            return digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
        }
        "default".to_owned()
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, origin: &LobbyOrigin) -> Result<keyring::Entry, TokenStoreError> {
        keyring::Entry::new(&self.service, origin.as_str()).map_err(unavailable)
    }
}

fn unavailable(e: keyring::Error) -> TokenStoreError {
    TokenStoreError::Unavailable(e.to_string())
}

impl TokenStore for KeyringTokenStore {
    fn load(&self, origin: &LobbyOrigin) -> Result<Option<SessionTokens>, TokenStoreError> {
        match self.entry(origin)?.get_password() {
            Ok(json) => serde_json::from_str(&json).map(Some).map_err(|e| {
                TokenStoreError::Unavailable(format!("stored tokens are unreadable: {e}"))
            }),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn save(&self, origin: &LobbyOrigin, tokens: &SessionTokens) -> Result<(), TokenStoreError> {
        let json = serde_json::to_string(tokens).expect("tokens serialize");
        self.entry(origin)?.set_password(&json).map_err(unavailable)
    }

    fn clear(&self, origin: &LobbyOrigin) -> Result<(), TokenStoreError> {
        match self.entry(origin)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(unavailable(e)),
        }
    }
}

/// In-process tokens. With `fail` set every call errors, to test that a keychain failure never
/// leaves tokens in memory only. With saves held, a save waits as a slow keychain does.
#[derive(Default, Clone)]
pub struct MemoryTokenStore {
    tokens: Arc<Mutex<HashMap<LobbyOrigin, SessionTokens>>>,
    fail: Arc<AtomicBool>,
    held: Arc<(Mutex<bool>, Condvar)>,
    saves_waiting: Arc<AtomicUsize>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_failing(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    /// While held, a save waits inside the keychain call until it is let go.
    pub fn hold_saves(&self, held: bool) {
        let (lock, changed) = &*self.held;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = held;
        changed.notify_all();
    }

    /// How many saves have begun and not yet returned, held or not.
    pub fn saves_waiting(&self) -> usize {
        self.saves_waiting.load(Ordering::SeqCst)
    }

    fn wait_while_held(&self) {
        let (lock, changed) = &*self.held;
        let mut held = lock.lock().unwrap_or_else(|e| e.into_inner());
        while *held {
            held = changed.wait(held).unwrap_or_else(|e| e.into_inner());
        }
    }

    pub fn peek(&self, origin: &LobbyOrigin) -> Option<SessionTokens> {
        self.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(origin)
            .cloned()
    }

    fn check(&self) -> Result<(), TokenStoreError> {
        if self.fail.load(Ordering::SeqCst) {
            Err(TokenStoreError::Unavailable("injected failure".into()))
        } else {
            Ok(())
        }
    }
}

impl TokenStore for MemoryTokenStore {
    fn load(&self, origin: &LobbyOrigin) -> Result<Option<SessionTokens>, TokenStoreError> {
        self.check()?;
        Ok(self.peek(origin))
    }

    fn save(&self, origin: &LobbyOrigin, tokens: &SessionTokens) -> Result<(), TokenStoreError> {
        self.saves_waiting.fetch_add(1, Ordering::SeqCst);
        self.wait_while_held();
        let saved = self.check().map(|()| {
            self.tokens
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(origin.clone(), tokens.clone());
        });
        self.saves_waiting.fetch_sub(1, Ordering::SeqCst);
        saved
    }

    fn clear(&self, origin: &LobbyOrigin) -> Result<(), TokenStoreError> {
        self.check()?;
        self.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(origin);
        Ok(())
    }
}

type Call = Box<dyn FnOnce(&dyn TokenStore) + Send>;

/// The token store in one order. A call takes its place in line when it is made, before anything
/// is awaited, and one thread runs the calls in that order, each to its end even when its caller
/// stopped waiting: a keychain call that runs cannot be stopped, so without the order a clear
/// could land before a save made earlier, and a Member who signed out would be signed in again.
/// The thread ends once every clone is dropped and the calls in line have run.
#[derive(Clone)]
pub(crate) struct Keychain {
    line: mpsc::Sender<Call>,
    /// Calls made that have not run to their end.
    in_line: Arc<AtomicUsize>,
    timeout: Duration,
}

impl Keychain {
    pub fn new(store: Arc<dyn TokenStore>) -> Self {
        let (line, calls) = mpsc::channel::<Call>();
        let in_line = Arc::new(AtomicUsize::new(0));
        let ran = in_line.clone();
        std::thread::Builder::new()
            .name("lobby-keychain".into())
            .spawn(move || {
                for call in calls {
                    // A call that panics fails alone; its caller hears "the keychain call failed".
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| call(store.as_ref())));
                    ran.fetch_sub(1, Ordering::SeqCst);
                }
            })
            .expect("the keychain thread starts");
        Self {
            line,
            in_line,
            timeout: STORE_TIMEOUT,
        }
    }

    /// How long a caller waits before it counts the keychain as unavailable.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn load(
        &self,
        origin: &LobbyOrigin,
    ) -> impl Future<Output = Result<Option<SessionTokens>, TokenStoreError>> + Send + 'static {
        let origin = origin.clone();
        self.call(move |s| s.load(&origin))
    }

    pub fn save(
        &self,
        origin: &LobbyOrigin,
        tokens: &SessionTokens,
    ) -> impl Future<Output = Result<(), TokenStoreError>> + Send + 'static {
        let (origin, tokens) = (origin.clone(), tokens.clone());
        self.call(move |s| s.save(&origin, &tokens))
    }

    /// Dropping the answer unheard does not take the clear back.
    pub fn clear(
        &self,
        origin: &LobbyOrigin,
    ) -> impl Future<Output = Result<(), TokenStoreError>> + Send + 'static {
        let origin = origin.clone();
        self.call(move |s| s.clear(&origin))
    }

    /// Done once every call made before it has run, however long they take; `None` when every
    /// one has already.
    pub fn settled(&self) -> Option<impl Future<Output = ()> + Send + 'static> {
        if self.in_line.load(Ordering::SeqCst) == 0 {
            return None;
        }
        let ran = self.queue(|_| Ok(()));
        Some(async move {
            let _ = ran.await;
        })
    }

    /// A thread that is gone drops the call, and the answer says it failed.
    fn queue<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn TokenStore) -> Result<T, TokenStoreError> + Send + 'static,
    ) -> oneshot::Receiver<Result<T, TokenStoreError>> {
        let (answer, answered) = oneshot::channel();
        self.in_line.fetch_add(1, Ordering::SeqCst);
        let call = Box::new(move |store: &dyn TokenStore| {
            let _ = answer.send(f(store));
        });
        if self.line.send(call).is_err() {
            self.in_line.fetch_sub(1, Ordering::SeqCst);
        }
        answered
    }

    fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn TokenStore) -> Result<T, TokenStoreError> + Send + 'static,
    ) -> impl Future<Output = Result<T, TokenStoreError>> + Send + 'static {
        let answered = self.queue(f);
        let timeout = self.timeout;
        async move {
            match tokio::time::timeout(timeout, answered).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(TokenStoreError::Unavailable(
                    "the keychain call failed".into(),
                )),
                Err(_) => Err(TokenStoreError::Unavailable(
                    "the keychain did not answer".into(),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpPolicy, testing::tokens_for_tests};

    /// A call's place in line is the moment it is made. Entry makes a clear and then a save while
    /// an older save holds the keychain, and their callers can be polled the other way round.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn calls_made_behind_a_held_save_land_in_the_order_they_were_made() {
        let store = MemoryTokenStore::new();
        let keychain = Keychain::new(Arc::new(store.clone()));
        let origin = LobbyOrigin::parse("http://127.0.0.1:1", HttpPolicy::LoopbackOnly).unwrap();
        store.hold_saves(true);
        let held = {
            let (keychain, origin) = (keychain.clone(), origin.clone());
            tokio::spawn(async move { keychain.save(&origin, &tokens_for_tests("old")).await })
        };
        while store.saves_waiting() == 0 {
            tokio::task::yield_now().await;
        }
        let newest = tokens_for_tests("new");
        let clear = keychain.clear(&origin);
        let save = keychain.save(&origin, &newest);
        store.hold_saves(false);
        save.await.expect("the newer save");
        clear.await.expect("the clear");
        held.await.unwrap().expect("the held save");
        assert_eq!(
            store.peek(&origin),
            Some(newest),
            "the newer save lands after the clear made before it"
        );
    }
}

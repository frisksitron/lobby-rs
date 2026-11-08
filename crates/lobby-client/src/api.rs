//! REST. Crate-private: the desktop reaches it only through `Client`, Entry and the `Session`,
//! which spawn on the client's runtime.
//!
//! `ApiClient`'s token mutex makes refresh single-flight: a refresh happens while it is held,
//! so concurrent callers wait and then see the fresh pair. A new pair is written to the
//! token store *before* it replaces the old one in memory or is used (persist before use), and
//! tokens are cleared only when the refresh endpoint says 401/403.

use std::{error::Error as _, future::Future, io, pin::Pin, sync::Arc, time::Duration};

use lobby_protocol::{
    AccessRequestId, AccessToken, Attachment, ErrorCode, ErrorDetail, ErrorEnvelope, JoinOutcome,
    JoinRequest, JoinStatus, JoinStatusRequest, LobbyInfo, MagicCodeRequest, MagicCodeRequested,
    MagicCodeVerify, MessageChanges, MessagePage, OwnerTransfer, PROTOCOL_VERSION, RefreshRequest,
    SessionTokens, SignOutRequest, UnixMs, User, UserId, Username, UsernameChange, VerifyOutcome,
    routes::{self, ChangesQuery, HistoryQuery, UploadQuery},
};
use reqwest::{
    Method, RequestBuilder, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_TYPE},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _},
    sync::Mutex,
};
use tracing::{info, warn};

use crate::{
    LobbyOrigin, copy,
    drafts::{DraftSource, UploadError},
    tokens::{Keychain, TokenStoreError},
};

pub(crate) const JSON_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const UPLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const UPLOAD_CHUNK: usize = 64 * 1024;

pub(crate) type Progress = Arc<dyn Fn(u64) + Send + Sync>;
/// A token this close to expiry could lapse on its way to the server.
pub(crate) const REFRESH_BEFORE_USE: Duration = Duration::from_secs(60);
/// Sign Out is local-first: the server gets this long, then the device signs out anyway.
pub(crate) const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    pub status: Option<u16>,
    pub kind: ApiKind,
    pub message: String,
    /// On a 429: how long until the Lobby takes the next try (`retryAfterMs`, relative, so a
    /// skewed clock does not matter).
    pub retry_after: Option<Duration>,
    pub detail: Option<ErrorDetail>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKind {
    Server(ErrorCode),
    Network,
    Timeout,
    Decode,
    SessionExpired,
    /// The token store failed; tokens are never kept in memory only.
    KeychainUnavailable,
}

impl ApiError {
    fn new(status: Option<u16>, kind: ApiKind, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            message: message.into(),
            retry_after: None,
            detail: None,
        }
    }

    fn session_expired() -> Self {
        Self::new(None, ApiKind::SessionExpired, copy::SESSION_EXPIRED)
    }

    fn keychain(e: &TokenStoreError) -> Self {
        warn!(error = %e, "token store failed");
        Self::new(
            None,
            ApiKind::KeychainUnavailable,
            copy::KEYCHAIN_UNAVAILABLE,
        )
    }

    pub fn is_retryable(&self) -> bool {
        match self.kind {
            ApiKind::Network | ApiKind::Timeout => true,
            ApiKind::Server(ErrorCode::RateLimited) => true,
            ApiKind::Server(_) => self.status.is_some_and(|s| s >= 500),
            ApiKind::Decode | ApiKind::SessionExpired | ApiKind::KeychainUnavailable => false,
        }
    }

    /// The user sees fixed copy; the cause (DNS, TLS, refused, …) goes to the support log here,
    /// in the caller's span, since `ApiError` does not keep it.
    pub(crate) fn from_reqwest(e: &reqwest::Error) -> Self {
        warn!(error = %error_chain(e), "request failed");
        let status = e.status().map(|s| s.as_u16());
        if e.is_timeout() {
            Self::new(status, ApiKind::Timeout, "The request timed out")
        } else if e.is_decode() {
            Self::new(
                status,
                ApiKind::Decode,
                "Unexpected response from the Lobby",
            )
        } else {
            Self::new(status, ApiKind::Network, copy::NETWORK_ERROR)
        }
    }

    pub(crate) fn from_error_response(status: StatusCode, body: &[u8]) -> Self {
        match serde_json::from_slice::<ErrorEnvelope>(body) {
            Ok(ErrorEnvelope { error }) => Self {
                status: Some(status.as_u16()),
                kind: ApiKind::Server(error.code),
                message: copy::server_error(error.code, &error.message),
                retry_after: error.retry_after_ms.map(Duration::from_millis),
                detail: error.detail,
            },
            Err(_) => Self::new(
                Some(status.as_u16()),
                ApiKind::Decode,
                format!("Unexpected response from the Lobby (HTTP {status})"),
            ),
        }
    }
}

/// Why `GET /lobby` did not reach a Lobby this client can talk to. `Display` is the user copy;
/// the cause is logged where it happened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LobbyInfoError {
    #[error("{}", copy::FAILED_TO_CONNECT)]
    Api(#[source] ApiError),
    #[error("{}", copy::PROTOCOL_MISMATCH)]
    ProtocolMismatch { server: u32 },
}

pub(crate) fn check_protocol(info: LobbyInfo) -> Result<LobbyInfo, LobbyInfoError> {
    if info.protocol_version == PROTOCOL_VERSION {
        Ok(info)
    } else {
        warn!(
            server = info.protocol_version,
            client = PROTOCOL_VERSION,
            "protocol mismatch"
        );
        Err(LobbyInfoError::ProtocolMismatch {
            server: info.protocol_version,
        })
    }
}

/// `e` and its sources: reqwest's own message names only the URL.
fn error_chain(e: &reqwest::Error) -> String {
    let mut chain = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    chain
}

/// An error [`ApiClient::authed`] can tell a refused access token from: the plain API error and
/// an upload's.
trait TokenRefused: From<ApiError> {
    fn token_refused(&self) -> bool;
}

impl TokenRefused for ApiError {
    fn token_refused(&self) -> bool {
        self.status == Some(401)
    }
}

impl From<ApiError> for UploadError {
    fn from(e: ApiError) -> Self {
        UploadError::Api(e)
    }
}

impl TokenRefused for UploadError {
    fn token_refused(&self) -> bool {
        matches!(self, UploadError::Api(e) if e.token_refused())
    }
}

async fn json<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, ApiError> {
    let res = req
        .timeout(JSON_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| ApiError::from_reqwest(&e))?;
    let status = res.status();
    let body = res.bytes().await.map_err(|e| ApiError::from_reqwest(&e))?;
    if !status.is_success() {
        return Err(ApiError::from_error_response(status, &body));
    }
    serde_json::from_slice(&body).map_err(|_| {
        ApiError::new(
            Some(status.as_u16()),
            ApiKind::Decode,
            "Unexpected response from the Lobby",
        )
    })
}

async fn empty(req: RequestBuilder) -> Result<(), ApiError> {
    let res = req
        .timeout(JSON_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| ApiError::from_reqwest(&e))?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let body = res.bytes().await.map_err(|e| ApiError::from_reqwest(&e))?;
    Err(ApiError::from_error_response(status, &body))
}

pub(crate) async fn request_magic_code(
    http: &reqwest::Client,
    origin: &LobbyOrigin,
    body: MagicCodeRequest,
) -> Result<MagicCodeRequested, ApiError> {
    json(http.post(origin.http_url(routes::MAGIC_CODE)).json(&body)).await
}

pub(crate) async fn verify_magic_code(
    http: &reqwest::Client,
    origin: &LobbyOrigin,
    body: MagicCodeVerify,
) -> Result<VerifyOutcome, ApiError> {
    json(
        http.post(origin.http_url(routes::MAGIC_CODE_VERIFY))
            .json(&body),
    )
    .await
}

pub(crate) async fn join(
    http: &reqwest::Client,
    origin: &LobbyOrigin,
    body: JoinRequest,
) -> Result<JoinOutcome, ApiError> {
    json(http.post(origin.http_url(routes::JOIN)).json(&body)).await
}

pub(crate) async fn join_status(
    http: &reqwest::Client,
    origin: &LobbyOrigin,
    body: JoinStatusRequest,
) -> Result<JoinStatus, ApiError> {
    json(http.post(origin.http_url(routes::JOIN_STATUS)).json(&body)).await
}

pub(crate) async fn lobby_info(
    http: &reqwest::Client,
    origin: &LobbyOrigin,
) -> Result<LobbyInfo, ApiError> {
    json(http.get(origin.http_url(routes::LOBBY))).await
}

pub(crate) async fn checked_lobby_info(
    http: &reqwest::Client,
    origin: &LobbyOrigin,
) -> Result<LobbyInfo, LobbyInfoError> {
    let info = lobby_info(http, origin)
        .await
        .map_err(LobbyInfoError::Api)?;
    check_protocol(info)
}

enum Slot {
    Unloaded,
    Loaded(Option<SessionTokens>),
}

pub(crate) struct ApiClient {
    http: reqwest::Client,
    origin: LobbyOrigin,
    keychain: Keychain,
    tokens: Mutex<Slot>,
}

impl ApiClient {
    pub fn new(http: reqwest::Client, origin: LobbyOrigin, keychain: Keychain) -> Self {
        Self {
            http,
            origin,
            keychain,
            tokens: Mutex::new(Slot::Unloaded),
        }
    }

    /// With the tokens the caller just read from the store, so the first call does not read them
    /// again.
    pub fn with_tokens(
        http: reqwest::Client,
        origin: LobbyOrigin,
        keychain: Keychain,
        tokens: SessionTokens,
    ) -> Self {
        Self {
            http,
            origin,
            keychain,
            tokens: Mutex::new(Slot::Loaded(Some(tokens))),
        }
    }

    pub fn origin(&self) -> &LobbyOrigin {
        &self.origin
    }

    async fn current<'a>(
        &self,
        slot: &'a mut Slot,
    ) -> Result<&'a mut Option<SessionTokens>, ApiError> {
        if let Slot::Unloaded = slot {
            let stored = self
                .keychain
                .load(&self.origin)
                .await
                .map_err(|e| ApiError::keychain(&e))?;
            *slot = Slot::Loaded(stored);
        }
        match slot {
            Slot::Loaded(tokens) => Ok(tokens),
            Slot::Unloaded => unreachable!("loaded above"),
        }
    }

    pub async fn access_token(&self) -> Result<(AccessToken, UnixMs), ApiError> {
        self.fresh_within(REFRESH_BEFORE_USE).await
    }

    pub async fn fresh_within(&self, lead: Duration) -> Result<(AccessToken, UnixMs), ApiError> {
        let mut slot = self.tokens.lock().await;
        let tokens = self.current(&mut slot).await?;
        let Some(current) = tokens.as_ref() else {
            return Err(ApiError::session_expired());
        };
        if current.access_expires_at > UnixMs::now().saturating_add(lead) {
            return Ok((current.access_token.clone(), current.access_expires_at));
        }
        let fresh = self.refresh_locked(tokens).await?;
        Ok((fresh.access_token, fresh.access_expires_at))
    }

    /// After a 401 on `stale`: refresh, unless another caller already replaced it.
    pub async fn force_refresh(&self, stale: &AccessToken) -> Result<AccessToken, ApiError> {
        let mut slot = self.tokens.lock().await;
        let tokens = self.current(&mut slot).await?;
        match tokens.as_ref() {
            None => Err(ApiError::session_expired()),
            Some(current) if current.access_token != *stale => Ok(current.access_token.clone()),
            Some(_) => Ok(self.refresh_locked(tokens).await?.access_token),
        }
    }

    pub async fn access_expires_at(&self) -> Option<UnixMs> {
        let mut slot = self.tokens.lock().await;
        let tokens = self.current(&mut slot).await.ok()?;
        tokens.as_ref().map(|t| t.access_expires_at)
    }

    pub async fn clear(&self) {
        let mut slot = self.tokens.lock().await;
        *slot = Slot::Loaded(None);
        if let Err(e) = self.keychain.clear(&self.origin).await {
            warn!(error = %e, "could not clear stored tokens");
        }
    }

    /// Refreshes with the mutex held. The new pair is persisted before it replaces
    /// the old one; 401/403 clears both (the only case that does).
    async fn refresh_locked(
        &self,
        tokens: &mut Option<SessionTokens>,
    ) -> Result<SessionTokens, ApiError> {
        let Some(current) = tokens.as_ref() else {
            return Err(ApiError::session_expired());
        };
        let body = RefreshRequest {
            refresh_token: current.refresh_token.clone(),
        };
        let result: Result<SessionTokens, ApiError> = json(
            self.http
                .post(self.origin.http_url(routes::REFRESH))
                .json(&body),
        )
        .await;
        match result {
            Ok(fresh) => {
                self.keychain
                    .save(&self.origin, &fresh)
                    .await
                    .map_err(|e| ApiError::keychain(&e))?;
                info!(
                    access_expires_at = fresh.access_expires_at.0,
                    "tokens refreshed"
                );
                *tokens = Some(fresh.clone());
                Ok(fresh)
            }
            Err(e) if matches!(e.status, Some(401 | 403)) => {
                warn!(error = %e, "refresh refused: signed out of this Lobby");
                *tokens = None;
                if let Err(e) = self.keychain.clear(&self.origin).await {
                    warn!(error = %e, "could not clear stored tokens");
                }
                Err(ApiError::session_expired())
            }
            Err(e) => Err(e),
        }
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &impl Serialize,
    ) -> Result<T, ApiError> {
        let url = self.origin.http_url(path);
        self.authed(|token| json(self.http.get(&url).query(query).bearer_auth(token.expose())))
            .await
    }

    async fn post_empty<B: Serialize>(&self, path: &str, body: &B) -> Result<(), ApiError> {
        let url = self.origin.http_url(path);
        self.authed(|token| empty(self.http.post(&url).bearer_auth(token.expose()).json(body)))
            .await
    }

    /// `send` with a fresh access token; a 401 means that token lapsed on its way, so it is
    /// refreshed and `send` runs once more. A second 401 ends the session.
    async fn authed<T, E, Fut>(&self, send: impl Fn(&AccessToken) -> Fut) -> Result<T, E>
    where
        E: TokenRefused,
        Fut: Future<Output = Result<T, E>>,
    {
        let (token, _) = self.access_token().await?;
        match send(&token).await {
            Err(e) if e.token_refused() => {
                let token = self.force_refresh(&token).await?;
                send(&token).await.map_err(|e| {
                    if e.token_refused() {
                        ApiError::session_expired().into()
                    } else {
                        e
                    }
                })
            }
            other => other,
        }
    }

    pub async fn rename(&self, username: &Username) -> Result<User, ApiError> {
        let body = UsernameChange {
            username: username.clone(),
        };
        let url = self.origin.http_url(routes::USERS_ME);
        self.authed(|token| {
            json(
                self.http
                    .patch(&url)
                    .bearer_auth(token.expose())
                    .json(&body),
            )
        })
        .await
    }

    pub async fn vouch(&self, id: &AccessRequestId) -> Result<(), ApiError> {
        self.post_empty(&routes::vouch(id), &serde_json::json!({}))
            .await
    }

    pub async fn decline(&self, id: &AccessRequestId) -> Result<(), ApiError> {
        self.post_empty(&routes::decline(id), &serde_json::json!({}))
            .await
    }

    pub async fn remove_member(&self, user: &UserId) -> Result<(), ApiError> {
        self.post_empty(&routes::remove_member(user), &serde_json::json!({}))
            .await
    }

    pub async fn transfer_ownership(&self, user: &UserId) -> Result<(), ApiError> {
        let body = OwnerTransfer {
            user_id: user.clone(),
        };
        self.post_empty(routes::LOBBY_OWNER, &body).await
    }

    /// The first half of Sign Out, local-first: takes a usable access token (at most
    /// [`SIGN_OUT_TIMEOUT`]), then clears this Lobby's tokens, in memory and in the store.
    pub async fn forget(&self) -> Option<AccessToken> {
        let token = tokio::time::timeout(SIGN_OUT_TIMEOUT, self.access_token())
            .await
            .ok()
            .and_then(Result::ok)
            .map(|(token, _)| token);
        self.clear().await;
        token
    }

    /// The second half: the Lobby is told with `token`, if it answers within
    /// [`SIGN_OUT_TIMEOUT`]. A Lobby that never hears of it keeps a session nobody holds a token
    /// for.
    pub async fn tell_signed_out(&self, token: Option<AccessToken>, everywhere: bool) {
        let Some(token) = token else {
            warn!("signed out here; no usable token to tell the Lobby");
            return;
        };
        let told = tokio::time::timeout(
            SIGN_OUT_TIMEOUT,
            empty(
                self.http
                    .post(self.origin.http_url(routes::SIGN_OUT))
                    .bearer_auth(token.expose())
                    .json(&SignOutRequest { everywhere }),
            ),
        )
        .await;
        match told {
            Ok(Ok(())) => info!(everywhere, "signed out"),
            Ok(Err(e)) => warn!(error = %e, "sign-out not confirmed by the Lobby; signed out here"),
            Err(_) => warn!("sign-out timed out; signed out here"),
        }
    }

    pub async fn messages(&self, page: Page, limit: u8) -> Result<MessagePage, ApiError> {
        let (before, after) = match page {
            Page::Before(id) => (Some(id), None),
            Page::After(id) => (None, Some(id)),
            Page::Latest => (None, None),
        };
        let query = HistoryQuery {
            limit: Some(limit),
            before,
            after,
        };
        self.get_json(routes::MESSAGES, &query).await
    }

    pub async fn message_changes(&self, after: u64) -> Result<MessageChanges, ApiError> {
        self.get_json(routes::MESSAGE_CHANGES, &ChangesQuery { after })
            .await
    }

    /// The raw bytes as the body, streamed in [`UPLOAD_CHUNK`]s with `progress` told the
    /// cumulative count. On 401 the token is refreshed
    /// once and the body replayed from its source (a file is opened again).
    pub async fn upload(
        &self,
        source: &DraftSource,
        progress: Progress,
    ) -> Result<Attachment, UploadError> {
        self.send_file(Method::POST, routes::UPLOADS, source, progress)
            .await
    }

    pub async fn set_avatar(&self, source: &DraftSource) -> Result<User, UploadError> {
        self.send_file(
            Method::PUT,
            routes::USERS_ME_AVATAR,
            source,
            Arc::new(|_| {}),
        )
        .await
    }

    /// Leave Lobby. Not retried after a 401: the Lobby ends this
    /// Member's sessions, so a second try is from someone who is gone.
    pub async fn leave(&self) -> Result<(), ApiError> {
        let url = self.origin.http_url(routes::USERS_ME);
        let (token, _) = self.access_token().await?;
        empty(self.http.delete(&url).bearer_auth(token.expose())).await
    }

    pub async fn set_icon(&self, source: &DraftSource) -> Result<LobbyInfo, UploadError> {
        self.send_file(Method::PUT, routes::LOBBY_ICON, source, Arc::new(|_| {}))
            .await
    }

    pub async fn remove_icon(&self) -> Result<LobbyInfo, ApiError> {
        let url = self.origin.http_url(routes::LOBBY_ICON);
        self.authed(|token| json(self.http.delete(&url).bearer_auth(token.expose())))
            .await
    }

    pub async fn remove_avatar(&self) -> Result<User, ApiError> {
        let url = self.origin.http_url(routes::USERS_ME_AVATAR);
        self.authed(|token| json(self.http.delete(&url).bearer_auth(token.expose())))
            .await
    }

    async fn send_file<T: DeserializeOwned>(
        &self,
        method: Method,
        route: &str,
        source: &DraftSource,
        progress: Progress,
    ) -> Result<T, UploadError> {
        let query = UploadQuery {
            name: Some(source.name()),
        };
        let url = self.origin.http_url(route);
        self.authed(|token| {
            self.send_file_once(&method, &url, &query, token.clone(), source, &progress)
        })
        .await
    }

    async fn send_file_once<T: DeserializeOwned>(
        &self,
        method: &Method,
        url: &str,
        query: &UploadQuery,
        token: AccessToken,
        source: &DraftSource,
        progress: &Progress,
    ) -> Result<T, UploadError> {
        let (body, len) = upload_body(source, progress.clone()).await?;
        let res = self
            .http
            .request(method.clone(), url)
            .query(query)
            .bearer_auth(token.expose())
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, len)
            .body(body)
            .timeout(UPLOAD_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| UploadError::Api(ApiError::from_reqwest(&e)))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| UploadError::Api(ApiError::from_reqwest(&e)))?;
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(UploadError::TooLarge);
        }
        if !status.is_success() {
            return Err(UploadError::Api(ApiError::from_error_response(
                status, &bytes,
            )));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            UploadError::Api(ApiError::new(
                Some(status.as_u16()),
                ApiKind::Decode,
                "Unexpected response from the Lobby",
            ))
        })
    }
}

/// The upload's body and its length: the source read in [`UPLOAD_CHUNK`]s, each reported to
/// `progress`. A file's length is that of the handle just opened, not of the stat taken when it
/// was attached, and the body never runs past it, so the declared length holds if the file
/// changed in between.
async fn upload_body(
    source: &DraftSource,
    progress: Progress,
) -> Result<(reqwest::Body, u64), UploadError> {
    let (reader, len): (Pin<Box<dyn AsyncRead + Send>>, u64) = match source {
        DraftSource::File { path, .. } => {
            let opened = async {
                let file = tokio::fs::File::open(path).await?;
                let len = file.metadata().await?.len();
                Ok::<_, io::Error>((file, len))
            };
            let (file, len) = opened.await.map_err(|e| UploadError::Io(e.to_string()))?;
            (Box::pin(file.take(len)), len)
        }
        DraftSource::Bytes { bytes, .. } => {
            (Box::pin(io::Cursor::new(bytes.clone())), bytes.len() as u64)
        }
    };
    let stream = futures_util::stream::try_unfold(
        (reader, 0u64, progress),
        |(mut reader, sent, progress)| async move {
            let mut buf = vec![0u8; UPLOAD_CHUNK];
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                return Ok::<_, io::Error>(None);
            }
            buf.truncate(n);
            let sent = sent + n as u64;
            progress(sent);
            Ok(Some((buf, (reader, sent, progress))))
        },
    );
    Ok((reqwest::Body::wrap_stream(stream), len))
}

/// The newest page comes in READY, which can miss what was committed while it was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Before(lobby_protocol::MessageId),
    After(lobby_protocol::MessageId),
    /// The newest page, for a READY whose page was empty while Messages up to its
    /// `lastMessageSeq` exist: there is no Message to fetch after.
    Latest,
}

#[cfg(test)]
mod tests {
    use lobby_protocol::RefreshToken;

    use super::*;
    use crate::{
        MemoryTokenStore, TokenStore as _,
        testing::{FakeBehavior, FakeLobby, Observed, RefreshReply},
    };

    fn expiring(tag: &str, access_in: Duration) -> SessionTokens {
        SessionTokens {
            access_token: AccessToken::new(format!("access-{tag}")),
            access_expires_at: UnixMs::now().saturating_add(access_in),
            refresh_token: RefreshToken::new(format!("refresh-{tag}")),
            refresh_expires_at: UnixMs::now().saturating_add(Duration::from_secs(86_400)),
        }
    }

    fn api_for(fake: &FakeLobby, store: &MemoryTokenStore) -> Arc<ApiClient> {
        Arc::new(ApiClient::new(
            reqwest::Client::new(),
            fake.origin(),
            Keychain::new(Arc::new(store.clone())),
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ten_concurrent_callers_cause_one_refresh() {
        let mut fake = FakeLobby::start(FakeBehavior::default()).await;
        let store = MemoryTokenStore::new();
        store
            .save(&fake.origin(), &expiring("old", Duration::from_secs(10)))
            .unwrap();
        let api = api_for(&fake, &store);
        let calls = (0..10).map(|_| {
            let api = api.clone();
            tokio::spawn(async move { api.access_token().await })
        });
        let tokens: Vec<_> = futures_util::future::join_all(calls)
            .await
            .into_iter()
            .map(|r| r.unwrap().unwrap().0)
            .collect();
        assert!(
            tokens.iter().all(|t| *t == tokens[0]),
            "everyone sees the fresh pair"
        );
        assert_ne!(tokens[0].expose(), "access-old");
        assert_eq!(fake.next_observed().await, Observed::RefreshRequested);
        assert_eq!(
            fake.observed_now(),
            Vec::<Observed>::new(),
            "exactly one refresh"
        );
        assert_eq!(store.peek(&fake.origin()).unwrap().access_token, tokens[0]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_401_or_403_from_refresh_clears_the_tokens() {
        for (reply, cleared) in [
            (RefreshReply::Status(503), false),
            (RefreshReply::Status(500), false),
            (RefreshReply::Status(401), true),
            (RefreshReply::Status(403), true),
        ] {
            let fake = FakeLobby::start(FakeBehavior {
                refresh: reply.clone(),
                ..FakeBehavior::default()
            })
            .await;
            let store = MemoryTokenStore::new();
            store
                .save(&fake.origin(), &expiring("old", Duration::from_secs(1)))
                .unwrap();
            let api = api_for(&fake, &store);
            let err = api.access_token().await.unwrap_err();
            assert_eq!(store.peek(&fake.origin()).is_none(), cleared, "{reply:?}");
            assert_eq!(
                err.kind == ApiKind::SessionExpired,
                cleared,
                "{reply:?}: {err:?}"
            );
        }
    }
}

//! Opaque tokens: 32 random bytes, base64url without padding; only their sha256 is stored.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use lobby_protocol::{AccessToken, RefreshToken, SessionTokens, UnixMs};
use sha2::{Digest as _, Sha256};

use crate::{config::AuthConfig, db::sessions::NewPair};

pub type Digest = [u8; 32];

pub fn digest(token: &str) -> Digest {
    Sha256::digest(token.as_bytes()).into()
}

fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn join_ticket() -> (lobby_protocol::JoinTicket, Digest) {
    let token = random_token();
    let hash = digest(&token);
    (lobby_protocol::JoinTicket::new(token), hash)
}

pub fn wait_token() -> (lobby_protocol::WaitToken, Digest) {
    let token = random_token();
    let hash = digest(&token);
    (lobby_protocol::WaitToken::new(token), hash)
}

pub fn magic_challenge() -> (lobby_protocol::MagicChallenge, Digest) {
    let token = random_token();
    let hash = digest(&token);
    (lobby_protocol::MagicChallenge::new(token), hash)
}

/// `sha256(email ":" code)`: a code is stored only as this.
pub fn code_digest(email: &lobby_protocol::Email, code: &str) -> Digest {
    let mut h = Sha256::new();
    h.update(email.as_str().as_bytes());
    h.update(b":");
    h.update(code.as_bytes());
    h.finalize().into()
}

/// Compares two digests in constant time.
pub fn same_digest(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct Minted {
    access: AccessToken,
    access_hash: Digest,
    access_expires_at: UnixMs,
    refresh: RefreshToken,
    refresh_hash: Digest,
    refresh_expires_at: UnixMs,
}

impl Minted {
    pub fn new(now: UnixMs, auth: &AuthConfig) -> Minted {
        let access = random_token();
        let refresh = random_token();
        Minted {
            access_hash: digest(&access),
            access: AccessToken::new(access),
            access_expires_at: now.saturating_add(auth.access_ttl),
            refresh_hash: digest(&refresh),
            refresh: RefreshToken::new(refresh),
            refresh_expires_at: now.saturating_add(auth.refresh_ttl),
        }
    }

    pub fn pair(&self) -> NewPair<'_> {
        NewPair {
            access_hash: &self.access_hash,
            access_expires_at: self.access_expires_at,
            refresh_hash: &self.refresh_hash,
            refresh_expires_at: self.refresh_expires_at,
        }
    }

    pub fn into_tokens(self) -> SessionTokens {
        SessionTokens {
            access_token: self.access,
            access_expires_at: self.access_expires_at,
            refresh_token: self.refresh,
            refresh_expires_at: self.refresh_expires_at,
        }
    }
}

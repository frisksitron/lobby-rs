//! `LobbyOrigin`: the normalized `scheme://host[:port]` a Lobby is known by,
//! and the rule for which origins may be entered.

use std::{fmt, net::IpAddr};

use crate::copy;

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LobbyOrigin(Box<str>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OriginError {
    #[error("Enter a Lobby URL")]
    Empty,
    #[error("{}", copy::LOBBY_MUST_USE_HTTPS)]
    NotHttps,
    #[error("{}", copy::NOT_A_LOBBY_ADDRESS)]
    Invalid,
}

/// Where plain `http` is accepted. Bearer tokens, refresh tokens, Join tickets and Magic Codes must
/// never travel in cleartext, so only a loopback Lobby may skip TLS unless the developer switch
/// `LOBBY_ALLOW_HTTP=1` says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HttpPolicy {
    #[default]
    LoopbackOnly,
    Anywhere,
}

impl HttpPolicy {
    pub fn from_allow_http(allow_http: bool) -> Self {
        if allow_http {
            Self::Anywhere
        } else {
            Self::LoopbackOnly
        }
    }
}

impl LobbyOrigin {
    /// What a person typed: trimmed, a scheme-less address is `http://` for a loopback host and
    /// `https://` for any other, lowercased host, default port, path, query and fragment dropped;
    /// then the https rule of `policy`.
    pub fn parse(input: &str, policy: HttpPolicy) -> Result<Self, OriginError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(OriginError::Empty);
        }
        let origin = if input.contains("://") {
            Self::normalize(input)?
        } else {
            let https = Self::normalize(&format!("https://{input}"))?;
            if https.is_loopback() {
                Self::normalize(&format!("http://{input}"))?
            } else {
                https
            }
        };
        if origin.is_allowed(policy) {
            Ok(origin)
        } else {
            Err(OriginError::NotHttps)
        }
    }

    /// Structural normalization only: an explicit `http` or `https` scheme and a host. Stored
    /// origins come back through this, so a file written under `LOBBY_ALLOW_HTTP=1` still loads;
    /// the policy is applied again wherever an origin is used to connect.
    fn normalize(input: &str) -> Result<Self, OriginError> {
        let url = url::Url::parse(input).map_err(|_| OriginError::Invalid)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(OriginError::NotHttps);
        }
        if url.host().is_none() || !url.username().is_empty() || url.password().is_some() {
            return Err(OriginError::Invalid);
        }
        Ok(Self(url.origin().ascii_serialization().into_boxed_str()))
    }

    pub fn is_allowed(&self, policy: HttpPolicy) -> bool {
        self.is_https() || policy == HttpPolicy::Anywhere || self.is_loopback()
    }

    fn is_https(&self) -> bool {
        self.0.starts_with("https://")
    }

    fn is_loopback(&self) -> bool {
        let url = url::Url::parse(&self.0).expect("normalized origin is a valid URL");
        match url.host().expect("normalized origin has a host") {
            url::Host::Domain(domain) => domain == "localhost",
            url::Host::Ipv4(ip) => IpAddr::V4(ip).is_loopback(),
            url::Host::Ipv6(ip) => IpAddr::V6(ip).is_loopback(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `host[:port]`, without the scheme: how a person names the Lobby's address.
    pub fn host(&self) -> &str {
        self.0
            .split_once("://")
            .expect("normalized origin has a scheme")
            .1
    }

    pub(crate) fn http_url(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }

    pub(crate) fn ws_url(&self, path: &str) -> String {
        let rest = self
            .0
            .strip_prefix("https://")
            .map(|r| format!("wss://{r}"))
            .or_else(|| self.0.strip_prefix("http://").map(|r| format!("ws://{r}")))
            .expect("origin scheme is http or https");
        format!("{rest}{path}")
    }
}

impl TryFrom<String> for LobbyOrigin {
    type Error = OriginError;

    fn try_from(s: String) -> Result<Self, OriginError> {
        Self::normalize(&s)
    }
}

impl From<LobbyOrigin> for String {
    fn from(origin: LobbyOrigin) -> String {
        origin.0.into_string()
    }
}

impl fmt::Display for LobbyOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for LobbyOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LobbyOrigin({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_normalization_table() {
        use HttpPolicy::{Anywhere, LoopbackOnly};
        for (input, policy, expected) in [
            (
                "https://lobby.example.com",
                LoopbackOnly,
                Ok("https://lobby.example.com"),
            ),
            (
                "  HTTPS://Lobby.Example.COM/some/path?q=1#x ",
                LoopbackOnly,
                Ok("https://lobby.example.com"),
            ),
            (
                "https://lobby.example.com:443",
                LoopbackOnly,
                Ok("https://lobby.example.com"),
            ),
            (
                "https://lobby.example.com:8443/",
                LoopbackOnly,
                Ok("https://lobby.example.com:8443"),
            ),
            (
                "lobby.example.com",
                LoopbackOnly,
                Ok("https://lobby.example.com"),
            ),
            (
                "Lobby.Example.com:8443/x",
                LoopbackOnly,
                Ok("https://lobby.example.com:8443"),
            ),
            ("localhost:8080", LoopbackOnly, Ok("http://localhost:8080")),
            ("localhost", LoopbackOnly, Ok("http://localhost")),
            ("127.0.0.1", LoopbackOnly, Ok("http://127.0.0.1")),
            ("[::1]:9000", LoopbackOnly, Ok("http://[::1]:9000")),
            (
                "https://localhost:8080",
                LoopbackOnly,
                Ok("https://localhost:8080"),
            ),
            (
                "localhost.example.com",
                LoopbackOnly,
                Ok("https://localhost.example.com"),
            ),
            (
                "192.168.1.20:8080",
                LoopbackOnly,
                Ok("https://192.168.1.20:8080"),
            ),
            (
                "http://localhost:8080",
                LoopbackOnly,
                Ok("http://localhost:8080"),
            ),
            ("http://127.0.0.1:80/", LoopbackOnly, Ok("http://127.0.0.1")),
            (
                "http://127.1.2.3:8080",
                LoopbackOnly,
                Ok("http://127.1.2.3:8080"),
            ),
            ("http://[::1]:8080", LoopbackOnly, Ok("http://[::1]:8080")),
            (
                "http://lobby.example.com",
                LoopbackOnly,
                Err(OriginError::NotHttps),
            ),
            (
                "http://192.168.1.20:8080",
                LoopbackOnly,
                Err(OriginError::NotHttps),
            ),
            (
                "http://localhost.example.com",
                LoopbackOnly,
                Err(OriginError::NotHttps),
            ),
            (
                "http://[::2]:8080",
                LoopbackOnly,
                Err(OriginError::NotHttps),
            ),
            (
                "http://lobby.example.com",
                Anywhere,
                Ok("http://lobby.example.com"),
            ),
            (
                "http://192.168.1.20:8080",
                Anywhere,
                Ok("http://192.168.1.20:8080"),
            ),
            (
                "ftp://lobby.example.com",
                Anywhere,
                Err(OriginError::NotHttps),
            ),
            (
                "ws://localhost:8080",
                LoopbackOnly,
                Err(OriginError::NotHttps),
            ),
            (" ", LoopbackOnly, Err(OriginError::Empty)),
            ("http://", LoopbackOnly, Err(OriginError::Invalid)),
            (
                "https://u:p@lobby.example.com",
                LoopbackOnly,
                Err(OriginError::Invalid),
            ),
            ("lobby example", LoopbackOnly, Err(OriginError::Invalid)),
        ] {
            assert_eq!(
                LobbyOrigin::parse(input, policy)
                    .as_ref()
                    .map(LobbyOrigin::as_str),
                expected.as_ref().map(|s| *s),
                "{input:?} under {policy:?}"
            );
        }
    }
}

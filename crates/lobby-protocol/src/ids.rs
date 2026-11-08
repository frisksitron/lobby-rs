//! Identifiers. Prefixed ids are `prefix_<24 lowercase hex>`, validated on parse.

use std::{fmt, num::NonZeroU64, str::FromStr};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("invalid {kind}: expected \"{prefix}_\" followed by 24 lowercase hex characters")]
    Prefixed {
        kind: &'static str,
        prefix: &'static str,
    },
    #[error("invalid rid: expected 1-64 characters of [A-Za-z0-9_-]")]
    Rid,
    #[error("invalid {kind}: expected an integer greater than 0")]
    NotPositive { kind: &'static str },
}

const HEX_LEN: usize = 24;

fn is_valid_prefixed(prefix: &str, s: &str) -> bool {
    s.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('_'))
        .is_some_and(|hex| {
            hex.len() == HEX_LEN && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

#[cfg(feature = "rand")]
fn random_prefixed(prefix: &str) -> String {
    use std::fmt::Write as _;
    let bytes: [u8; HEX_LEN / 2] = rand::random();
    let mut s = String::with_capacity(prefix.len() + 1 + HEX_LEN);
    s.push_str(prefix);
    s.push('_');
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

macro_rules! prefixed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(Box<str>);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[cfg(feature = "rand")]
            pub fn random() -> Self {
                Self(random_prefixed($prefix).into_boxed_str())
            }

            fn invalid() -> IdError {
                IdError::Prefixed { kind: stringify!($name), prefix: $prefix }
            }
        }

        impl FromStr for $name {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, IdError> {
                if is_valid_prefixed($prefix, s) { Ok(Self(s.into())) } else { Err(Self::invalid()) }
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(s: String) -> Result<Self, IdError> {
                if is_valid_prefixed($prefix, &s) {
                    Ok(Self(s.into_boxed_str()))
                } else {
                    Err(Self::invalid())
                }
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0.into_string()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

prefixed_id!(UserId, "usr");
prefixed_id!(AccessRequestId, "acr");
prefixed_id!(
    /// Media URL component; a 96-bit random capability.
    BlobId,
    "blb"
);
prefixed_id!(
    /// Generated once per database, so a client can detect a reset Lobby at the same URL.
    LobbyInstanceId,
    "lby"
);

/// A Message's position in the Lobby's single Chat (`messages.seq`); also the page cursor.
/// Always > 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct MessageId(i64);

impl MessageId {
    pub fn new(seq: i64) -> Result<Self, IdError> {
        if seq > 0 {
            Ok(Self(seq))
        } else {
            Err(IdError::NotPositive { kind: "MessageId" })
        }
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for MessageId {
    type Error = IdError;
    fn try_from(seq: i64) -> Result<Self, IdError> {
        Self::new(seq)
    }
}

impl From<MessageId> for i64 {
    fn from(id: MessageId) -> i64 {
        id.0
    }
}

/// Server-assigned per Voice join; every RTC frame carries it. Always > 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct VoiceSessionId(NonZeroU64);

impl VoiceSessionId {
    pub fn new(generation: u64) -> Result<Self, IdError> {
        NonZeroU64::new(generation)
            .map(Self)
            .ok_or(IdError::NotPositive {
                kind: "VoiceSessionId",
            })
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for VoiceSessionId {
    type Error = IdError;
    fn try_from(generation: u64) -> Result<Self, IdError> {
        Self::new(generation)
    }
}

impl From<VoiceSessionId> for u64 {
    fn from(id: VoiceSessionId) -> u64 {
        id.get()
    }
}

/// Server-assigned when a Screen Share starts. It survives bitrate, encoder and layer changes, and
/// changes when the share restarts or its source changes. Always > 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct ShareId(NonZeroU64);

impl ShareId {
    pub fn new(id: u64) -> Result<Self, IdError> {
        NonZeroU64::new(id)
            .map(Self)
            .ok_or(IdError::NotPositive { kind: "ShareId" })
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for ShareId {
    type Error = IdError;
    fn try_from(id: u64) -> Result<Self, IdError> {
        Self::new(id)
    }
}

impl From<ShareId> for u64 {
    fn from(id: ShareId) -> u64 {
        id.get()
    }
}

/// Client-chosen command id: 1-64 characters of `[A-Za-z0-9_-]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rid(Box<str>);

impl Rid {
    pub const MAX_LEN: usize = 64;

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(s: &str) -> bool {
        (1..=Self::MAX_LEN).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }
}

impl FromStr for Rid {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        if Self::is_valid(s) {
            Ok(Self(s.into()))
        } else {
            Err(IdError::Rid)
        }
    }
}

impl TryFrom<String> for Rid {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, IdError> {
        if Self::is_valid(&s) {
            Ok(Self(s.into_boxed_str()))
        } else {
            Err(IdError::Rid)
        }
    }
}

impl From<Rid> for String {
    fn from(rid: Rid) -> String {
        rid.0.into_string()
    }
}

impl fmt::Display for Rid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

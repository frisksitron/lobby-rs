//! Secret newtypes: transparent on the wire, `***` in `Debug`, so no derived
//! `Debug` of a DTO can leak one into a log.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValueError;

macro_rules! opaque_secret {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(token: impl Into<String>) -> Self {
                Self(token.into())
            }

            pub fn expose(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(***)"))
            }
        }
    };
}

opaque_secret!(
    /// Opaque bearer token (32 random bytes, base64url). Sent in `IDENTIFY` and `Authorization`.
    AccessToken
);
opaque_secret!(
    /// Opaque refresh token (32 random bytes, base64url), rotated on every refresh.
    RefreshToken
);
opaque_secret!(
    /// Proof this device checked a code for an email no Member uses: what choosing a name spends.
    /// 32 random bytes, base64url; ten minutes, once.
    JoinTicket
);
opaque_secret!(
    /// Proof that this device asked to join (32 random bytes, base64url): what Waiting for a
    /// Vouch checks with, and what turns into a session once a Member Vouches.
    WaitToken
);
opaque_secret!(
    /// Which request a Magic Code answers (32 random bytes, base64url). Only the device that
    /// asked holds it, so only it can spend that code's attempts.
    MagicChallenge
);
opaque_secret!(
    #[derive(Default)]
    SmtpPassword
);

/// Exactly 8 ASCII digits. A secret: `Debug` prints `***`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MagicCode(String);

impl MagicCode {
    pub const LEN: usize = 8;

    pub fn new(code: impl Into<String>) -> Result<Self, ValueError> {
        let code: String = code.into().trim().to_owned();
        if code.is_empty() {
            return Err(ValueError::Invalid("code is required"));
        }
        if code.chars().count() != Self::LEN {
            return Err(ValueError::Invalid("invalid code length"));
        }
        if !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ValueError::Invalid("code must contain only digits"));
        }
        Ok(Self(code))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MagicCode {
    type Error = ValueError;
    fn try_from(s: String) -> Result<Self, ValueError> {
        Self::new(s)
    }
}

impl From<MagicCode> for String {
    fn from(c: MagicCode) -> String {
        c.0
    }
}

impl fmt::Debug for MagicCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MagicCode(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_secret() {
        assert_eq!(format!("{:?}", AccessToken::new("abc")), "AccessToken(***)");
        assert_eq!(
            format!("{:?}", RefreshToken::new("abc")),
            "RefreshToken(***)"
        );
        assert_eq!(format!("{:?}", JoinTicket::new("abc")), "JoinTicket(***)");
        assert_eq!(
            format!("{:?}", MagicCode::new("12345678").unwrap()),
            "MagicCode(***)"
        );
    }
}

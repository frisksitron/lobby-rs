use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValueError {
    /// The message is user-facing copy.
    #[error("{0}")]
    Invalid(&'static str),
}

/// Three capital letters that name one request for a Magic Code ("KQV"). The code's email says
/// it and the app shows it, so a Member picks the mail that answers their request when others
/// asked for codes to the same address. Not a secret: it is made from the request's hash, and
/// names nothing but the mail.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequestLabel(String);

impl RequestLabel {
    pub const LEN: usize = 3;
    /// No vowels, so a label spells no word.
    const LETTERS: &'static [u8] = b"BCDFGHJKLMNPQRSTVWXZ";

    pub fn from_digest(digest: &[u8; 32]) -> Self {
        let letters = Self::LETTERS;
        Self(
            digest
                .iter()
                .take(Self::LEN)
                .map(|b| char::from(letters[usize::from(*b) % letters.len()]))
                .collect(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RequestLabel {
    type Error = ValueError;

    fn try_from(s: String) -> Result<Self, ValueError> {
        if s.len() == Self::LEN && s.bytes().all(|b| Self::LETTERS.contains(&b)) {
            Ok(Self(s))
        } else {
            Err(ValueError::Invalid("invalid request label"))
        }
    }
}

impl From<RequestLabel> for String {
    fn from(l: RequestLabel) -> String {
        l.0
    }
}

impl fmt::Display for RequestLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Trimmed, lowercased, at most 254 chars, one `@`, a non-empty local part and a dotted domain.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Email(Box<str>);

impl Email {
    pub const MAX_LEN: usize = 254;

    pub fn parse(input: &str) -> Result<Self, ValueError> {
        let s = input.trim().to_lowercase();
        if s.is_empty() {
            return Err(ValueError::Invalid("email is required"));
        }
        let invalid = ValueError::Invalid("invalid email format");
        if s.chars().count() > Self::MAX_LEN || s.chars().any(char::is_whitespace) {
            return Err(invalid);
        }
        let mut parts = s.split('@');
        let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid);
        };
        let dotted = domain.split('.').count() >= 2 && domain.split('.').all(|l| !l.is_empty());
        if local.is_empty() || !dotted {
            return Err(invalid);
        }
        Ok(Self(s.into_boxed_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The mailbox behind a `+tag` alias: `a+b@x.com` is `a@x.com`. A local part that is only a
    /// tag stays as it is.
    pub fn without_tag(&self) -> Self {
        let (local, domain) = self.0.split_once('@').expect("validated Email contains @");
        match local.split_once('+') {
            Some((base, _)) if !base.is_empty() => Self(format!("{base}@{domain}").into()),
            _ => self.clone(),
        }
    }

    /// Logs carry the domain only.
    pub fn domain(&self) -> &str {
        self.0
            .split_once('@')
            .expect("validated Email contains @")
            .1
    }
}

impl FromStr for Email {
    type Err = ValueError;
    fn from_str(s: &str) -> Result<Self, ValueError> {
        Self::parse(s)
    }
}

impl TryFrom<String> for Email {
    type Error = ValueError;
    fn try_from(s: String) -> Result<Self, ValueError> {
        Self::parse(&s)
    }
}

impl From<Email> for String {
    fn from(e: Email) -> String {
        e.0.into_string()
    }
}

impl fmt::Debug for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Email(…@{})", self.domain())
    }
}

/// The name `@everyone` mentions every Member by. No Member may take it as a username.
pub const EVERYONE: &str = "everyone";

/// Trimmed, 3–32 characters of `[A-Za-z0-9_-]`. Uniqueness is case-insensitive; display is as
/// typed.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Username(Box<str>);

impl Username {
    pub fn parse(input: &str) -> Result<Self, ValueError> {
        let s = input.trim();
        if !crate::limits::USERNAME_LEN.contains(&s.chars().count()) {
            return Err(ValueError::Invalid("Username must be 3-32 characters"));
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(ValueError::Invalid(
                "Username may only contain letters, numbers, underscores and hyphens",
            ));
        }
        Ok(Self(s.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A name no one may be given, though `parse` accepts it: a stored row or a wire frame that
    /// already holds it must still decode.
    pub fn is_reserved(&self) -> bool {
        self.0.eq_ignore_ascii_case(EVERYONE)
    }
}

impl FromStr for Username {
    type Err = ValueError;
    fn from_str(s: &str) -> Result<Self, ValueError> {
        Self::parse(s)
    }
}

impl TryFrom<String> for Username {
    type Error = ValueError;
    fn try_from(s: String) -> Result<Self, ValueError> {
        Self::parse(&s)
    }
}

impl From<Username> for String {
    fn from(u: Username) -> String {
        u.0.into_string()
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Username({})", self.0)
    }
}

/// What others see of a Member. `online`, `idle` (the OS idle time) and `dnd` (Do Not Disturb,
/// the Member's choice) are what a client may ask for; `offline` means no connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Online,
    Idle,
    Dnd,
    Offline,
}

impl Presence {
    pub fn is_settable(self) -> bool {
        self != Presence::Offline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_normalized_and_validated() {
        assert_eq!(
            Email::parse("  Dave@Example.COM ").unwrap().as_str(),
            "dave@example.com"
        );
        assert_eq!(Email::parse("a@b.co").unwrap().domain(), "b.co");
        assert_eq!(
            Email::parse("  ").unwrap_err().to_string(),
            "email is required"
        );
        for bad in [
            "dave",
            "@example.com",
            "dave@",
            "dave@example",
            "a@b@c.com",
            "a@.com",
            "a b@c.com",
        ] {
            assert_eq!(
                Email::parse(bad).unwrap_err().to_string(),
                "invalid email format",
                "{bad:?}"
            );
        }
        let long = format!("{}@example.com", "a".repeat(250));
        assert!(Email::parse(&long).is_err());
    }
}

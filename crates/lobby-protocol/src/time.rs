//! `UnixMs`: the one time representation in every `*At` field, the DB and the wire.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMs(pub i64);

impl UnixMs {
    pub fn now() -> Self {
        Self::from(SystemTime::now())
    }

    pub fn saturating_add(self, d: Duration) -> Self {
        Self(
            self.0
                .saturating_add(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        )
    }
}

impl From<SystemTime> for UnixMs {
    fn from(t: SystemTime) -> Self {
        // A clock before 1970 is a broken host; clamp instead of panicking.
        let ms = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        Self(i64::try_from(ms).unwrap_or(i64::MAX))
    }
}

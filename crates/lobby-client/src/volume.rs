//! How loud one other Member plays for this person: whole percent from silent to double, 100 as
//! sent. It is a private choice kept on this device; nobody else hears the change.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(from = "u16", into = "u16")]
pub struct Volume(u16);

impl Volume {
    pub const MAX_PERCENT: u16 = 200;
    pub const DEFAULT: Volume = Volume(100);

    /// `percent`, held to [`Volume::MAX_PERCENT`].
    pub fn new(percent: u16) -> Self {
        Self(percent.min(Self::MAX_PERCENT))
    }

    pub fn percent(self) -> u16 {
        self.0
    }

    /// The factor the mix multiplies the Member's samples by.
    pub fn gain(self) -> f32 {
        f32::from(self.0) / 100.0
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<u16> for Volume {
    fn from(percent: u16) -> Self {
        Self::new(percent)
    }
}

impl From<Volume> for u16 {
    fn from(volume: Volume) -> Self {
        volume.0
    }
}

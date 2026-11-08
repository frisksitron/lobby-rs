//! Ink: viewers draw on a Screen Share. Points refer to the share's source, so a layer or encoder
//! change never moves a stroke.

use serde::{Deserialize, Serialize};

use crate::{ShareId, UserId};

/// The server advances it when drawing turns on or off and when the source size changes. A batch
/// with an old epoch is stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DrawingEpoch(pub u32);

/// Chosen by the author, unique per author per share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StrokeId(pub u32);

/// A point on the source: 0 at the left or top edge, 65535 at the right or bottom edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InkPoint {
    pub x: u16,
    pub y: u16,
}

/// Ink is lossy, so each batch draws on its own: `prev` is the previous batch's last point, and a
/// receiver breaks the line at a gap in `seq`. `end` marks pen up. At most `INK_POINTS_MAX`
/// points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InkBatch {
    pub share_id: ShareId,
    pub epoch: DrawingEpoch,
    pub stroke: StrokeId,
    pub seq: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<InkPoint>,
    pub points: Vec<InkPoint>,
    pub end: bool,
}

/// The server's `INK`: a batch with its author, to the Sharer and the Watchers but not back to
/// the author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InkRelay {
    pub author_id: UserId,
    pub batch: InkBatch,
}

/// `DRAWING_SET`: the Sharer's "Let viewers draw".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingSet {
    pub enabled: bool,
}

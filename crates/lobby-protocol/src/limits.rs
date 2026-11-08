use std::{ops::RangeInclusive, time::Duration};

pub const MESSAGE_MAX_CHARS: usize = 8000;
pub const MESSAGE_NESTING_MAX: usize = 32;
pub const HISTORY_DEFAULT: u8 = 50;
pub const HISTORY_MAX: u8 = 100;
/// The most `GET /api/v1/messages/changes` answers with. A client further behind starts its
/// feed over.
pub const CHANGES_MAX: u64 = 200;
pub const ATTACHMENTS_MAX: usize = 10;
pub const USERNAME_LEN: RangeInclusive<usize> = 3..=32;
pub const MAGIC_CODE_ATTEMPTS: u32 = 5;
/// The window of the per-email cap on mailed Magic Codes (`LobbyInfo::magic_code_sends`).
pub const MAGIC_CODE_SEND_WINDOW: Duration = Duration::from_secs(10 * 60);
pub const TYPING_SEND_INTERVAL: Duration = Duration::from_secs(3);
pub const TYPING_EXPIRY: Duration = Duration::from_secs(6);
/// Points in one `INK` batch.
pub const INK_POINTS_MAX: usize = 64;
/// JSON request bodies on every non-upload REST route.
pub const JSON_BODY_MAX_BYTES: usize = 64 * 1024;
pub const IMAGE_MAX_EDGE: u32 = 16_384;
pub const IMAGE_MAX_ALLOC: u64 = 256 * 1024 * 1024;

//! WebSocket close codes. Fatal conditions are close codes, not frames.

/// Client sign-out, switch or quit; the server after a clean client close.
pub const NORMAL: u16 = 1000;
pub const GOING_AWAY: u16 = 1001;
/// The server hit a bug while handling this connection.
pub const INTERNAL_ERROR: u16 = 1011;
/// Binary frame, oversized frame, a command before IDENTIFY, a bad IDENTIFY payload.
pub const PROTOCOL_VIOLATION: u16 = 4000;
pub const IDENTIFY_TIMEOUT: u16 = 4001;
pub const PONG_TIMEOUT: u16 = 4002;
pub const AUTH_INVALID: u16 = 4003;
/// This session was ended by a Sign Out, on this device or everywhere from another.
pub const SESSION_REVOKED: u16 = 4005;
/// Removed from the Lobby by its owner. The client clears this Lobby's tokens and goes to Sign In,
/// saying why.
pub const REMOVED: u16 = 4006;
pub const SLOW_CONSUMER: u16 = 4008;
pub const REPLACED: u16 = 4009;
pub const IDENTIFY_RATE_LIMITED: u16 = 4029;

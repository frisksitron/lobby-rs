//! Why a write was refused. `Display` is the copy the client shows. The REST edge (`error.rs`) and
//! the WebSocket edge (`ws/refusal.rs`) each map it to the wire, once.

use crate::db::{DbError, admission::TicketRefused, users::Taken};

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("Invalid code")]
    InvalidCode,
    /// The challenge has no unused code: the same wire answer as a wrong code.
    #[error("Invalid code")]
    NoLiveCode,
    /// This client had its codes for now, for this email or for any. Only the asker's own requests
    /// count, so saying so tells nothing about the email.
    #[error("Too many codes asked for. Try again later.")]
    CodesCapped(std::time::Duration),
    #[error("Too many attempts")]
    TooManyAttempts,
    #[error("Code has expired")]
    CodeExpired,
    #[error("Invalid or used join ticket")]
    InvalidTicket,
    #[error("Join ticket has expired")]
    TicketExpired,
    #[error("Username already taken")]
    UsernameTaken,
    /// A join whose email became a Member's after its code was checked.
    #[error("Email is already in use")]
    EmailTaken,
    /// Nobody can join a Lobby without Members except its bootstrap email.
    #[error("This Lobby has not been set up yet")]
    NotSetUp,
    #[error("This Lobby is not accepting join requests")]
    Locked,
    /// Too many Asks to Join wait for a Vouch already.
    #[error("Too many people are waiting to join. Try again later.")]
    TooManyRequests,
    #[error("Invalid or expired wait token")]
    InvalidWait,
    #[error("Access request not found")]
    RequestNotFound,
    /// The acting Member was removed while their write waited for the writer.
    #[error("You are no longer a Member of this Lobby")]
    Removed,
    #[error("Only the Lobby's owner can do that")]
    NotOwner,
    #[error("The Lobby's owner cannot be removed")]
    CannotRemoveOwner,
    #[error("The Lobby's owner cannot leave. Hand ownership to another Member first.")]
    OwnerCannotLeave,
    #[error("No such Member")]
    NotAMember,
    #[error("One or more attachments are no longer available")]
    AttachmentInvalid,
    #[error("That Message is gone")]
    MessageMissing,
    #[error("Only its author can change a Message")]
    NotYourMessage,
    #[error("Server is shutting down. Try again in a moment.")]
    Unavailable,
    #[error("Server is busy. Try again in a moment.")]
    Busy,
    #[error("An internal error occurred")]
    Panicked,
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for WriteError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e.into())
    }
}

impl From<Taken> for WriteError {
    fn from(t: Taken) -> Self {
        match t {
            Taken::Username => Self::UsernameTaken,
            Taken::Email => Self::EmailTaken,
        }
    }
}

impl From<TicketRefused> for WriteError {
    fn from(r: TicketRefused) -> Self {
        match r {
            TicketRefused::Invalid => Self::InvalidTicket,
            TicketRefused::Expired => Self::TicketExpired,
        }
    }
}

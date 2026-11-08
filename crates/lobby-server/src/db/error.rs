use sqlx::error::ErrorKind;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A UNIQUE or PRIMARY KEY constraint; the message names the column.
    #[error("constraint failed: {0}")]
    Conflict(String),
    #[error("database busy")]
    Busy,
    /// No connection within [`super::ACQUIRE_TIMEOUT`].
    #[error("database did not answer in time")]
    Timeout,
    /// A stored value that our own types reject: a bug or a hand-edited database.
    #[error("unexpected stored value: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Other(sqlx::Error),
}

impl From<sqlx::Error> for DbError {
    fn from(e: sqlx::Error) -> Self {
        if matches!(e, sqlx::Error::PoolTimedOut) {
            return DbError::Timeout;
        }
        if let Some(db) = e.as_database_error() {
            if db.kind() == ErrorKind::UniqueViolation {
                return DbError::Conflict(db.message().to_owned());
            }
            if db.code().is_some_and(|c| c == "5" || c == "6") {
                return DbError::Busy;
            }
        }
        DbError::Other(e)
    }
}

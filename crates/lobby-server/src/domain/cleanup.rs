//! The cleanup job's writes: one short batch each, so the Lobby's own writes interleave.

use lobby_protocol::UnixMs;

use super::{Domain, WriteError};
use crate::{
    blobs::BlobFiles,
    db::{admission, blobs, sessions},
};

#[derive(Debug, Clone, Copy)]
pub enum Batch {
    MagicCodes,
    JoinTickets,
    JoinWaits,
    Sessions,
    /// Declined requests decided before this.
    Declined(UnixMs),
    /// Requests still waiting for a Vouch that were made before this.
    StalePending(UnixMs),
}

impl Domain {
    /// Deletes up to one batch of what expired by `now`; how many rows went.
    pub async fn clean(&self, batch: Batch, now: UnixMs) -> Result<u64, WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let conn = &mut w.conn;
                Ok(match batch {
                    Batch::MagicCodes => admission::delete_expired_codes(conn, now).await?,
                    Batch::JoinTickets => admission::delete_expired_tickets(conn, now).await?,
                    Batch::JoinWaits => admission::delete_expired_waits(conn, now).await?,
                    Batch::Sessions => sessions::delete_expired(conn, now).await?,
                    Batch::Declined(before) => admission::delete_old_declined(conn, before).await?,
                    Batch::StalePending(before) => {
                        let gone = admission::delete_stale_pending(conn, before).await?;
                        w.lobby.access_requests_expired(&gone, now);
                        gone.len() as u64
                    }
                })
            })
        })
        .await
    }

    /// Deletes up to one batch of expired unclaimed uploads' rows and answers their files, for the
    /// caller to delete. A crash in between leaves orphan files, never rows pointing at nothing.
    pub async fn take_expired_uploads(&self, now: UnixMs) -> Result<Vec<BlobFiles>, WriteError> {
        self.run(move |w| Box::pin(async move { Ok(blobs::take_expired(&mut w.conn, now).await?) }))
            .await
    }

    pub async fn optimize(&self) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                sqlx::query("PRAGMA optimize").execute(&mut w.conn).await?;
                Ok(())
            })
        })
        .await
    }
}

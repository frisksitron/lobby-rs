//! Sessions: refresh rotation and the first use of a token pair.

use lobby_protocol::{SessionTokens, UnixMs};
use tracing::warn;

use super::{Domain, WriteError};
pub(crate) use crate::db::sessions::Refreshed;
use crate::{
    auth::tokens::{Digest, Minted},
    db::sessions,
};

impl Domain {
    /// Rotates the session that holds `presented`, or reissues it after a lost response; a reuse
    /// deletes the session and closes its connection with 4005. The tokens are `minted`'s.
    pub async fn refresh(
        &self,
        presented: Digest,
        minted: Minted,
    ) -> Result<(Refreshed, SessionTokens), WriteError> {
        self.run_at_front_door(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let outcome =
                    sessions::refresh(&mut w.conn, &presented, &minted.pair(), now).await?;
                if let Refreshed::ReuseDetected { session, user } = &outcome {
                    warn!(%user, session = session.0, "refresh token reuse: session revoked");
                    w.lobby.kick_session(*session);
                }
                Ok((outcome, minted.into_tokens()))
            })
        })
        .await
    }

    /// The first use of the current pair (at most once per rotation). From then on, presenting the
    /// previous refresh token counts as reuse.
    pub async fn mark_used(&self, access: Digest) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                sessions::mark_used(&mut w.conn, &access, UnixMs::now()).await?;
                Ok(())
            })
        })
        .await
    }
}

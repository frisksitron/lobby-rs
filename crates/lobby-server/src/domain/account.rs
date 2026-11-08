//! A Member's own account: their name and their sessions. A Sign Out deletes sessions and, still
//! holding the writer, closes their connections with 4005 and refuses any registration whose auth
//! lookup came before it.

use lobby_protocol::{UnixMs, User, UserId, Username};
use tracing::info;

use super::{Domain, WriteError};
use crate::{
    auth::Authenticated,
    db::{self, sessions, users},
};

impl Domain {
    /// The same name again is answered and nothing else; another name nobody holds
    /// (case-insensitively, Access Requests that still wait included) is `MEMBER_UPSERT` for
    /// everyone.
    pub async fn rename(&self, me: UserId, username: Username) -> Result<User, WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                let Some(mut profile) = users::active_profile(&mut tx, &me).await? else {
                    return Err(WriteError::Removed);
                };
                if profile.username == username {
                    return Ok(profile.user());
                }
                if users::username_taken(&mut tx, &username, &me).await? {
                    return Err(WriteError::UsernameTaken);
                }
                users::rename(&mut tx, &me, &username, now).await?;
                tx.commit().await?;
                w.lobby.renamed(&me, &username);
                info!(user = %me, "renamed");
                profile.username = username;
                Ok(profile.user())
            })
        })
        .await
    }

    /// This session, or with `everywhere` every session of the Member.
    pub async fn sign_out(&self, me: Authenticated, everywhere: bool) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                if everywhere {
                    let gone = sessions::delete_all(&mut w.conn, &me.user).await?;
                    w.lobby.kick_user(&me.user);
                    info!(user = %me.user, sessions = gone, "signed out everywhere");
                } else {
                    sessions::delete_one(&mut w.conn, me.session).await?;
                    w.lobby.kick_session(me.session);
                    info!(user = %me.user, session = me.session.0, "signed out");
                }
                Ok(())
            })
        })
        .await
    }
}

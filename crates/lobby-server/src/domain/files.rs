//! Files and their rows: chat attachments, avatars and the Lobby's icon. Every upload is staged
//! first: its files are written before the write is accepted, and they stay only once a row
//! names them. A [`Staged`] dropped before that (the body was cut, the Member cancelled, the row
//! was refused) deletes its files, off the caller. A blocking file write still running in that
//! instant can land after; the orphan sweep removes it.

use std::{sync::Arc, time::Duration};

use axum::{BoxError, body::Bytes};
use futures_util::Stream;
use lobby_protocol::{Attachment, BlobId, LobbyInfo, UnixMs, User, UserId};
use sqlx::SqliteConnection;
use tokio::sync::Semaphore;
use tracing::info;

use super::{Domain, WriteError};
use crate::{
    blobs::{BlobError, BlobFiles, BlobKind, BlobStore, StoredBlob, StoredPreview},
    db::{
        self,
        blobs::{self, NewBlob},
        icon, users,
    },
};

/// An upload nobody attaches to a Message is deleted after this.
pub const UNCLAIMED_TTL: Duration = Duration::from_secs(24 * 3600);

/// Files on disk that no row names yet.
pub struct Staged {
    blobs: BlobStore,
    blob: StoredBlob,
    preview: Option<StoredPreview>,
    kept: bool,
}

impl Staged {
    /// Streams `body` into the store as `kind`. A chat attachment gets its preview, if it is an
    /// image; an avatar or icon becomes its square, and the upload itself goes.
    pub async fn upload<S, E>(
        blobs: &BlobStore,
        cpu: &Arc<Semaphore>,
        kind: BlobKind,
        name: Option<&str>,
        body: S,
    ) -> Result<Staged, BlobError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: Into<BoxError>,
    {
        let mut upload = Staged::new(blobs, blobs.save(kind, name, body).await?);
        if kind != BlobKind::ChatAttachment {
            return Ok(Staged::new(blobs, blobs.square(cpu, &upload.blob).await?));
        }
        upload.preview = blobs.make_preview(cpu, &upload.blob).await;
        Ok(upload)
    }

    fn new(blobs: &BlobStore, blob: StoredBlob) -> Staged {
        Staged {
            blobs: blobs.clone(),
            blob,
            preview: None,
            kept: false,
        }
    }

    /// A row names the files now: they stay.
    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.kept {
            self.blobs.discard(self.blob.files(self.preview.as_ref()));
        }
    }
}

impl Domain {
    /// An unclaimed chat attachment, until a Message claims it or [`UNCLAIMED_TTL`] passes.
    pub async fn store_upload(&self, by: UserId, staged: Staged) -> Result<Attachment, WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let attachment = blobs::insert(
                    &mut w.conn,
                    NewBlob {
                        stored: &staged.blob,
                        preview: staged.preview.as_ref(),
                        uploaded_by: &by,
                        now,
                        expires_at: Some(now.saturating_add(UNCLAIMED_TTL)),
                    },
                )
                .await?;
                staged.keep();
                Ok(attachment)
            })
        })
        .await
    }

    /// `MEMBER_UPSERT` with the new avatar to everyone. Removing an avatar that is not there
    /// changes nothing and tells nobody.
    pub async fn set_avatar(&self, me: UserId, new: Option<Staged>) -> Result<User, WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                let Some(profile) = users::active_profile(&mut tx, &me).await? else {
                    return Err(WriteError::Removed);
                };
                let mut user = profile.user();
                if new.is_none() && profile.avatar.is_none() {
                    return Ok(user);
                }
                if let Some(new) = &new {
                    insert_row(&mut tx, new, &me, now).await?;
                }
                let id = new.as_ref().map(|n| n.blob.id.clone());
                users::set_avatar(&mut tx, &me, id.as_ref(), now).await?;
                let old = delete_row(&mut tx, profile.avatar.as_ref()).await?;
                tx.commit().await?;
                settle(&w.blobs, new, old);
                w.lobby.avatar_changed(&me, id.clone());
                info!(user = %me, set = id.is_some(), "avatar changed");
                user.avatar = id;
                Ok(user)
            })
        })
        .await
    }

    /// `LOBBY_UPDATE` with the new icon to everyone; any Member may. Removing an icon that is not
    /// there changes nothing and tells nobody.
    pub async fn set_icon(&self, me: UserId, new: Option<Staged>) -> Result<LobbyInfo, WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                if users::active_profile(&mut tx, &me).await?.is_none() {
                    return Err(WriteError::Removed);
                }
                let current = icon::get(&mut tx).await?;
                if new.is_none() && current.is_none() {
                    return Ok(w.lobby.info());
                }
                if let Some(new) = &new {
                    insert_row(&mut tx, new, &me, now).await?;
                }
                let id = new.as_ref().map(|n| n.blob.id.clone());
                icon::set(&mut tx, id.as_ref(), now).await?;
                let old = delete_row(&mut tx, current.as_ref()).await?;
                tx.commit().await?;
                settle(&w.blobs, new, old);
                let lobby = w.lobby.icon_changed(id.clone());
                info!(user = %me, set = id.is_some(), "Lobby icon changed");
                Ok(lobby)
            })
        })
        .await
    }
}

/// Right after the commit, before anyone is told: the new picture's files stay, and the replaced
/// one's go off the writer task.
fn settle(blobs: &BlobStore, new: Option<Staged>, old: Option<BlobFiles>) {
    if let Some(new) = new {
        new.keep();
    }
    if let Some(old) = old {
        blobs.discard(old);
    }
}

async fn insert_row(
    tx: &mut SqliteConnection,
    staged: &Staged,
    by: &UserId,
    now: UnixMs,
) -> Result<(), WriteError> {
    blobs::insert(
        tx,
        NewBlob {
            stored: &staged.blob,
            preview: None,
            uploaded_by: by,
            now,
            expires_at: None,
        },
    )
    .await?;
    Ok(())
}

async fn delete_row(
    tx: &mut SqliteConnection,
    old: Option<&BlobId>,
) -> Result<Option<BlobFiles>, WriteError> {
    match old {
        Some(id) => Ok(blobs::delete(tx, id).await?),
        None => Ok(None),
    }
}

//! Messages. The author must still be a Member and every attachment theirs, unclaimed and live.
//! Only its author edits or deletes a Message.

use lobby_protocol::{Ack, BlobId, MessageId, Rid, ServerMsg, UnixMs, UserId};
use tracing::{debug, info};

use super::{Domain, WriteError};
use crate::{
    db::messages::{self, Inserted, NotChanged},
    tiers::ConnId,
};

impl From<NotChanged> for WriteError {
    fn from(no: NotChanged) -> Self {
        match no {
            NotChanged::Missing => Self::MessageMissing,
            NotChanged::NotAuthor => Self::NotYourMessage,
            NotChanged::AuthorRemoved => Self::Removed,
        }
    }
}

impl Domain {
    /// Stores the Message and sends `MESSAGE_CREATE` to everyone, or replays the one stored under
    /// the same `nonce` to `conn` alone.
    pub async fn send_message(
        &self,
        author: UserId,
        conn: ConnId,
        nonce: Rid,
        content: String,
        attachments: Vec<BlobId>,
    ) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let stored = messages::insert(
                    &mut w.conn,
                    &author,
                    &content,
                    nonce.as_str(),
                    &attachments,
                    UnixMs::now(),
                )
                .await?;
                match stored {
                    Inserted::New(message) => {
                        w.lobby.broadcast_message(&message);
                        debug!(id = message.id.get(), "message stored");
                    }
                    Inserted::Duplicate(message) => {
                        info!(
                            id = message.id.get(),
                            "duplicate nonce: replayed to the sender"
                        );
                        w.lobby
                            .reply_to(&author, conn, &ServerMsg::MessageCreate { message });
                    }
                    Inserted::Deleted => {
                        info!("duplicate nonce of a deleted Message: acknowledged");
                        w.lobby
                            .reply_to(&author, conn, &ServerMsg::Ack(Ack::new(nonce)));
                    }
                    Inserted::AttachmentsUnavailable => return Err(WriteError::AttachmentInvalid),
                    Inserted::AuthorRemoved => return Err(WriteError::Removed),
                }
                Ok(())
            })
        })
        .await
    }

    pub async fn edit_message(
        &self,
        author: UserId,
        id: MessageId,
        content: String,
    ) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let message =
                    messages::edit(&mut w.conn, &author, id, &content, UnixMs::now()).await??;
                w.lobby.message_updated(&message);
                debug!(id = id.get(), "message edited");
                Ok(())
            })
        })
        .await
    }

    /// Deletes the Message and its attachments and sends `MESSAGE_REMOVED` to everyone. The
    /// attachments' files go after the commit.
    pub async fn delete_message(&self, author: UserId, id: MessageId) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let deleted = messages::delete(&mut w.conn, &author, id, UnixMs::now()).await??;
                for f in deleted.files {
                    w.blobs.discard(f);
                }
                w.lobby.message_removed(id, deleted.rev);
                debug!(id = id.get(), "message deleted");
                Ok(())
            })
        })
        .await
    }
}

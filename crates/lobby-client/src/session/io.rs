//! The operations a `Session` starts and the runner that owns them on the client runtime. Each
//! one's result comes back on the Session's event channel as a [`Completion`].

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use lobby_protocol::{
    AccessRequestId, Attachment, LobbyInfo, MessageChanges, MessagePage, User, UserId, Username,
};
use tokio::{runtime::Handle, sync::mpsc};
use tokio_util::task::AbortOnDropHandle;
use tracing::{Instrument as _, info_span};

use super::{SessionEvent, account::PictureError};
use crate::{
    ApiError, ClientTask,
    api::{ApiClient, Page, Progress},
    drafts::{AttachError, DraftId, DraftSource, UploadError, UploadKey},
    media::{MediaConfig, MediaError, VoiceCallId, VoiceEvents, VoiceHandle},
    state::OwnerAction,
    volume::Volume,
};

/// Upload progress reaches the Session at most this often; it is lossy, and the next report
/// overtakes a dropped one.
const UPLOAD_PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// A REST call the Session asked for.
#[derive(Debug)]
pub(super) enum Op {
    Messages {
        page: Page,
        limit: u8,
    },
    /// The edits and deletions after change revision `after`, for a catch-up up to `to`.
    Changes {
        after: u64,
        to: u64,
    },
    Decide {
        id: AccessRequestId,
        vouch: bool,
    },
    Owner {
        user: UserId,
        action: OwnerAction,
    },
    /// `POST /uploads?name=`. Keyed by draft, so a Retry's newer attempt replaces the older.
    Upload {
        key: UploadKey,
        source: DraftSource,
    },
    Rename(Username),
    /// `max` is what the Lobby said it takes: a larger file is refused without being sent.
    SetAvatar {
        path: PathBuf,
        max: u64,
    },
    RemoveAvatar,
    SetIcon {
        path: PathBuf,
        max: u64,
    },
    RemoveIcon,
    Leave,
}

/// What an operation the Session started came back with. Only an operation makes one.
#[derive(Debug)]
pub struct Completion(pub(super) Done);

#[derive(Debug)]
pub(super) enum Done {
    Messages {
        page: Page,
        result: Result<MessagePage, ApiError>,
    },
    Changes {
        to: u64,
        result: Result<MessageChanges, ApiError>,
    },
    Decided {
        id: AccessRequestId,
        vouch: bool,
        result: Result<(), ApiError>,
    },
    OwnerActed {
        user: UserId,
        action: OwnerAction,
        result: Result<(), ApiError>,
    },
    UploadProgress {
        key: UploadKey,
        sent: u64,
    },
    Uploaded {
        key: UploadKey,
        result: Result<Attachment, UploadError>,
    },
    Renamed(Result<User, ApiError>),
    Avatar(Result<(), PictureError>),
    /// The Lobby as it is after the change.
    Icon(Result<LobbyInfo, PictureError>),
    Left(Result<(), ApiError>),
}

pub(super) struct IoRunner {
    pub(super) rt: Handle,
    pub(super) api: Arc<ApiClient>,
    events: mpsc::Sender<SessionEvent>,
    /// By kind ("messages") or by subject ("decide:acr_…"): a newer request replaces the older.
    tasks: HashMap<String, AbortOnDropHandle<()>>,
    voice: Option<VoiceHandle>,
    /// The teardowns of ended Voice sessions still running, awaited by `Session::shutdown`.
    voice_stopping: Vec<ClientTask<()>>,
}

impl IoRunner {
    pub(super) fn new(rt: Handle, api: Arc<ApiClient>, events: mpsc::Sender<SessionEvent>) -> Self {
        Self {
            rt,
            api,
            events,
            tasks: HashMap::new(),
            voice: None,
            voice_stopping: Vec::new(),
        }
    }

    pub(super) fn run(&mut self, op: Op) {
        self.tasks.retain(|_, task| !task.is_finished());
        let key = match &op {
            // Older pages run beside the newest page and the backfill, never replacing them.
            Op::Messages {
                page: Page::Before(_),
                ..
            } => "messages:older".to_owned(),
            Op::Messages { .. } => "messages".to_owned(),
            Op::Changes { .. } => "messages:changes".to_owned(),
            Op::Decide { id, .. } => format!("decide:{}", id.as_str()),
            Op::Owner { user, .. } => format!("owner:{user}"),
            Op::Upload { key, .. } => upload_task_key(key.draft),
            Op::Rename(_) => "account:rename".to_owned(),
            Op::SetAvatar { .. } | Op::RemoveAvatar => "account:avatar".to_owned(),
            Op::SetIcon { .. } | Op::RemoveIcon => "account:icon".to_owned(),
            Op::Leave => "account:leave".to_owned(),
        };
        let (api, events) = (self.api.clone(), self.events.clone());
        let task = self.rt.spawn(async move {
            let done = perform(&api, op, &events).await;
            let _ = events.send(SessionEvent::Completed(Completion(done))).await;
        });
        self.tasks.insert(key, AbortOnDropHandle::new(task));
    }

    pub(super) fn cancel_upload(&mut self, draft: DraftId) {
        self.tasks.remove(&upload_task_key(draft));
    }

    /// Starts the media of a new call, stopping any before it.
    pub(super) fn start_voice(
        &mut self,
        call: VoiceCallId,
        me: UserId,
        media: MediaConfig,
        volumes: HashMap<UserId, Volume>,
    ) -> Result<(), MediaError> {
        self.stop_voice();
        let events = VoiceEvents::new(call, self.events.clone());
        self.voice = Some(VoiceHandle::start(me, media, volumes, events)?);
        Ok(())
    }

    pub(super) fn voice(&self) -> Option<&VoiceHandle> {
        self.voice.as_ref()
    }

    pub(super) fn voice_mut(&mut self) -> Option<&mut VoiceHandle> {
        self.voice.as_mut()
    }

    pub(super) fn stop_voice(&mut self) {
        if let Some(v) = self.voice.take() {
            self.voice_stopping.retain(|t| !t.is_finished());
            self.voice_stopping.push(v.shutdown(&self.rt));
        }
    }

    /// Every task aborts, and the media stops; the task resolves once its threads have exited.
    pub(super) fn shutdown(mut self) -> (Handle, Vec<ClientTask<()>>) {
        self.stop_voice();
        let stopping = std::mem::take(&mut self.voice_stopping);
        (self.rt.clone(), stopping)
    }
}

async fn perform(api: &ApiClient, op: Op, events: &mpsc::Sender<SessionEvent>) -> Done {
    match op {
        Op::Messages { page, limit } => Done::Messages {
            page,
            result: api.messages(page, limit).await,
        },
        Op::Changes { after, to } => Done::Changes {
            to,
            result: api.message_changes(after).await,
        },
        Op::Decide { id, vouch } => {
            let result = if vouch {
                api.vouch(&id).await
            } else {
                api.decline(&id).await
            };
            Done::Decided { id, vouch, result }
        }
        Op::Owner { user, action } => {
            let result = match action {
                OwnerAction::Remove => api.remove_member(&user).await,
                OwnerAction::MakeOwner => api.transfer_ownership(&user).await,
            };
            Done::OwnerActed {
                user,
                action,
                result,
            }
        }
        Op::Upload { key, source } => {
            let progress = throttled_progress(key, events.clone());
            Done::Uploaded {
                key,
                result: api.upload(&source, progress).await,
            }
        }
        Op::Rename(username) => Done::Renamed(
            api.rename(&username)
                .instrument(info_span!("account", op = "rename"))
                .await,
        ),
        Op::SetAvatar { path, max } => Done::Avatar(
            async {
                let source = picture_source(&path, max)?;
                api.set_avatar(&source).await?;
                Ok(())
            }
            .instrument(info_span!("account", op = "set_avatar"))
            .await,
        ),
        Op::RemoveAvatar => Done::Avatar(
            api.remove_avatar()
                .instrument(info_span!("account", op = "remove_avatar"))
                .await
                .map(|_| ())
                .map_err(PictureError::Api),
        ),
        Op::SetIcon { path, max } => Done::Icon(
            async {
                let source = picture_source(&path, max)?;
                Ok(api.set_icon(&source).await?)
            }
            .instrument(info_span!("account", op = "set_icon"))
            .await,
        ),
        Op::RemoveIcon => Done::Icon(
            api.remove_icon()
                .instrument(info_span!("account", op = "remove_icon"))
                .await
                .map_err(PictureError::Api),
        ),
        Op::Leave => Done::Left(
            api.leave()
                .instrument(info_span!("account", op = "leave"))
                .await,
        ),
    }
}

/// The size is checked here against what the Lobby said it takes, so a file that cannot go
/// through is not sent.
fn picture_source(path: &std::path::Path, max: u64) -> Result<DraftSource, PictureError> {
    let source = DraftSource::from_path(path).map_err(|e| match e {
        AttachError::NotAFile | AttachError::Unreadable | AttachError::TooMany => {
            PictureError::Unreadable
        }
        AttachError::TooLarge => PictureError::TooLarge,
    })?;
    if source.size() > max {
        return Err(PictureError::TooLarge);
    }
    Ok(source)
}

fn upload_task_key(draft: DraftId) -> String {
    format!("upload:{draft}")
}

/// Progress for the Session: `try_send`, so a full queue drops it, and at most every
/// [`UPLOAD_PROGRESS_EVERY`]. The final `Uploaded` is awaited, never dropped.
fn throttled_progress(key: UploadKey, events: mpsc::Sender<SessionEvent>) -> Progress {
    let last = std::sync::Mutex::new(None::<Instant>);
    Arc::new(move |sent| {
        let now = Instant::now();
        let mut last = last.lock().expect("progress throttle");
        if last.is_some_and(|at| now.duration_since(at) < UPLOAD_PROGRESS_EVERY) {
            return;
        }
        *last = Some(now);
        let done = Completion(Done::UploadProgress { key, sent });
        let _ = events.try_send(SessionEvent::Completed(done));
    })
}

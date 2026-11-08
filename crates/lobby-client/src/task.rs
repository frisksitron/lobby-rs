//! `ClientTask`: the only kind of future `lobby-client` hands out.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{runtime::Handle, task::JoinError};
use tokio_util::task::AbortOnDropHandle;

/// Work already running on the client's Tokio runtime. Awaiting it needs no reactor, so a GPUI
/// task may await it. Dropping it cancels the work without needing any executor tick; a Tokio
/// worker then drops the future shortly afterwards, so cleanup that must have finished (a
/// closed socket, a released port) is awaited through a `shutdown()` instead.
#[must_use = "dropping a ClientTask aborts its work"]
pub struct ClientTask<T>(Inner<T>);

enum Inner<T> {
    Spawned(AbortOnDropHandle<T>),
    Ready(Option<T>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TaskGone {
    #[error("the task was cancelled")]
    Cancelled,
    #[error("the task panicked")]
    Panicked,
}

impl From<JoinError> for TaskGone {
    fn from(e: JoinError) -> Self {
        if e.is_panic() {
            TaskGone::Panicked
        } else {
            TaskGone::Cancelled
        }
    }
}

impl<T: Send + 'static> ClientTask<T> {
    pub(crate) fn spawn<F>(rt: &Handle, fut: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        Self(Inner::Spawned(AbortOnDropHandle::new(rt.spawn(fut))))
    }

    /// Work spawned already, such as a Session's driver handed over to finish its teardown.
    pub(crate) fn from_handle(handle: AbortOnDropHandle<T>) -> Self {
        Self(Inner::Spawned(handle))
    }
}

impl<T> ClientTask<T> {
    pub fn ready(value: T) -> Self {
        Self(Inner::Ready(Some(value)))
    }

    pub(crate) fn is_finished(&self) -> bool {
        match &self.0 {
            Inner::Spawned(handle) => handle.is_finished(),
            Inner::Ready(_) => true,
        }
    }
}

// Never pin-projects `T`: the value is moved out of the `Option`, so pinning is irrelevant.
impl<T> Unpin for ClientTask<T> {}

impl<T> Future for ClientTask<T> {
    type Output = Result<T, TaskGone>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.0 {
            Inner::Spawned(handle) => Pin::new(handle).poll(cx).map_err(TaskGone::from),
            Inner::Ready(value) => Poll::Ready(Ok(value
                .take()
                .expect("ClientTask polled after completion"))),
        }
    }
}

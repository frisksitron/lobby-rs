//! The session's idle state from the compositor, by `ext-idle-notify-v1`. GPUI owns the window's
//! Wayland connection and does not hand it out, so this opens a second one, used only here.

use std::{io::ErrorKind, sync::mpsc, time::Duration};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
};
use tracing::{debug, info, warn};
use wayland_client::{
    Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle,
    backend::WaylandError,
    delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

use super::publish;

/// The longest the loop sleeps without an event: how soon dropping the owner stops it, and how
/// often the answer is renewed so it does not go stale.
const WAKE_EVERY: Duration = Duration::from_secs(1);
/// What `get_input_idle_notification` needs: input only, so a video player or Screen Share that
/// holds an idle inhibitor does not keep a Member who is away from showing as active.
const INPUT_IDLE_SINCE: u32 = 2;

#[derive(Clone, Copy)]
pub(super) enum Ended {
    /// The owner was dropped.
    Stopped,
    /// The compositor answered and has no `ext-idle-notify-v1`: asking again changes nothing.
    NoProtocol,
    /// There was no connection, no seat, or the connection was lost: a later try may work.
    Failed,
}

/// Why the connection stopped working; it is only logged.
#[derive(Debug, thiserror::Error)]
enum Lost {
    #[error("{0}")]
    Wayland(#[from] WaylandError),
    #[error("{0}")]
    Dispatch(#[from] DispatchError),
    #[error("polling the connection: {0}")]
    Poll(#[from] Errno),
}

#[derive(Default)]
struct State {
    idle: bool,
}

impl Dispatch<ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.idle = true,
            ext_idle_notification_v1::Event::Resumed => state.idle = false,
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore ExtIdleNotifierV1);

/// Publishes "at least `idle_after`" while the compositor says the session is idle and "no time"
/// while it says active, until the owner of `stopped` is dropped.
pub(super) fn watch(idle_after: Duration, stopped: &mpsc::Receiver<()>) -> Ended {
    let connection = match Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(error) => {
            debug!(%error, "no second Wayland connection for the idle time");
            return Ended::Failed;
        }
    };
    let (globals, mut queue) = match registry_queue_init::<State>(&connection) {
        Ok(pair) => pair,
        Err(error) => {
            debug!(%error, "cannot list the compositor's globals");
            return Ended::Failed;
        }
    };
    let qh = queue.handle();
    let notifier = match globals.bind::<ExtIdleNotifierV1, _, _>(&qh, 1..=2, ()) {
        Ok(notifier) => notifier,
        Err(error) => {
            debug!(%error, "the compositor has no ext-idle-notify-v1");
            return Ended::NoProtocol;
        }
    };
    let seat = match globals.bind::<WlSeat, _, _>(&qh, 1..=1, ()) {
        Ok(seat) => seat,
        Err(error) => {
            debug!(%error, "the compositor has no seat");
            return Ended::Failed;
        }
    };
    let millis = u32::try_from(idle_after.as_millis()).unwrap_or(u32::MAX);
    let _notification = if notifier.version() >= INPUT_IDLE_SINCE {
        notifier.get_input_idle_notification(millis, &seat, &qh, ())
    } else {
        notifier.get_idle_notification(millis, &seat, &qh, ())
    };
    info!(
        version = notifier.version(),
        "idle time from the compositor's ext-idle-notify-v1"
    );

    let mut state = State::default();
    loop {
        match serve(&connection, &mut queue, &mut state) {
            Ok(()) => {}
            Err(error) => {
                warn!(%error, "the Wayland connection for the idle time is lost");
                return Ended::Failed;
            }
        }
        publish(Some(if state.idle {
            idle_after
        } else {
            Duration::ZERO
        }));
        // Nothing is ever sent: the channel only closes when the owner is dropped.
        if !matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Ended::Stopped;
        }
    }
}

/// Sends what is queued, dispatches what has arrived, and waits up to [`WAKE_EVERY`] for more.
fn serve(
    connection: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
) -> Result<(), Lost> {
    connection.flush()?;
    queue.dispatch_pending(state)?;
    // `None` means events were queued meanwhile: dispatch them first.
    let Some(guard) = queue.prepare_read() else {
        return Ok(());
    };
    let timeout = Timespec {
        tv_sec: WAKE_EVERY.as_secs().try_into().unwrap_or(1),
        tv_nsec: 0,
    };
    let ready = {
        let fd = guard.connection_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(count) => count > 0,
            Err(Errno::INTR) => false,
            Err(errno) => return Err(errno.into()),
        }
    };
    if ready {
        // A read that finds nothing (`WouldBlock`) is not a failure.
        match guard.read() {
            Ok(_) => {}
            Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
    }
    queue.dispatch_pending(state)?;
    Ok(())
}

//! The session's idle time from GNOME's Mutter, over the session bus. A lost connection is made
//! again.

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use tracing::{debug, info, warn};
use zbus::blocking::{Connection, connection::Builder};

use super::{POLL, publish};

const MUTTER: &str = "org.gnome.Mutter.IdleMonitor";
const CORE_MONITOR: &str = "/org/gnome/Mutter/IdleMonitor/Core";

/// A call that has no answer by then is a failure and drops the connection.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Between a failed connection and the next try.
const RETRY: Duration = Duration::from_secs(30);

enum Link {
    Down { retry_at: Instant },
    Up(Connection),
}

/// Asks every [`POLL`] until the owner of `stopped` is dropped.
pub(super) fn watch(stopped: &mpsc::Receiver<()>) {
    let mut link = Link::Down {
        retry_at: Instant::now(),
    };
    loop {
        link = match link {
            Link::Down { retry_at } if Instant::now() < retry_at => Link::Down { retry_at },
            Link::Down { .. } => connect(),
            Link::Up(connection) => ask(connection),
        };
        // Nothing is ever sent: the wait ends early only when the owner is dropped.
        if !matches!(
            stopped.recv_timeout(POLL),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            publish(None);
            return;
        }
    }
}

fn connect() -> Link {
    let built = Builder::session().and_then(|builder| builder.method_timeout(CALL_TIMEOUT).build());
    match built {
        Ok(connection) => {
            info!("connected to the session bus for the idle time");
            ask(connection)
        }
        Err(error) => {
            warn!(%error, "no session bus: idle presence is off for now");
            publish(None);
            Link::Down {
                retry_at: Instant::now() + RETRY,
            }
        }
    }
}

fn ask(connection: Connection) -> Link {
    let reply =
        connection.call_method(Some(MUTTER), CORE_MONITOR, Some(MUTTER), "GetIdletime", &());
    match reply {
        Ok(reply) => match reply.body().deserialize::<u64>() {
            Ok(millis) => {
                debug!(millis, "session idle time");
                publish(Some(Duration::from_millis(millis)));
            }
            Err(error) => {
                debug!(%error, "Mutter's idle time is not a number");
                publish(None);
            }
        },
        // The bus answered, so the connection is good: Mutter is not there (it restarts, or this
        // is not GNOME). It is asked again at the next poll.
        Err(zbus::Error::MethodError(name, ..)) => {
            debug!(%name, "Mutter's idle monitor is not on the bus");
            publish(None);
        }
        Err(error) => {
            warn!(%error, "the session bus does not answer: connecting again");
            publish(None);
            return Link::Down {
                retry_at: Instant::now() + RETRY,
            };
        }
    }
    Link::Up(connection)
}

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use lobby_protocol::{ClientFrame, RawClientFrame};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    ConnEvent, Connection, SessionEvent,
    connection::{Command, Signals, SocketId},
};

/// Room for a test to emit events before anything drains them.
const FAKE_EVENT_QUEUE: usize = 4 * crate::connection::EVENT_QUEUE;

pub struct FakeConn {
    events_tx: mpsc::Sender<SessionEvent>,
    events_rx: Option<mpsc::Receiver<SessionEvent>>,
    sent: mpsc::Receiver<Command>,
    signals: Arc<Signals>,
    socket: SocketId,
    cancel: CancellationToken,
}

pub fn fake_connection() -> (Connection, FakeConn) {
    let (cmds_tx, cmds_rx) = mpsc::channel(64);
    let (events_tx, events_rx) = mpsc::channel(FAKE_EVENT_QUEUE);
    let socket: SocketId = Arc::new(AtomicU64::new(1));
    let cancel = CancellationToken::new();
    let signals = Arc::new(Signals::default());
    let conn = Connection::detached(cmds_tx, socket.clone(), cancel.clone(), signals.clone());
    let fake = FakeConn {
        events_tx,
        events_rx: Some(events_rx),
        sent: cmds_rx,
        signals,
        socket,
        cancel,
    };
    (conn, fake)
}

impl FakeConn {
    pub fn take_events(&mut self) -> mpsc::Receiver<SessionEvent> {
        self.events_rx.take().expect("events taken once")
    }

    /// The Session's event channel, for a test that sends from a task of its own.
    pub fn sender(&self) -> mpsc::Sender<SessionEvent> {
        self.events_tx.clone()
    }

    pub fn emit(&self, ev: ConnEvent) {
        self.events_tx
            .try_send(SessionEvent::Conn(ev))
            .expect("event queue has room");
    }

    pub fn push(&self, ev: SessionEvent) {
        self.events_tx.try_send(ev).expect("event queue has room");
    }

    /// The commands the Session sent, then the ink frames still waiting for the current socket.
    pub fn sent(&mut self) -> Vec<ClientFrame> {
        let decode = |text: &str| {
            RawClientFrame::parse(text)
                .and_then(RawClientFrame::decode)
                .expect("the Session sends valid frames")
        };
        let mut frames = Vec::new();
        while let Ok(cmd) = self.sent.try_recv() {
            frames.push(decode(&cmd.text));
        }
        let socket = self.socket.load(Ordering::SeqCst);
        while let Some(text) = self.signals.ink.pop(socket) {
            frames.push(decode(&text));
        }
        frames
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

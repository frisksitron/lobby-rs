//! READY's history at the Session's boundary: what the feed holds after a READY, whatever the
//! Lobby committed or renamed while the READY was being made. The WebSocket is a fake one and
//! REST a `FakeLobby` whose history page is all its Messages.

use std::{sync::Arc, time::Duration};

use lobby_client::{
    ConnEvent, History, Phase, SessionHandle, Snapshot, Updates,
    testing::{
        FakeBehavior, FakeConn, FakeLobby, Observed, Session, frames, inspect, scripted_handle,
    },
};
use lobby_protocol::{
    Message, MessageChanges, MessagePage, Presence, ReadyHistory, ServerMsg, UnixMs,
};
use tokio::sync::Notify;

/// Real waits that only a broken Session runs into.
const WAIT: Duration = Duration::from_secs(5);

struct Scripted {
    handle: SessionHandle,
    updates: Updates,
    snapshot: Arc<Snapshot>,
    fake: FakeConn,
    lobby: FakeLobby,
}

impl Scripted {
    async fn start(history: Vec<Message>) -> Scripted {
        Scripted::with(FakeBehavior {
            history,
            ..FakeBehavior::default()
        })
        .await
    }

    async fn with(behavior: FakeBehavior) -> Scripted {
        let lobby = FakeLobby::start(behavior).await;
        let (handle, updates, fake) = scripted_handle(&lobby);
        Scripted {
            handle,
            updates,
            snapshot: Arc::default(),
            fake,
            lobby,
        }
    }

    async fn history_asked(&mut self) {
        let asked = tokio::time::timeout(WAIT, async {
            while !matches!(
                self.lobby.next_observed().await,
                Observed::MessagesRequested { .. }
            ) {}
        })
        .await;
        assert!(asked.is_ok(), "timed out waiting for a history request");
    }

    /// Until the Session took `n` history pages, the ones it dropped included.
    async fn pages_taken(&self, n: u64) {
        let taken = tokio::time::timeout(WAIT, async {
            while inspect(&self.handle, Session::history_pages).await < n {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(taken.is_ok(), "timed out waiting for {n} history pages");
    }

    fn ready(&self, ready: ServerMsg) {
        self.fake.emit(ConnEvent::Msg(ready));
        self.fake.emit(ConnEvent::Phase(Phase::Connected));
    }

    async fn until(&mut self, what: &str, cond: impl Fn(&Snapshot) -> bool) {
        let waited = tokio::time::timeout(WAIT, async {
            while !cond(&self.snapshot) {
                assert!(self.updates.changed().await, "the Session ended");
                self.snapshot = self.updates.take().snapshot;
            }
        })
        .await;
        assert!(waited.is_ok(), "timed out waiting for {what}");
    }
}

fn message(id: i64, author: &str, content: &str) -> Message {
    match frames::message(id, author, content, &format!("n{id}")) {
        ServerMsg::MessageCreate { message } => message,
        _ => unreachable!("a MESSAGE_CREATE"),
    }
}

/// READY from bob's view, with `history` and the Lobby's newest Message id.
fn ready(history: ReadyHistory, last_message_seq: u64) -> ServerMsg {
    let mut ready = frames::ready("bob", &[("alice", Presence::Online)]);
    if let ServerMsg::Ready(r) = &mut ready {
        r.history = history;
        r.last_message_seq = last_message_seq;
    }
    ready
}

fn latest(messages: Vec<Message>) -> ReadyHistory {
    ReadyHistory::Latest(MessagePage {
        messages,
        has_more: false,
    })
}

/// The Lobby read its empty history, then alice's first Message was committed before bob's
/// registration: READY's page is empty and its `lastMessageSeq` is 1. The feed fetches it.
#[tokio::test]
async fn a_message_committed_while_an_empty_history_was_read_is_fetched() {
    let mut s = Scripted::start(vec![message(1, "alice", "first")]).await;
    s.ready(ready(latest(vec![]), 1));
    s.until("alice's first Message in the feed", |s| {
        s.state.history == History::Loaded && s.state.chat.len() == 1
    })
    .await;
}

/// The Lobby read bob's page, then alice fixed her typo before his registration: the page has the
/// typo and READY's `lastChangeRev` is the fix. The feed catches up from the page's revision.
#[tokio::test]
async fn an_edit_committed_while_the_ready_page_was_read_is_caught_up() {
    let typo = message(1, "alice", "teh cat");
    let fixed = Message {
        content: "the cat".into(),
        edited_at: Some(UnixMs(typo.created_at.0 + 5_000)),
        rev: Some(1),
        ..typo.clone()
    };
    let mut s = Scripted::with(FakeBehavior {
        changes: MessageChanges {
            updated: vec![fixed],
            removed: vec![],
        },
        ..FakeBehavior::default()
    })
    .await;
    let mut ready = ready(latest(vec![typo]), 1);
    if let ServerMsg::Ready(r) = &mut ready {
        r.last_change_rev = 1;
    }
    s.ready(ready);
    s.until("the fix in the feed", |s| {
        s.state.chat.values().any(|m| m.content == "the cat")
    })
    .await;
}

/// alice renamed herself to "alicia" between the history read and the roster READY carries:
/// her Message shows the roster's name, not the page's.
#[tokio::test]
async fn ready_history_read_before_a_rename_shows_the_new_name() {
    let mut s = Scripted::start(Vec::new()).await;
    let mut ready = ready(latest(vec![message(1, "alice", "hi")]), 1);
    if let ServerMsg::Ready(r) = &mut ready {
        let alice = r
            .members
            .iter_mut()
            .find(|m| m.username.as_str() == "alice")
            .expect("alice on the roster");
        alice.username = "alicia".parse().expect("a valid username");
    }
    s.ready(ready);
    s.until("READY taken", |s| s.state.history == History::Loaded)
        .await;
    let author = s.snapshot.state.chat.values().map(|m| &m.author.username);
    assert_eq!(author.collect::<Vec<_>>(), ["alicia"]);
}

/// A backfill asked under one READY answers after the next READY started the feed over (more
/// was missed than the Lobby fills in): the old answer is dropped, so the feed has no hole.
#[tokio::test]
async fn a_backfill_that_answers_after_a_newer_ready_is_dropped() {
    let gate = Arc::new(Notify::new());
    let mut s = Scripted::with(FakeBehavior {
        history: vec![message(2, "alice", "two"), message(3, "alice", "three")],
        history_gate: Some(gate.clone()),
        ..FakeBehavior::default()
    })
    .await;
    s.ready(ready(latest(vec![message(1, "alice", "one")]), 3));
    s.history_asked().await;
    let newest = ReadyHistory::Latest(MessagePage {
        messages: vec![message(5, "alice", "five"), message(6, "alice", "six")],
        has_more: true,
    });
    s.fake.emit(ConnEvent::Msg(ready(newest, 6)));
    let ids = |s: &Snapshot| s.state.chat.keys().map(|id| id.get()).collect::<Vec<_>>();
    s.until("the feed started over", |s| ids(s) == [5, 6]).await;
    gate.notify_one();
    s.pages_taken(1).await;
    let held = inspect(&s.handle, |s| {
        s.state.chat.keys().map(|id| id.get()).collect::<Vec<_>>()
    })
    .await;
    assert_eq!(held, [5, 6], "the old backfill left no Messages behind");
}

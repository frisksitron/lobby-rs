//! Notifications on a Linux desktop, spoken to `org.freedesktop.Notifications` (the Desktop
//! Notifications spec) from a thread of its own: the UI thread only sends commands and reads
//! events, so a bus or a daemon that stalls never stalls a GPUI update. GNOME, KDE, mako and
//! dunst all answer it.
//!
//! What a notification says is [`crate::os_toasts`]'s; this decides how the spec carries it: one
//! notification per tag (a newer one has the older one's id as `replaces_id`), `CloseNotification`
//! when its thing is gone, a `default` action only where the daemon has `actions`, and markup only
//! where it has `body-markup`.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::Duration,
};

use futures_lite::{StreamExt as _, future::or};
use tokio::sync::{
    Notify as Wake,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use zbus::{
    Connection, MatchRule, MessageStream,
    message::{Message, Type},
    zvariant::Value,
};

use crate::{
    host_env::{APP_ID, APP_NAME},
    os_toasts::OsToastKind,
};

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
/// A call with no answer by then fails. Only the worker waits.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// The daemon's own default for how long a banner stays (`expire_timeout` -1).
const DAEMON_TIMEOUT: i32 = -1;
/// A click on the notification itself, which the spec names `default`.
const DEFAULT_ACTION: &str = "default";
const DEFAULT_ACTION_LABEL: &str = "Open";
/// Notifications kept track of, here and in the UI that owns their click handlers. A daemon that
/// never reports one closed does not grow either.
pub const MAX_TRACKED: usize = 64;
/// Commands waiting for the worker, one per tag at most. A stalled daemon answers each call only
/// after [`CALL_TIMEOUT`], so what waits must not grow with the Messages that arrive.
const MAX_PENDING: usize = 64;
/// Lobby plays no sound of its own for a Message or an Ask to Join, and the Windows toast is
/// silent: so the daemon is asked to be too, and a Member in Voice is not rung over.
const SUPPRESS_SOUND: bool = true;

/// One post of a tag. A tag is posted again and again as its author writes on, so an event
/// about an earlier post (it expired, it was dismissed) must not be taken for one about the
/// latest: the generation tells them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub tag: String,
    pub generation: u64,
}

/// What the Member sees: the words, and which thing it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// What a newer notification replaces, and what is taken back with.
    pub tag: String,
    /// Grows with every post the UI makes, under any tag.
    pub generation: u64,
    pub summary: String,
    pub body: String,
    pub kind: OsToastKind,
}

impl Toast {
    fn ticket(&self) -> Ticket {
        Ticket {
            tag: self.tag.clone(),
            generation: self.generation,
        }
    }
}

/// What the daemon says it can show (`GetCapabilities`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub actions: bool,
    pub body: bool,
    pub body_markup: bool,
}

impl Capabilities {
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        let has = |wanted: &str| names.iter().any(|n| n.as_ref() == wanted);
        Self {
            actions: has("actions"),
            body: has("body"),
            body_markup: has("body-markup"),
        }
    }
}

/// One `Notify` call, as it goes on the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notify {
    pub replaces_id: u32,
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// The spec's flat list of id and label pairs.
    pub actions: Vec<&'static str>,
    pub category: &'static str,
    pub suppress_sound: bool,
}

/// The spec's markup subset has three characters that must not pass as markup.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// What goes to a daemon with these `caps`: nothing it cannot show.
pub fn compose(toast: &Toast, caps: Capabilities, replaces_id: u32, icon: &str) -> Notify {
    let body = match (caps.body, caps.body_markup) {
        (false, _) => String::new(),
        (true, true) => escape_markup(&toast.body),
        (true, false) => toast.body.clone(),
    };
    Notify {
        replaces_id,
        icon: icon.to_owned(),
        summary: toast.summary.clone(),
        body,
        actions: if caps.actions {
            vec![DEFAULT_ACTION, DEFAULT_ACTION_LABEL]
        } else {
            Vec::new()
        },
        category: match toast.kind {
            OsToastKind::Message => "im.received",
            OsToastKind::AskToJoin => "im",
        },
        suppress_sound: SUPPRESS_SOUND,
    }
}

/// The click on a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Click {
    pub ticket: Ticket,
}

/// A notification the daemon shows: the id it gave, and the bus name that gave it. Ids are
/// the daemon's own, so one is only meaningful to that daemon.
#[derive(Debug)]
struct Shown {
    ticket: Ticket,
    id: u32,
    owner: String,
}

/// What this app has on screen with the daemon: tag to the id the daemon gave it. A daemon's
/// signal is about its own ids only, so every lookup by id is by the bus name that sent it too.
#[derive(Debug, Default)]
pub struct Desk {
    shown: VecDeque<Shown>,
}

impl Desk {
    /// The id a notification with this tag replaces with the daemon `owner`: 0 when it has none,
    /// or when another daemon gave the id.
    pub fn replaces(&self, tag: &str, owner: &str) -> u32 {
        self.shown
            .iter()
            .find(|s| s.ticket.tag == tag && s.owner == owner)
            .map_or(0, |s| s.id)
    }

    /// The daemon `owner` showed it, under `id`. The tickets no longer tracked to make room,
    /// oldest first: they can no longer be replaced or taken back.
    pub fn posted(&mut self, ticket: Ticket, id: u32, owner: &str) -> Vec<Ticket> {
        self.shown
            .retain(|s| s.ticket.tag != ticket.tag && !s.is(owner, id));
        self.shown.push_back(Shown {
            ticket,
            id,
            owner: owner.to_owned(),
        });
        let mut dropped = Vec::new();
        while self.shown.len() > MAX_TRACKED {
            dropped.extend(self.shown.pop_front().map(|s| s.ticket));
        }
        dropped
    }

    /// `departed` no longer owns the daemon's name (`""`: the name was free, so nobody went).
    /// What it showed is gone with it and is forgotten, and said. Only that: a daemon that
    /// answered since keeps what it showed, however late this is told.
    pub fn departed(&mut self, departed: &str) -> Vec<Ticket> {
        if departed.is_empty() {
            return Vec::new();
        }
        let (gone, kept): (VecDeque<_>, VecDeque<_>) =
            self.shown.drain(..).partition(|s| s.owner == departed);
        self.shown = kept;
        gone.into_iter().map(|s| s.ticket).collect()
    }

    /// Takes the tag back as of `ticket`'s post, and gives the daemon and the id to close: what
    /// the daemon shows from a later post is not what was asked to go.
    pub fn retract(&mut self, ticket: &Ticket) -> Option<(String, u32)> {
        let at = self
            .shown
            .iter()
            .position(|s| s.ticket.tag == ticket.tag && s.ticket.generation <= ticket.generation)?;
        self.shown.remove(at).map(|s| (s.owner, s.id))
    }

    /// Takes back what was shown from every post up to `generation`, oldest first.
    fn sweep(&mut self, generation: u64) -> Vec<Shown> {
        let (swept, kept): (VecDeque<_>, VecDeque<_>) = self
            .shown
            .drain(..)
            .partition(|s| s.ticket.generation <= generation);
        self.shown = kept;
        swept.into()
    }

    /// The daemon `owner` closed its `id` (a click, the Member's dismissal, a timeout, or our
    /// own close).
    pub fn closed(&mut self, owner: &str, id: u32) -> Option<Ticket> {
        let at = self.shown.iter().position(|s| s.is(owner, id))?;
        self.shown.remove(at).map(|s| s.ticket)
    }

    /// The bus or the daemon went away: its ids mean nothing now. What was forgotten, oldest
    /// first.
    pub fn forget_all(&mut self) -> Vec<Ticket> {
        self.shown.drain(..).map(|s| s.ticket).collect()
    }

    /// `ActionInvoked(id, action)` from the daemon `owner`: a click on one of ours.
    pub fn invoked(&self, owner: &str, id: u32, action: &str) -> Option<Click> {
        if action != DEFAULT_ACTION {
            return None;
        }
        let ticket = self.shown.iter().find(|s| s.is(owner, id))?.ticket.clone();
        Some(Click { ticket })
    }
}

impl Shown {
    fn is(&self, owner: &str, id: u32) -> bool {
        self.owner == owner && self.id == id
    }
}

/// What the worker tells the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Clicked(Click),
    /// The daemon does not show it: whoever closed it, and also a post that was never delivered
    /// (no bus, no daemon, a call that failed) or that was let go before it was sent.
    Gone(Ticket),
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Post(Toast),
    Retract(Ticket),
    /// Takes back what was shown from every post up to this generation: the retracts that did
    /// not fit in the queue, as one.
    Sweep(u64),
}

/// What waits for the worker, oldest first in each part, so a daemon that stalls does not make
/// Messages pile up. Posts can be let go: past [`MAX_PENDING`] the oldest is, and the UI is told.
/// A retract cannot: the UI has already let go of the banner's click handler, so a banner it
/// misses stays on the desktop with nothing that can take it back. Retracts are kept apart and
/// sent first; past [`MAX_PENDING`] the oldest become one [`Command::Sweep`], which takes back
/// more than was asked rather than less.
#[derive(Debug, Default)]
struct Pending {
    /// The latest post of each tag.
    posts: VecDeque<Toast>,
    /// The latest retract of each tag.
    retracts: VecDeque<Ticket>,
    /// The newest generation among the retracts that did not fit.
    sweep: Option<u64>,
}

impl Pending {
    /// A newer post replaces a queued older one, and a retract a queued post (it still closes
    /// what the daemon shows under the tag). The posts let go to make room, which were never
    /// shown.
    fn push(&mut self, command: Command) -> Vec<Ticket> {
        match command {
            Command::Post(toast) => {
                self.posts.retain(|queued| queued.tag != toast.tag);
                self.posts.push_back(toast);
            }
            Command::Retract(ticket) => {
                self.posts.retain(|queued| {
                    queued.tag != ticket.tag || queued.generation > ticket.generation
                });
                self.retracts.retain(|queued| queued.tag != ticket.tag);
                self.retracts.push_back(ticket);
                while self.retracts.len() > MAX_PENDING {
                    let oldest = self.retracts.pop_front().map(|t| t.generation);
                    self.sweep = self.sweep.max(oldest);
                }
            }
            Command::Sweep(generation) => self.sweep = self.sweep.max(Some(generation)),
        }
        let mut dropped = Vec::new();
        while self.posts.len() > MAX_PENDING {
            dropped.extend(self.posts.pop_front().map(|toast| toast.ticket()));
        }
        dropped
    }

    fn take(&mut self) -> Option<Command> {
        if let Some(ticket) = self.retracts.pop_front() {
            return Some(Command::Retract(ticket));
        }
        if let Some(generation) = self.sweep.take() {
            return Some(Command::Sweep(generation));
        }
        self.posts.pop_front().map(Command::Post)
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What the owner and the worker share. `stop` is the owner's way to end the worker that does
/// not go through the queue: it also interrupts a call the daemon has not answered.
struct Shared {
    pending: Mutex<Pending>,
    wake: Wake,
    stop: CancellationToken,
    events: UnboundedSender<Event>,
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, command: Command) {
        if self.stop.is_cancelled() {
            return;
        }
        let dropped = self.pending().push(command);
        if !dropped.is_empty() {
            debug!(
                count = dropped.len(),
                "notifications wait for the daemon: oldest let go"
            );
        }
        // Never shown, so the UI is told it is not on the desktop.
        tell_gone(&self.events, dropped);
        self.wake.notify_one();
    }

    /// The next command, `None` when there is none or the owner has gone.
    fn take(&self) -> Option<Command> {
        if self.stop.is_cancelled() {
            return None;
        }
        self.pending().take()
    }
}

/// The worker's owner: dropping it stops the worker at once, even in a call to a stalled daemon,
/// and what is still queued is not sent. The thread is not joined.
pub struct Notifier {
    shared: Arc<Shared>,
}

impl Drop for Notifier {
    fn drop(&mut self) {
        self.shared.stop.cancel();
        self.shared.pending().clear();
    }
}

impl Notifier {
    /// `None` when there is no thread for it: the toast is then lost, as with no daemon.
    pub fn start() -> Option<(Self, UnboundedReceiver<Event>)> {
        let (notifier, events, _thread) = Self::spawn()?;
        Some((notifier, events))
    }

    fn spawn() -> Option<(Self, UnboundedReceiver<Event>, thread::JoinHandle<()>)> {
        let (events, event_rx) = unbounded_channel();
        let shared = Arc::new(Shared {
            pending: Mutex::default(),
            wake: Wake::new(),
            stop: CancellationToken::new(),
            events,
        });
        let worker = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("notifications".into())
            .spawn(move || zbus::block_on(work(&worker)));
        match spawned {
            Ok(thread) => Some((Self { shared }, event_rx, thread)),
            Err(error) => {
                warn!(%error, "no thread for notifications: they are off");
                None
            }
        }
    }

    pub fn post(&self, toast: Toast) {
        self.shared.push(Command::Post(toast));
    }

    /// Takes back what `ticket`'s post, or an earlier one under its tag, put on the desktop.
    pub fn retract(&self, ticket: Ticket) {
        self.shared.push(Command::Retract(ticket));
    }
}

struct Bus {
    conn: Connection,
    signals: MessageStream,
    owner: MessageStream,
}

enum Input {
    Command(Command),
    Signal(Message),
    /// The bus name that no longer owns the daemon's name: empty when the name was free.
    /// `None` when the change cannot be read.
    Owner(Option<String>),
    BusGone,
    Closed,
}

async fn connect() -> zbus::Result<Bus> {
    let conn = zbus::connection::Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()
        .await?;
    let signals = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(DEST)?
        .interface(DEST)?
        .path(PATH)?
        .build();
    let owner = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(DBUS)?
        .interface(DBUS)?
        .member("NameOwnerChanged")?
        .arg(0, DEST)?
        .build();
    Ok(Bus {
        signals: MessageStream::for_match_rule(signals, &conn, None).await?,
        owner: MessageStream::for_match_rule(owner, &conn, None).await?,
        conn,
    })
}

/// Ends `work` when the owner stops the worker, which is what `None` says.
async fn unless_stopped<T>(shared: &Shared, work: impl Future<Output = T>) -> Option<T> {
    let stopped = async {
        shared.stop.cancelled().await;
        None
    };
    or(stopped, async { Some(work.await) }).await
}

async fn next_input(shared: &Shared, bus: Option<&mut Bus>) -> Input {
    let stopped = async {
        shared.stop.cancelled().await;
        Input::Closed
    };
    let command = async {
        loop {
            if let Some(command) = shared.take() {
                return Input::Command(command);
            }
            shared.wake.notified().await;
        }
    };
    let Some(bus) = bus else {
        return or(stopped, command).await;
    };
    let signal = async {
        match bus.signals.next().await {
            Some(Ok(message)) => Input::Signal(message),
            _ => Input::BusGone,
        }
    };
    let owner = async {
        match bus.owner.next().await {
            Some(Ok(message)) => Input::Owner(departed(&message)),
            _ => Input::BusGone,
        }
    };
    // Signals come before commands, so a backlog never keeps a click or a close waiting.
    or(stopped, or(signal, or(owner, command))).await
}

/// `NameOwnerChanged(name, old_owner, new_owner)`'s old owner. The event can come late, after a
/// newer daemon has already answered, so it says only who went, not who is there now.
fn departed(message: &Message) -> Option<String> {
    message
        .body()
        .deserialize::<(String, String, String)>()
        .map(|(_, old, _)| old)
        .ok()
}

/// The bus name a reply or a signal came from: the daemon itself, whatever name it was called by.
fn sender_of(message: &Message) -> String {
    message
        .header()
        .sender()
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// What the worker knows about the daemon.
struct Daemon {
    caps: Option<Capabilities>,
    /// The bus name that told us `caps`.
    caps_from: String,
    /// The "no daemon" line is logged once until one answers.
    complained: bool,
    icon: String,
}

async fn work(shared: &Shared) {
    let events = &shared.events;
    let mut bus: Option<Bus> = None;
    let mut desk = Desk::default();
    let mut daemon = Daemon {
        caps: None,
        caps_from: String::new(),
        complained: false,
        icon: String::new(),
    };
    loop {
        match next_input(shared, bus.as_mut()).await {
            Input::Closed => return,
            Input::BusGone => {
                warn!("the session bus went away: notifications start again at the next one");
                bus = None;
                daemon.caps = None;
                tell_gone(events, desk.forget_all());
            }
            Input::Owner(Some(departed)) => {
                debug!("the notification daemon changed");
                if daemon.caps_from == departed {
                    daemon.caps = None;
                }
                tell_gone(events, desk.departed(&departed));
            }
            Input::Owner(None) => {
                warn!("an unreadable change of the notification daemon: what it showed is let go");
                daemon.caps = None;
                tell_gone(events, desk.forget_all());
            }
            Input::Signal(message) => signal(&message, &mut desk, events),
            Input::Command(Command::Post(toast)) => {
                if bus.is_none() {
                    let Some(connected) = unless_stopped(shared, connect()).await else {
                        return;
                    };
                    match connected {
                        Ok(connected) => bus = Some(connected),
                        Err(error) => {
                            if !daemon.complained {
                                warn!(%error, "no session bus: notifications are lost");
                                daemon.complained = true;
                            }
                            tell_gone(events, [toast.ticket()]);
                            continue;
                        }
                    }
                }
                let Some(bus) = &bus else {
                    continue;
                };
                let Some(delivered) = unless_stopped(
                    shared,
                    post(&bus.conn, &mut daemon, &mut desk, events, &toast),
                )
                .await
                else {
                    return;
                };
                if !delivered {
                    tell_gone(events, [toast.ticket()]);
                }
            }
            Input::Command(Command::Retract(ticket)) => {
                let (Some(bus), Some((owner, id))) = (&bus, desk.retract(&ticket)) else {
                    continue;
                };
                if unless_stopped(shared, close(&bus.conn, &owner, id))
                    .await
                    .is_none()
                {
                    return;
                }
            }
            Input::Command(Command::Sweep(generation)) => {
                let swept = desk.sweep(generation);
                // The UI may still hold handlers for some of these: it hears they are gone.
                tell_gone(events, swept.iter().map(|s| s.ticket.clone()));
                let Some(bus) = &bus else {
                    continue;
                };
                for shown in swept {
                    if unless_stopped(shared, close(&bus.conn, &shown.owner, shown.id))
                        .await
                        .is_none()
                    {
                        return;
                    }
                }
            }
        }
    }
}

/// Closes `id` with the daemon `owner` that showed it: the id means nothing to another daemon,
/// or something else.
async fn close(conn: &Connection, owner: &str, id: u32) {
    let args = (id,);
    let closed = conn
        .call_method(Some(owner), PATH, Some(DEST), "CloseNotification", &args)
        .await;
    if let Err(error) = closed {
        debug!(%error, "the daemon did not close a notification");
    }
}

fn tell_gone(events: &UnboundedSender<Event>, tickets: impl IntoIterator<Item = Ticket>) {
    for ticket in tickets {
        let _ = events.send(Event::Gone(ticket));
    }
}

fn signal(message: &Message, desk: &mut Desk, events: &UnboundedSender<Event>) {
    let header = message.header();
    let Some(member) = header.member() else {
        return;
    };
    // A signal queued by a daemon that has since gone names its own ids, which a newer daemon
    // may have given out again.
    let owner = sender_of(message);
    let body = message.body();
    match member.as_str() {
        "ActionInvoked" => {
            if let Ok((id, action)) = body.deserialize::<(u32, String)>()
                && let Some(click) = desk.invoked(&owner, id, &action)
            {
                let _ = events.send(Event::Clicked(click));
            }
        }
        "NotificationClosed" => {
            if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>()
                && let Some(ticket) = desk.closed(&owner, id)
            {
                let _ = events.send(Event::Gone(ticket));
            }
        }
        _ => {}
    }
}

async fn post(
    conn: &Connection,
    daemon: &mut Daemon,
    desk: &mut Desk,
    events: &UnboundedSender<Event>,
    toast: &Toast,
) -> bool {
    let Some((owner, caps)) = capabilities(conn, daemon).await else {
        return false;
    };
    let call = compose(toast, caps, desk.replaces(&toast.tag, &owner), &daemon.icon);
    let hints: HashMap<&str, Value<'_>> = HashMap::from([
        ("desktop-entry", Value::from(APP_ID)),
        ("category", Value::from(call.category)),
        ("suppress-sound", Value::from(call.suppress_sound)),
    ]);
    let reply = conn
        .call_method(
            Some(owner.as_str()),
            PATH,
            Some(DEST),
            "Notify",
            &(
                APP_NAME,
                call.replaces_id,
                call.icon.as_str(),
                call.summary.as_str(),
                call.body.as_str(),
                call.actions,
                hints,
                DAEMON_TIMEOUT,
            ),
        )
        .await;
    match reply.map(|r| r.body().deserialize::<u32>()) {
        Ok(Ok(id)) => {
            tell_gone(events, desk.posted(toast.ticket(), id, &owner));
            true
        }
        Ok(Err(error)) => {
            warn!(%error, "the daemon's answer to a notification is not an id");
            false
        }
        Err(error) => {
            warn!(%error, "the daemon did not take a notification");
            daemon.caps = None;
            tell_gone(events, desk.forget_all());
            false
        }
    }
}

/// The daemon that has the name now, by its unique name, and what it can do, asked once per
/// daemon. The call that follows goes to that name: an id or a capability one daemon gave means
/// nothing to the one that took the name after it. `None` when there is none on the bus: one log
/// line, and the next notification asks again, which costs a refusal from the bus.
async fn capabilities(conn: &Connection, daemon: &mut Daemon) -> Option<(String, Capabilities)> {
    let owner = conn
        .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "GetNameOwner", &(DEST,))
        .await
        .ok()
        .and_then(|r| r.body().deserialize::<String>().ok());
    if let (Some(owner), Some(caps)) = (&owner, daemon.caps)
        && daemon.caps_from == *owner
    {
        return Some((owner.clone(), caps));
    }
    // Nobody has the name yet: a call to the name itself has the bus start a daemon it can.
    let to = owner.as_deref().unwrap_or(DEST);
    let reply = conn
        .call_method(Some(to), PATH, Some(DEST), "GetCapabilities", &())
        .await;
    match reply.map(|r| (sender_of(&r), r.body().deserialize::<Vec<String>>())) {
        Ok((owner, Ok(names))) => {
            let caps = Capabilities::from_names(&names);
            info!(?caps, "notification daemon answers");
            daemon.caps = Some(caps);
            daemon.caps_from.clone_from(&owner);
            daemon.complained = false;
            if daemon.icon.is_empty() {
                daemon.icon = icon();
            }
            Some((owner, caps))
        }
        Ok((_, Err(error))) => {
            warn!(%error, "the notification daemon's capabilities are unreadable");
            None
        }
        Err(error) => {
            if !daemon.complained {
                warn!(%error, "no notification daemon on the bus: notifications are lost");
                daemon.complained = true;
            } else {
                debug!("a notification is dropped: still no daemon");
            }
            None
        }
    }
}

/// The icon's theme name once Lobby is installed, else the file of the launcher icon, written
/// where only this user can see it: a build run from `target` has no installed icon to name.
fn icon() -> String {
    if super::install::icon_installed() {
        return APP_ID.to_owned();
    }
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        return String::new();
    };
    let file = dir.join(format!("{APP_ID}.png"));
    let current = fs::read(&file).is_ok_and(|bytes| bytes == super::install::ICON);
    if !current && let Err(error) = fs::write(&file, super::install::ICON) {
        debug!(%error, "no file for the notification icon");
        return String::new();
    }
    file.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(tag: &str, generation: u64, body: &str, kind: OsToastKind) -> Toast {
        Toast {
            tag: tag.into(),
            generation,
            summary: "alice".into(),
            body: body.into(),
            kind,
        }
    }

    fn ticket(tag: &str, generation: u64) -> Ticket {
        Ticket {
            tag: tag.into(),
            generation,
        }
    }

    const DAEMON: &str = ":1.7";

    const ALL: Capabilities = Capabilities {
        actions: true,
        body: true,
        body_markup: true,
    };

    #[test]
    fn a_daemon_gets_only_what_it_can_show() {
        let message = toast("m", 1, "a <b> & c", OsToastKind::Message);

        let rich = compose(&message, ALL, 0, "icon");
        assert_eq!(
            rich.body, "a &lt;b&gt; &amp; c",
            "markup escaped, not sent as markup"
        );
        assert_eq!(rich.actions, ["default", "Open"]);
        assert_eq!(rich.category, "im.received");
        assert!(rich.suppress_sound);

        let plain = Capabilities {
            body_markup: false,
            ..ALL
        };
        assert_eq!(compose(&message, plain, 0, "").body, "a <b> & c");

        let bare = Capabilities::from_names::<&str>(&[]);
        let call = compose(&message, bare, 0, "");
        assert_eq!((call.body.as_str(), call.actions.len()), ("", 0));
        assert_eq!(call.summary, "alice");

        let ask = toast("d", 2, "bob's laptop", OsToastKind::AskToJoin);
        assert_eq!(compose(&ask, ALL, 0, "").category, "im");

        assert_eq!(
            Capabilities::from_names(&["actions", "body", "sound", "body-markup"]),
            ALL
        );
    }

    #[test]
    fn a_newer_notification_replaces_the_older_until_it_is_gone() {
        let mut desk = Desk::default();
        assert_eq!(desk.replaces("m/alice", DAEMON), 0);
        desk.posted(ticket("m/alice", 1), 7, DAEMON);
        desk.posted(ticket("m/bob", 2), 8, DAEMON);
        assert_eq!(desk.replaces("m/alice", DAEMON), 7);
        // The daemon may give a replacement another id: the tag follows it.
        desk.posted(ticket("m/alice", 3), 9, DAEMON);
        assert_eq!(desk.replaces("m/alice", DAEMON), 9);
        assert_eq!(desk.replaces("m/bob", DAEMON), 8);

        // The Member dismissed it: the next one is new, not a replacement.
        assert_eq!(desk.closed(DAEMON, 9), Some(ticket("m/alice", 3)));
        assert_eq!(desk.replaces("m/alice", DAEMON), 0);
        assert_eq!(desk.closed(DAEMON, 9), None);

        // Seen in the app: the daemon and the id to close, once.
        assert_eq!(desk.retract(&ticket("m/bob", 2)), Some((DAEMON.into(), 8)));
        assert_eq!(desk.retract(&ticket("m/bob", 2)), None);
        assert_eq!(desk.replaces("m/bob", DAEMON), 0);

        desk.posted(ticket("m/carol", 4), 3, DAEMON);
        assert_eq!(desk.forget_all(), [ticket("m/carol", 4)]);
        assert_eq!(desk.replaces("m/carol", DAEMON), 0);

        // Past what it tracks, the oldest are let go, and said: the UI forgets them too.
        let mut dropped = Vec::new();
        for i in 0..=MAX_TRACKED {
            let ticket = ticket(&format!("n/{i}"), 10 + i as u64);
            dropped.extend(desk.posted(ticket, 100 + i as u32, DAEMON));
        }
        assert_eq!(dropped, [ticket("n/0", 10)]);
        assert_eq!(desk.replaces("n/0", DAEMON), 0);
        assert_eq!(desk.replaces("n/1", DAEMON), 101);
    }

    #[test]
    fn a_retract_takes_back_only_posts_up_to_its_own() {
        let mut desk = Desk::default();
        desk.posted(ticket("m/alice", 5), 7, DAEMON);
        // The UI asked to retract generation 4 (an older post): generation 5 is not what it meant.
        assert_eq!(desk.retract(&ticket("m/alice", 4)), None);
        assert_eq!(
            desk.retract(&ticket("m/alice", 5)),
            Some((DAEMON.into(), 7))
        );
    }

    fn post_of(tag: &str, generation: u64, body: &str) -> Command {
        Command::Post(toast(tag, generation, body, OsToastKind::Message))
    }

    #[test]
    fn what_waits_for_the_daemon_is_the_latest_per_tag_and_bounded() {
        let mut pending = Pending::default();
        pending.push(post_of("m/alice", 1, "one"));
        pending.push(post_of("m/bob", 2, "hi"));
        // A newer post replaces the queued older one and goes behind the others.
        pending.push(post_of("m/alice", 3, "two"));
        // A retract takes the queued post's place, so nothing is shown for it, and goes first.
        pending.push(Command::Retract(ticket("m/bob", 2)));
        assert_eq!(pending.take(), Some(Command::Retract(ticket("m/bob", 2))));
        assert_eq!(pending.take(), Some(post_of("m/alice", 3, "two")));
        assert_eq!(pending.take(), None);

        // A stalled daemon: a Message from each of many authors, and the oldest are let go.
        let mut dropped = 0;
        for i in 0..MAX_PENDING * 10 {
            dropped += pending
                .push(post_of(&format!("m/{i}"), 10 + i as u64, "x"))
                .len();
        }
        assert_eq!(pending.posts.len(), MAX_PENDING);
        assert_eq!(dropped, MAX_PENDING * 9);
        let first = MAX_PENDING * 9;
        let wanted = post_of(&format!("m/{first}"), 10 + first as u64, "x");
        assert_eq!(pending.take(), Some(wanted));
    }

    fn shared() -> (Arc<Shared>, UnboundedReceiver<Event>) {
        let (events, rx) = unbounded_channel();
        let shared = Shared {
            pending: Mutex::default(),
            wake: Wake::new(),
            stop: CancellationToken::new(),
            events,
        };
        (Arc::new(shared), rx)
    }

    #[test]
    fn a_post_let_go_before_it_was_sent_is_told_to_the_ui() {
        let (shared, mut events) = shared();
        for i in 0..=MAX_PENDING {
            shared.push(post_of(&format!("m/{i}"), 1 + i as u64, "x"));
        }
        // The queue holds the limit; the oldest never reached the daemon, and the UI hears it.
        assert_eq!(events.try_recv(), Ok(Event::Gone(ticket("m/0", 1))));
        assert!(events.try_recv().is_err(), "only that one");
        // A retract that takes a queued post's place: the UI asked for it, nothing to tell.
        shared.push(Command::Retract(ticket("m/1", 2)));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_retract_is_never_let_go() {
        let (shared, _events) = shared();
        // Many banners shown, and the worker waits on a stalled daemon. Each new mention makes
        // the UI let go of the oldest handler (a retract) and post (the review's burst).
        let burst = MAX_PENDING / 2 + 1;
        for i in 0..burst {
            shared.push(Command::Retract(ticket(&format!("old/{i}"), 1 + i as u64)));
            shared.push(post_of(&format!("new/{i}"), 1000 + i as u64, "x"));
        }
        let taken: Vec<_> = std::iter::from_fn(|| shared.take()).collect();
        let retracts = taken
            .iter()
            .filter(|c| matches!(c, Command::Retract(_)))
            .count();
        assert_eq!(retracts, burst, "every retract is sent, before the posts");
        assert!(matches!(taken[0], Command::Retract(_)));

        // Past what the queue holds, the oldest retracts become one sweep: more is taken back
        // than was asked, never less.
        let mut desk = Desk::default();
        for i in 0..MAX_TRACKED {
            desk.posted(
                ticket(&format!("old/{i}"), 1 + i as u64),
                1 + i as u32,
                DAEMON,
            );
        }
        // Every one of them retracted, then a few the desk never showed.
        for i in 0..MAX_PENDING + 3 {
            shared.push(Command::Retract(ticket(&format!("old/{i}"), 1 + i as u64)));
        }
        let mut closed = 0;
        while let Some(command) = shared.take() {
            closed += match command {
                Command::Retract(t) => usize::from(desk.retract(&t).is_some()),
                Command::Sweep(generation) => desk.sweep(generation).len(),
                Command::Post(_) => 0,
            };
        }
        assert_eq!(closed, MAX_TRACKED, "every banner the UI let go is closed");
    }

    #[test]
    fn dropping_the_notifier_ends_the_worker_without_sending_what_is_queued() {
        // A backlog, then the owner goes. The worker is run here, on this thread, after that:
        // it must return at once. A worker that drained the queue would connect to the session
        // bus and post each of these.
        let (shared, _events) = shared();
        let notifier = Notifier {
            shared: Arc::clone(&shared),
        };
        for i in 0..MAX_PENDING {
            let toast = toast(&format!("m/{i}"), 1 + i as u64, "x", OsToastKind::Message);
            notifier.post(toast);
        }
        drop(notifier);
        assert_eq!(shared.pending().take(), None, "nothing is left queued");
        shared.push(post_of("m/late", 99, "x"));
        assert_eq!(
            shared.take(),
            None,
            "nothing is accepted once the owner is gone"
        );
        shared.pending().push(post_of("m/queued", 100, "x"));
        zbus::block_on(work(&shared));

        // And the thread of a Notifier with nothing to do ends when its owner is dropped.
        let (notifier, _events, worker) = Notifier::spawn().expect("a thread");
        drop(notifier);
        worker.join().expect("the worker ends");
    }

    #[test]
    fn a_daemon_that_goes_takes_only_what_it_showed() {
        let mut desk = Desk::default();
        desk.posted(ticket("m/old", 1), 3, ":1.5");
        // The old daemon went and a new one answered a Notify before either change was told.
        desk.posted(ticket("m/new", 2), 4, ":1.9");
        // Told late: the name was free, then the new daemon took it.
        assert_eq!(desk.departed(":1.5"), [ticket("m/old", 1)]);
        assert_eq!(desk.departed(""), []);
        assert_eq!(
            desk.replaces("m/new", ":1.9"),
            4,
            "the new daemon's entry survives"
        );
        assert_eq!(desk.replaces("m/old", ":1.5"), 0);
        assert!(desk.invoked(":1.9", 4, "default").is_some());

        // The new daemon goes away too: nothing it showed can be replaced or closed any more.
        desk.posted(ticket("m/new", 2), 4, ":1.9");
        assert_eq!(desk.departed(":1.9"), [ticket("m/new", 2)]);
        assert_eq!(desk.retract(&ticket("m/new", 2)), None);
    }

    /// A daemon's signal as the bus hands it over: from the daemon's own bus name.
    fn from<'a>(sender: &'a str, member: &'a str) -> zbus::message::Builder<'a> {
        Message::signal(PATH, DEST, member)
            .and_then(|m| m.sender(sender))
            .expect("a signal")
    }

    #[test]
    fn a_signal_from_an_earlier_daemon_does_not_touch_the_new_ones_id() {
        let mut desk = Desk::default();
        // The new daemon gave out id 1, which the old one also had.
        desk.posted(ticket("m/new", 2), 1, ":1.9");
        let (events, mut rx) = unbounded_channel();
        // Queued by the old daemon before it went.
        let old = [
            from(":1.5", "ActionInvoked").build(&(1u32, "default")),
            from(":1.5", "NotificationClosed").build(&(1u32, 2u32)),
        ];
        for message in old {
            signal(&message.expect("a signal"), &mut desk, &events);
        }
        assert!(rx.try_recv().is_err(), "nothing reaches the UI");
        assert_eq!(desk.replaces("m/new", ":1.9"), 1);

        // The new daemon's own click does.
        let click = from(":1.9", "ActionInvoked").build(&(1u32, "default"));
        signal(&click.expect("a signal"), &mut desk, &events);
        let click = Click {
            ticket: ticket("m/new", 2),
        };
        assert_eq!(rx.try_recv(), Ok(Event::Clicked(click)));
        let closed = from(":1.9", "NotificationClosed").build(&(1u32, 2u32));
        signal(&closed.expect("a signal"), &mut desk, &events);
        assert_eq!(rx.try_recv(), Ok(Event::Gone(ticket("m/new", 2))));
    }

    #[test]
    fn a_click_is_on_ours_and_on_the_default_action() {
        let mut desk = Desk::default();
        desk.posted(ticket("m/alice", 1), 7, DAEMON);

        let click = desk.invoked(DAEMON, 7, "default").expect("a click on ours");
        assert_eq!(click.ticket, ticket("m/alice", 1));
        assert_eq!(desk.invoked(DAEMON, 99, "default"), None);
        assert_eq!(desk.invoked(DAEMON, 7, "something-else"), None);
    }

    /// One call as the daemon's side of the socket saw it: to whom, which, and its `replaces_id`.
    type Seen = (String, String, u32);

    /// Stands in for the bus and for the daemon `owner` that now has the name, on the other end
    /// of a socket: answers every call as `owner`, until `notifies` Notify calls are answered.
    async fn daemon_now(conn: Connection, owner: &str, notifies: usize) -> Vec<Seen> {
        type NotifyArgs = (
            String,
            u32,
            String,
            String,
            String,
            Vec<String>,
            HashMap<String, zbus::zvariant::OwnedValue>,
            i32,
        );
        let mut calls = MessageStream::from(&conn);
        let mut seen = Vec::new();
        while seen.iter().filter(|(_, m, _)| m == "Notify").count() < notifies {
            let call = calls.next().await.expect("a call").expect("readable");
            let header = call.header();
            let member = header.member().expect("a method").to_string();
            let to = header.destination().expect("addressed").to_string();
            let answer = Message::method_return(&header)
                .and_then(|b| b.sender(owner))
                .expect("an answer");
            let (answer, replaces) = match member.as_str() {
                "GetNameOwner" => (answer.build(&(owner,)), 0),
                "GetCapabilities" => (answer.build(&(vec!["actions", "body"],)), 0),
                "Notify" => {
                    let args: NotifyArgs = call.body().deserialize().expect("Notify's arguments");
                    (answer.build(&(42u32,)), args.1)
                }
                other => panic!("unexpected call {other}"),
            };
            conn.send(&answer.expect("built")).await.expect("sent");
            seen.push((to, member, replaces));
        }
        seen
    }

    #[test]
    fn a_replacement_goes_only_to_the_daemon_that_gave_the_id() {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        zbus::block_on(async {
            let (conn, other_end) = futures_lite::future::zip(
                zbus::connection::Builder::async_io_unix_stream(ours)
                    .p2p()
                    .build(),
                zbus::connection::Builder::async_io_unix_stream(theirs)
                    .server(zbus::Guid::generate())
                    .expect("a server")
                    .p2p()
                    .build(),
            )
            .await;
            let (conn, other_end) = (conn.expect("ours"), other_end.expect("theirs"));
            // The daemon `:1.5` showed alice's Message as its id 7, and said it has no body.
            let mut desk = Desk::default();
            desk.posted(ticket("m/alice", 1), 7, ":1.5");
            let mut daemon = Daemon {
                caps: Some(Capabilities::default()),
                caps_from: ":1.5".into(),
                complained: false,
                icon: "icon".into(),
            };
            let (events, _rx) = unbounded_channel();
            // `:1.5` left and `:1.9` took the name before the worker heard of it.
            let posts = async {
                for generation in [2, 3] {
                    let next = toast("m/alice", generation, "hi", OsToastKind::Message);
                    assert!(post(&conn, &mut daemon, &mut desk, &events, &next).await);
                }
            };
            let ((), seen) =
                futures_lite::future::zip(posts, daemon_now(other_end, ":1.9", 2)).await;
            let call = |to: &str, member: &str, replaces| (to.into(), member.into(), replaces);
            assert_eq!(
                seen,
                [
                    call(DBUS, "GetNameOwner", 0),
                    // Its own capabilities, not `:1.5`'s.
                    call(":1.9", "GetCapabilities", 0),
                    // Not `:1.5`'s id 7, which may be one of `:1.9`'s own banners.
                    call(":1.9", "Notify", 0),
                    call(DBUS, "GetNameOwner", 0),
                    // Its own id 42 it does replace.
                    call(":1.9", "Notify", 42),
                ]
            );
        });
    }
}

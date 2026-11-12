//! Entry: reaching a Lobby, from the address to Signed in.
//!
//! [`Entry`] is plain code. It owns one [`EntryStep`] and decides what each Intent and each answer
//! does to it; its driver ([`EntryHandle`]) runs the requests it asks for and its deadlines.
//! Every request carries the epoch of the step it was sent from, a step change starts a new
//! epoch, and an answer from an older epoch changes nothing. A sign in's tokens are stored only
//! once its answer still counts, and Signed in shows only once they are.
//!
//! The way in is one path: the address, an email, its Magic Code. The Lobby then decides what
//! comes next: a Member is signed in, an Ask to Join already filed waits for a Vouch, and anyone
//! else picks a name, which sets up a new Lobby or Asks to Join.

mod code;
mod driver;

use std::time::{Duration, Instant};

use lobby_protocol::{
    Email, ErrorCode, JoinOutcome, JoinRequest, JoinStatus, JoinTicket, LobbyInfo,
    MagicCodeRequested, MagicCodeVerify, RequestLabel, SessionTokens, SignedIn, Username,
    VerifyOutcome, WaitToken,
};
use tracing::{debug, info, warn};

#[cfg(any(test, feature = "test-support"))]
pub use code::RESEND_AFTER;
pub use code::Resend;
pub use driver::{EntryHandle, EntryUpdates};
pub(crate) use driver::{EntryIo, drive};

use crate::{ApiError, ApiKind, HttpPolicy, LobbyInfoError, LobbyOrigin, TokenStoreError, copy};

/// Waiting for a Vouch checks this often by itself, so a Vouch is seen within a few seconds. The
/// Lobby takes 60 checks a minute from one IP: five waiters behind one router.
pub const JOIN_CHECK_EVERY: Duration = Duration::from_secs(5);
/// A 429 without a usable `retryAfterMs` holds the step this long.
const RATE_LIMIT_FALLBACK: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    NotOpenYet,
    Locked,
    Declined,
}

/// What the Lobby said a name is for. Only the screen's words differ: both send the same
/// `JoinRequest`, and the Lobby decides the path again when it takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamePath {
    /// The bootstrap email of a Lobby without Members: the name sets it up, as its owner.
    CreateLobby,
    AskToJoin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStep {
    Address,
    Email,
    Code {
        email: Email,
    },
    Name {
        email: Email,
        ticket: JoinTicket,
        path: NamePath,
    },
    AwaitingVouch {
        email: Email,
        wait: WaitToken,
        /// The name this device asked with; `None` when a code found the request waiting.
        name: Option<Username>,
    },
    Closed {
        why: Closed,
        email: Email,
        name: Option<Username>,
    },
    /// The tokens are stored, or were found stored at the address.
    SignedIn,
}

impl EntryStep {
    fn email(&self) -> Option<&Email> {
        match self {
            Self::Code { email }
            | Self::Name { email, .. }
            | Self::AwaitingVouch { email, .. }
            | Self::Closed { email, .. } => Some(email),
            Self::Address | Self::Email | Self::SignedIn => None,
        }
    }
}

/// A step on the rail of chips across the top of Entry's screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Lobby,
    Email,
    Code,
    /// On the rail only once the Lobby said a name is needed.
    Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub stage: Stage,
    pub at: ChipAt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipAt {
    Done {
        /// What the step settled: the Lobby's host, the email or the name. A code has none.
        value: Option<String>,
        /// Pressing the chip goes back to its step ([`EntryIntent::GoTo`]).
        back: bool,
    },
    Current,
    Ahead,
}

/// The step's one line of feedback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    Error(String),
    Info(String),
    Notice(String),
    Hint(String),
}

/// What a step change puts in a field, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fill {
    Email(Email),
    /// A name suggested from the email, for an empty field.
    Username(String),
}

/// What a view draws of Entry, published after every change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryView {
    /// Moves with every step change: a view applies `fill` and resets its fields once per epoch.
    pub epoch: u64,
    pub step: EntryStep,
    /// The steps so far, the current one and those ahead, in order. Empty once Signed in.
    pub rail: Vec<Chip>,
    pub origin: Option<LobbyOrigin>,
    pub info: Option<LobbyInfo>,
    /// The step's request is in flight.
    pub busy: bool,
    /// The Lobby asked for a pause (429): the step's button waits.
    pub held: bool,
    /// On the code step: the request this device asked last, which the email it checks names.
    pub code_label: Option<RequestLabel>,
    pub said: Option<Said>,
    pub resend: Resend,
    /// How many codes were refused so far; it moves with each one counted.
    pub wrong: u64,
    pub fill: Option<Fill>,
    /// How many Intents Entry has taken: every one [`EntryHandle::dispatch`] numbered up to this.
    pub taken: u64,
}

#[derive(Debug, Clone)]
pub enum EntryIntent {
    /// The address as typed.
    Connect(String),
    /// The field changed since the last answer: an error said was about the old text, and at
    /// the address anything said was.
    Edited,
    SendCode(String),
    Resend,
    SubmitCode(String),
    /// The name as typed, on the Name step.
    PickName(String),
    /// Check now, pressed.
    CheckNow,
    /// The window came back: Waiting for a Vouch checks, as its regular check does.
    WindowActive,
    /// A done chip on the rail, pressed: back to its step, if the chip says it goes back.
    GoTo(Stage),
    Back,
}

#[derive(Debug)]
pub(crate) enum Op {
    Reach(LobbyOrigin),
    Describe(LobbyOrigin),
    RequestCode(LobbyOrigin, Email),
    Verify(LobbyOrigin, MagicCodeVerify),
    Join(LobbyOrigin, JoinRequest),
    Status(LobbyOrigin, WaitToken),
    Store(LobbyOrigin, SessionTokens),
    Clear(LobbyOrigin),
}

#[derive(Debug)]
pub(crate) enum Done {
    Reached(
        Result<LobbyInfo, LobbyInfoError>,
        Result<bool, TokenStoreError>,
    ),
    /// Which Lobby was described: the answer may come after the person went on to another.
    Described(LobbyOrigin, Result<LobbyInfo, LobbyInfoError>),
    CodeSent(Email, Result<MagicCodeRequested, ApiError>),
    Verified(Result<VerifyOutcome, ApiError>),
    /// With the name asked for, which Waiting for a Vouch keeps.
    Joined(Username, Result<JoinOutcome, ApiError>),
    Status(Result<JoinStatus, ApiError>),
    Stored(Result<(), TokenStoreError>),
    Cleared,
}

pub(crate) struct Entry {
    policy: HttpPolicy,
    /// The Lobby the person is in (Add Lobby): reaching it again says so.
    here: Option<(LobbyOrigin, String)>,
    epoch: u64,
    step: EntryStep,
    origin: Option<LobbyOrigin>,
    info: Option<LobbyInfo>,
    busy: bool,
    /// A status check is in flight in this epoch, pressed or not.
    checking: bool,
    next_check: Option<Instant>,
    held_until: Option<Instant>,
    said: Option<Said>,
    fill: Option<Fill>,
    codes: code::Codes,
    /// Tokens being stored that nobody has seen Signed in for yet.
    storing: Option<LobbyOrigin>,
    ops: Vec<(u64, Op)>,
    taken: u64,
}

impl Entry {
    fn new(policy: HttpPolicy, step: EntryStep) -> Self {
        Self {
            policy,
            here: None,
            epoch: 0,
            step,
            origin: None,
            info: None,
            busy: false,
            checking: false,
            next_check: None,
            held_until: None,
            said: None,
            fill: None,
            codes: code::Codes::default(),
            storing: None,
            ops: Vec::new(),
            taken: 0,
        }
    }

    pub fn at_address(policy: HttpPolicy, here: Option<(LobbyOrigin, String)>) -> Self {
        Self {
            here,
            ..Self::new(policy, EntryStep::Address)
        }
    }

    pub fn reached(policy: HttpPolicy, origin: LobbyOrigin, info: LobbyInfo) -> Self {
        let mut entry = Self::new(policy, EntryStep::Email);
        entry.origin = Some(origin);
        entry.info = Some(info);
        entry
    }

    pub fn take_ops(&mut self) -> Vec<(u64, Op)> {
        std::mem::take(&mut self.ops)
    }

    /// Tokens on their way to the store that no step has shown Signed in for: a dropped Entry
    /// clears them once the save is done.
    pub fn storing(&self) -> Option<&LobbyOrigin> {
        self.storing.as_ref()
    }

    pub fn view(&self, now: Instant) -> EntryView {
        let (resend, code_label) = match &self.step {
            EntryStep::Code { email } => (self.resend_state(email, now), self.code_label(email)),
            _ => (Resend::Ready, None),
        };
        EntryView {
            code_label,
            epoch: self.epoch,
            step: self.step.clone(),
            rail: self.rail(),
            origin: self.origin.clone(),
            info: self.info.clone(),
            busy: self.busy,
            // On the Code step only the code's own refusals hold the button: a send pause is the
            // resend link's, in `resend`.
            held: self.held(now) || (self.step == EntryStep::Email && self.sends_held(now)),
            said: self.said.clone(),
            resend,
            wrong: self.codes.wrong,
            fill: self.fill.clone(),
            taken: self.taken,
        }
    }

    /// The next time Entry has something to do or show by itself, after `now`.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let resend = match &self.step {
            EntryStep::Code { email } => self.resend_changes_at(email, now),
            _ => None,
        };
        let sends = self.codes.sends_held_until(self.origin.as_ref(), now);
        [self.held_until, self.next_check, resend, sends]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn tick(&mut self, now: Instant) {
        if self.held_until.is_some_and(|u| u <= now) {
            self.held_until = None;
            self.said = None;
        }
        if self.codes.release_sends(now) && matches!(self.said, Some(Said::Error(_))) {
            self.said = None;
        }
        if self.next_check.is_some_and(|at| at <= now) {
            self.next_check = Some(now + JOIN_CHECK_EVERY);
            self.check(false, now);
        }
    }

    pub fn dispatch(&mut self, intent: EntryIntent, now: Instant) {
        self.taken += 1;
        match intent {
            EntryIntent::Connect(address) => self.connect(&address),
            EntryIntent::Edited if !self.busy => {
                if self.step == EntryStep::Address || matches!(self.said, Some(Said::Error(_))) {
                    self.said = None;
                }
            }
            EntryIntent::Edited => {}
            EntryIntent::SendCode(email) => self.send_code(&email, now),
            EntryIntent::Resend => self.resend(now),
            EntryIntent::SubmitCode(code) => self.submit_code(code, now),
            EntryIntent::PickName(name) => self.pick_name(&name, now),
            EntryIntent::CheckNow => self.check(true, now),
            EntryIntent::WindowActive => self.check(false, now),
            EntryIntent::GoTo(stage) => self.go_to(stage, now),
            EntryIntent::Back => self.back(now),
        }
    }

    pub fn complete(&mut self, epoch: u64, done: Done, now: Instant) {
        match done {
            // A newer description of this Lobby is welcome whenever it comes.
            Done::Described(origin, Ok(info)) => {
                if self.origin.as_ref() == Some(&origin) {
                    self.info = Some(info);
                }
                return;
            }
            Done::Described(_, Err(_)) | Done::Cleared => return,
            _ => {}
        }
        if epoch != self.epoch {
            debug!(
                epoch,
                current = self.epoch,
                "an answer to an earlier step: dropped"
            );
            return;
        }
        match done {
            Done::Reached(info, stored) => self.reached_answer(info, stored, now),
            Done::CodeSent(email, result) => self.code_sent(email, result, now),
            Done::Verified(result) => self.verified(result, now),
            Done::Joined(name, result) => self.joined(name, result, now),
            Done::Status(result) => self.status(result, now),
            Done::Stored(result) => self.stored(result, now),
            Done::Described(..) | Done::Cleared => {}
        }
    }

    fn cap(&self) -> usize {
        self.info
            .as_ref()
            .expect("the Lobby has been reached before requesting codes")
            .magic_code_sends as usize
    }

    fn held(&self, now: Instant) -> bool {
        self.held_until.is_some_and(|u| u > now)
    }

    fn ask(&mut self, op: Op) {
        self.ops.push((self.epoch, op));
    }

    fn reached_origin(&self) -> LobbyOrigin {
        self.origin
            .clone()
            .expect("a step past the address knows its Lobby")
    }

    /// The one place that says which steps are done and which of them a press goes back to:
    /// the Lobby and the email. A code is spent once it is checked, and so is a name once filed.
    fn rail(&self) -> Vec<Chip> {
        use ChipAt::{Ahead, Current};
        let chip = |stage, at| Chip { stage, at };
        let done = |stage, value: Option<&str>| Chip {
            stage,
            at: ChipAt::Done {
                value: value.map(str::to_owned),
                back: matches!(stage, Stage::Lobby | Stage::Email),
            },
        };
        let host = self.origin.as_ref().map(LobbyOrigin::host);
        let lobby = done(Stage::Lobby, host);
        let email = |email: &Email| done(Stage::Email, Some(email.as_str()));
        match &self.step {
            EntryStep::Address => vec![
                chip(Stage::Lobby, Current),
                chip(Stage::Email, Ahead),
                chip(Stage::Code, Ahead),
            ],
            EntryStep::Email => vec![lobby, chip(Stage::Email, Current), chip(Stage::Code, Ahead)],
            EntryStep::Code { email: e } => vec![lobby, email(e), chip(Stage::Code, Current)],
            EntryStep::Name { email: e, .. } => vec![
                lobby,
                email(e),
                done(Stage::Code, None),
                chip(Stage::Name, Current),
            ],
            EntryStep::AwaitingVouch { email: e, name, .. }
            | EntryStep::Closed { email: e, name, .. } => {
                let mut rail = vec![lobby, email(e), done(Stage::Code, None)];
                if let Some(name) = name {
                    rail.push(done(Stage::Name, Some(name.as_str())));
                }
                rail
            }
            EntryStep::SignedIn => Vec::new(),
        }
    }

    /// A new step, and a new epoch: what the old one had in flight no longer counts.
    fn go(&mut self, step: EntryStep, now: Instant) {
        self.epoch += 1;
        self.busy = false;
        self.checking = false;
        self.held_until = None;
        self.said = None;
        self.fill = None;
        self.leave_store();
        self.next_check =
            matches!(step, EntryStep::AwaitingVouch { .. }).then(|| now + JOIN_CHECK_EVERY);
        self.step = step;
        info!(step = ?step_name(&self.step), "entry step");
    }

    /// Tokens whose sign in no step will show: cleared behind the save that runs.
    fn leave_store(&mut self) {
        if let Some(origin) = self.storing.take() {
            info!("a sign in was left while its tokens were stored: clearing them");
            self.ask(Op::Clear(origin));
        }
    }

    /// Starts the step's request, unless one is in flight or the Lobby asked for a pause.
    fn begin(&mut self, now: Instant) -> bool {
        if self.busy || self.held(now) {
            return false;
        }
        self.busy = true;
        self.said = None;
        true
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.busy = false;
        self.said = Some(Said::Error(message.into()));
    }

    /// [`Self::fail`] for a Lobby's answer: a 429 also holds the step until it takes tries again.
    fn fail_api(&mut self, e: &ApiError, message: String, now: Instant) {
        self.hold(e, now);
        self.fail(message);
    }

    fn hold(&mut self, e: &ApiError, now: Instant) {
        if e.kind == ApiKind::Server(ErrorCode::RateLimited) {
            self.held_until = Some(now + e.retry_after.unwrap_or(RATE_LIMIT_FALLBACK));
        }
    }

    fn connect(&mut self, address: &str) {
        if self.step != EntryStep::Address || self.busy {
            return;
        }
        let origin = match LobbyOrigin::parse(address, self.policy) {
            Ok(origin) => origin,
            Err(e) => {
                self.said = Some(Said::Error(e.to_string()));
                return;
            }
        };
        if let Some((here, name)) = &self.here
            && *here == origin
        {
            info!(lobby = %origin, "already in this Lobby");
            self.said = Some(Said::Info(copy::already_in(name)));
            return;
        }
        self.epoch += 1;
        self.busy = true;
        self.said = None;
        self.origin = Some(origin.clone());
        self.info = None;
        self.ask(Op::Reach(origin));
    }

    fn reached_answer(
        &mut self,
        info: Result<LobbyInfo, LobbyInfoError>,
        stored: Result<bool, TokenStoreError>,
        now: Instant,
    ) {
        self.busy = false;
        let origin = self.reached_origin();
        match (info, stored) {
            (Ok(info), Ok(found)) => {
                info!(lobby = %origin, name = %info.name, found, "the Lobby answered");
                self.info = Some(info);
                let next = if found {
                    EntryStep::SignedIn
                } else {
                    EntryStep::Email
                };
                self.go(next, now);
            }
            (Ok(info), Err(e)) => {
                warn!(error = %e, "token store unavailable");
                self.info = Some(info);
                self.said = Some(Said::Error(copy::KEYCHAIN_UNAVAILABLE.into()));
            }
            (Err(e), _) => {
                warn!(lobby = %origin, error = ?e, "lobby info failed");
                self.said = Some(Said::Error(copy::lobby_info_failed(&origin, &e)));
            }
        }
    }

    /// The answer counts: its tokens are stored, and Signed in waits for the store.
    fn store(&mut self, signed_in: SignedIn) {
        info!(user = %signed_in.user.id, "signed in; storing the tokens");
        let origin = self.reached_origin();
        self.busy = true;
        self.storing = Some(origin.clone());
        self.ask(Op::Store(origin, signed_in.tokens));
    }

    fn stored(&mut self, result: Result<(), TokenStoreError>, now: Instant) {
        if self.storing.is_none() {
            return;
        }
        match result {
            Ok(()) => {
                self.storing = None;
                info!("tokens stored");
                self.go(EntryStep::SignedIn, now);
            }
            Err(e) => {
                warn!(error = %e, "token store failed");
                // A save that timed out may still land: the clear queues behind it.
                self.leave_store();
                self.fail(copy::KEYCHAIN_UNAVAILABLE);
            }
        }
    }

    fn pick_name(&mut self, typed: &str, now: Instant) {
        let EntryStep::Name { ticket, .. } = &self.step else {
            return;
        };
        if self.busy || self.held(now) {
            return;
        }
        let ticket = ticket.clone();
        let username = match Username::parse(typed) {
            Ok(u) => u,
            Err(_) => return self.fail(copy::name_rule(typed)),
        };
        self.begin(now);
        let origin = self.reached_origin();
        self.ask(Op::Join(origin, JoinRequest { ticket, username }));
    }

    /// A refusal keeps the name step, so a taken name can be tried again with the same ticket.
    fn joined(&mut self, name: Username, result: Result<JoinOutcome, ApiError>, now: Instant) {
        let EntryStep::Name { email, .. } = &self.step else {
            return;
        };
        let email = email.clone();
        match result {
            Ok(JoinOutcome::Session { session }) => self.store(session),
            Ok(JoinOutcome::PendingApproval { wait_token }) => self.go(
                EntryStep::AwaitingVouch {
                    email,
                    wait: wait_token,
                    name: Some(name),
                },
                now,
            ),
            Err(e) => {
                warn!(error = %e, detail = ?e.detail, "join refused");
                self.fail_api(&e, copy::join_error(&e), now);
            }
        }
    }

    /// One status check at a time: a press while one is in flight only shows it.
    fn check(&mut self, pressed: bool, now: Instant) {
        let EntryStep::AwaitingVouch { wait, .. } = &self.step else {
            return;
        };
        if self.storing.is_some() || self.held(now) {
            return;
        }
        if pressed {
            self.busy = true;
            if !self.checking {
                self.said = None;
            }
        }
        if self.checking {
            return;
        }
        self.checking = true;
        let (origin, wait) = (self.reached_origin(), wait.clone());
        self.ask(Op::Status(origin, wait));
    }

    fn status(&mut self, result: Result<JoinStatus, ApiError>, now: Instant) {
        let EntryStep::AwaitingVouch { email, name, .. } = &self.step else {
            return;
        };
        let (email, name) = (email.clone(), name.clone());
        let pressed = self.busy;
        self.busy = false;
        self.checking = false;
        match result {
            Ok(JoinStatus::PendingApproval) => {
                debug!("still waiting for a Vouch");
                self.said = pressed.then(|| Said::Hint(copy::NOT_YET.into()));
            }
            Ok(JoinStatus::Session { session }) => {
                info!("vouched in");
                self.said = None;
                self.next_check = None;
                self.store(session);
            }
            Ok(JoinStatus::Declined) => {
                info!("request declined");
                let why = Closed::Declined;
                self.go(EntryStep::Closed { why, email, name }, now);
            }
            Err(e)
                if matches!(
                    e.kind,
                    ApiKind::Server(ErrorCode::AuthFailed | ErrorCode::AuthExpired)
                ) =>
            {
                // Spent or expired: a code shows where it stands (and hands out a new token).
                info!("wait token no longer valid");
                self.go(EntryStep::Email, now);
                self.fill = Some(Fill::Email(email));
                self.said = Some(Said::Info(copy::WAIT_EXPIRED.into()));
            }
            Err(e) if pressed => {
                warn!(error = %e, "join status check failed");
                self.fail_api(&e, copy::request_code_error(&e), now);
            }
            Err(e) => {
                debug!(error = %e, "background join check failed; next one retries");
                self.hold(&e, now);
            }
        }
    }

    fn go_to(&mut self, stage: Stage, now: Instant) {
        let goes_back = self
            .rail()
            .into_iter()
            .any(|chip| chip.stage == stage && matches!(chip.at, ChipAt::Done { back: true, .. }));
        if goes_back {
            self.go_back(stage, now);
        }
    }

    fn back(&mut self, now: Instant) {
        let to = match &self.step {
            EntryStep::Address | EntryStep::SignedIn => return,
            EntryStep::Email => Stage::Lobby,
            _ => Stage::Email,
        };
        self.go_back(to, now);
    }

    /// Back at the email, the email so far is filled in to change.
    fn go_back(&mut self, to: Stage, now: Instant) {
        if let EntryStep::Closed { why, .. } = &self.step
            && *why != Closed::Declined
        {
            // The Lobby may have been set up or opened since it was described.
            let origin = self.reached_origin();
            self.ask(Op::Describe(origin));
        }
        let email = self.step.email().cloned();
        if to == Stage::Lobby {
            self.go(EntryStep::Address, now);
        } else {
            self.go(EntryStep::Email, now);
            self.fill = email.map(Fill::Email);
        }
    }
}

fn step_name(step: &EntryStep) -> &'static str {
    match step {
        EntryStep::Address => "Address",
        EntryStep::Email => "Email",
        EntryStep::Code { .. } => "Code",
        EntryStep::Name { .. } => "Name",
        EntryStep::AwaitingVouch { .. } => "AwaitingVouch",
        EntryStep::Closed { .. } => "Closed",
        EntryStep::SignedIn => "SignedIn",
    }
}

#[cfg(test)]
mod tests {
    use lobby_protocol::{MagicChallenge, PROTOCOL_VERSION, WaitToken};

    use super::*;
    use crate::testing::FakeLobby;

    fn requested(label: &str) -> MagicCodeRequested {
        MagicCodeRequested {
            challenge: MagicChallenge::new("challenge"),
            label: RequestLabel::try_from(label.to_owned()).unwrap(),
        }
    }

    fn reached() -> Entry {
        let origin = LobbyOrigin::parse("http://127.0.0.1:1", HttpPolicy::LoopbackOnly).unwrap();
        Entry::reached(
            HttpPolicy::LoopbackOnly,
            origin,
            FakeLobby::lobby_info(PROTOCOL_VERSION),
        )
    }

    fn carol() -> Email {
        Email::parse("carol@lobby.test").unwrap()
    }

    fn waiting() -> EntryStep {
        EntryStep::AwaitingVouch {
            email: carol(),
            wait: WaitToken::new("wait"),
            name: None,
        }
    }

    fn ops(entry: &mut Entry) -> Vec<&'static str> {
        entry
            .take_ops()
            .into_iter()
            .map(|(_, op)| match op {
                Op::RequestCode(..) => "code",
                Op::Status(..) => "status",
                Op::Clear(_) => "clear",
                Op::Verify(..) => "verify",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn a_late_answer_after_a_resend_changes_nothing() {
        let now = Instant::now();
        let mut entry = reached();
        entry.dispatch(EntryIntent::SendCode(carol().as_str().into()), now);
        let (sent, _) = entry.take_ops().remove(0);
        let challenge = || Ok(requested("KQV"));
        entry.complete(sent, Done::CodeSent(carol(), challenge()), now);
        let later = now + RESEND_AFTER;
        entry.dispatch(EntryIntent::Resend, later);
        let (resent, _) = entry.take_ops().remove(0);
        entry.dispatch(EntryIntent::Back, later);
        entry.complete(resent, Done::CodeSent(carol(), challenge()), later);
        let view = entry.view(later);
        assert_eq!(view.step, EntryStep::Email);
        assert_eq!(view.said, None);
    }

    #[test]
    fn the_code_step_names_the_request_of_the_newest_code() {
        let now = Instant::now();
        let mut entry = reached();
        entry.dispatch(EntryIntent::SendCode(carol().as_str().into()), now);
        let (sent, _) = entry.take_ops().remove(0);
        entry.complete(sent, Done::CodeSent(carol(), Ok(requested("KQV"))), now);
        let label = |entry: &Entry, at| entry.view(at).code_label.map(|l| l.to_string());
        assert_eq!(label(&entry, now).as_deref(), Some("KQV"));
        let later = now + RESEND_AFTER;
        entry.dispatch(EntryIntent::Resend, later);
        let (resent, _) = entry.take_ops().remove(0);
        entry.complete(resent, Done::CodeSent(carol(), Ok(requested("MZT"))), later);
        assert_eq!(label(&entry, later).as_deref(), Some("MZT"));
    }

    #[test]
    fn waiting_for_a_vouch_makes_one_status_check_at_a_time() {
        let now = Instant::now();
        let mut entry = reached();
        entry.go(waiting(), now);
        entry.dispatch(EntryIntent::CheckNow, now);
        entry.dispatch(EntryIntent::WindowActive, now);
        entry.dispatch(EntryIntent::CheckNow, now);
        let due = entry.next_deadline(now).expect("the regular check");
        assert_eq!(due, now + JOIN_CHECK_EVERY);
        entry.tick(due);
        assert_eq!(ops(&mut entry), ["status"], "one check in flight");
        assert!(entry.view(due).busy, "a press shows the check");
        entry.complete(
            entry.epoch,
            Done::Status(Ok(JoinStatus::PendingApproval)),
            due,
        );
        assert_eq!(entry.view(due).said, Some(Said::Hint(copy::NOT_YET.into())));
        entry.dispatch(EntryIntent::WindowActive, due);
        assert_eq!(
            ops(&mut entry),
            ["status"],
            "the next one goes once it answered"
        );
    }

    /// Back from a Closed screen asks the Lobby again what it is. When that answer comes after
    /// the person went on to another Lobby, it is not that Lobby's description.
    #[test]
    fn a_late_description_of_another_lobby_changes_nothing() {
        let now = Instant::now();
        let policy = HttpPolicy::LoopbackOnly;
        let described = |name: &str| LobbyInfo {
            name: name.into(),
            ..FakeLobby::lobby_info(PROTOCOL_VERSION)
        };
        let mut entry = Entry::at_address(policy, None);
        let reach = |entry: &mut Entry, address: &str, info: LobbyInfo| {
            entry.dispatch(EntryIntent::Connect(address.into()), now);
            let (epoch, _) = entry.take_ops().remove(0);
            entry.complete(epoch, Done::Reached(Ok(info), Ok(false)), now);
        };
        reach(&mut entry, "http://127.0.0.1:1", described("Lobby A"));
        let closed = EntryStep::Closed {
            why: Closed::NotOpenYet,
            email: carol(),
            name: None,
        };
        entry.go(closed, now);
        entry.dispatch(EntryIntent::Back, now);
        let (asked, Op::Describe(a)) = entry.take_ops().remove(0) else {
            panic!("Back from Closed asks the Lobby again");
        };
        entry.dispatch(EntryIntent::Back, now);
        assert_eq!(entry.view(now).step, EntryStep::Address);
        reach(&mut entry, "http://127.0.0.1:2", described("Lobby B"));
        entry.complete(asked, Done::Described(a, Ok(described("Lobby A"))), now);
        let view = entry.view(now);
        assert_eq!(view.info.map(|i| i.name), Some("Lobby B".to_owned()));
    }

    fn refused(code: ErrorCode) -> ApiError {
        ApiError {
            status: Some(500),
            kind: ApiKind::Server(code),
            message: "refused".into(),
            retry_after: None,
            detail: None,
        }
    }

    /// No regular check, window coming forward or press asks the Lobby again during its pause.
    #[test]
    fn a_rate_limited_vouch_check_waits_out_the_pause() {
        let now = Instant::now();
        let pause = Duration::from_secs(30);
        for pressed in [true, false] {
            let mut entry = reached();
            entry.go(waiting(), now);
            let check = if pressed {
                EntryIntent::CheckNow
            } else {
                EntryIntent::WindowActive
            };
            entry.dispatch(check, now);
            entry.take_ops();
            let limited = ApiError {
                retry_after: Some(pause),
                ..refused(ErrorCode::RateLimited)
            };
            entry.complete(entry.epoch, Done::Status(Err(limited)), now);
            let during = now + JOIN_CHECK_EVERY;
            entry.tick(during);
            entry.dispatch(EntryIntent::WindowActive, during);
            entry.dispatch(EntryIntent::CheckNow, during);
            assert!(ops(&mut entry).is_empty(), "pressed: {pressed}");
            assert!(entry.view(during).held, "pressed: {pressed}");
            entry.tick(now + pause);
            entry.dispatch(EntryIntent::WindowActive, now + pause);
            assert_eq!(ops(&mut entry), ["status"], "pressed: {pressed}");
        }
    }

    /// A check the person did not ask for says nothing of a failure; one they pressed does, and a
    /// spent wait token goes back to the email with it filled in.
    #[test]
    fn waiting_for_a_vouch_hears_every_answer_calmly() {
        let now = Instant::now();
        let mut entry = reached();
        entry.go(waiting(), now);
        let answer = |entry: &mut Entry, pressed: bool, status| {
            let check = if pressed {
                EntryIntent::CheckNow
            } else {
                EntryIntent::WindowActive
            };
            entry.dispatch(check, now);
            entry.take_ops();
            entry.complete(entry.epoch, Done::Status(status), now);
            entry.view(now)
        };
        let view = answer(&mut entry, false, Err(refused(ErrorCode::InternalError)));
        assert_eq!(view.said, None);
        let view = answer(&mut entry, true, Err(refused(ErrorCode::InternalError)));
        assert_eq!(view.said, Some(Said::Error("refused".into())));
        let view = answer(&mut entry, false, Ok(JoinStatus::PendingApproval));
        assert_eq!(
            view.said, None,
            "the Lobby answered: the stale line is gone"
        );
        let view = answer(&mut entry, true, Ok(JoinStatus::PendingApproval));
        assert_eq!(view.said, Some(Said::Hint(copy::NOT_YET.into())));
        let view = answer(&mut entry, false, Ok(JoinStatus::Declined));
        assert!(matches!(
            view.step,
            EntryStep::Closed {
                why: Closed::Declined,
                ..
            }
        ));
        entry.dispatch(EntryIntent::Back, now);
        assert_eq!(entry.view(now).step, EntryStep::Email);

        entry.go(waiting(), now);
        let view = answer(&mut entry, false, Err(refused(ErrorCode::AuthFailed)));
        assert_eq!(view.step, EntryStep::Email);
        assert_eq!(view.fill, Some(Fill::Email(carol())));
        assert_eq!(view.said, Some(Said::Info(copy::WAIT_EXPIRED.into())));
    }

    fn rate_limited(retry: Duration) -> ApiError {
        ApiError {
            status: Some(429),
            kind: ApiKind::Server(ErrorCode::RateLimited),
            message: "slow down".into(),
            retry_after: Some(retry),
            detail: None,
        }
    }

    /// A Lobby that refuses a new code pauses sending alone: the code already sent is still
    /// checked, and the link to send another returns once the Lobby's wait is over.
    #[test]
    fn a_refused_new_code_pauses_sending_and_not_the_code_already_sent() {
        let now = Instant::now();
        let mut entry = reached();
        entry.dispatch(EntryIntent::SendCode(carol().as_str().into()), now);
        let (sent, _) = entry.take_ops().remove(0);
        entry.complete(sent, Done::CodeSent(carol(), Ok(requested("KQV"))), now);
        let later = now + RESEND_AFTER;
        entry.dispatch(EntryIntent::Resend, later);
        let (resent, _) = entry.take_ops().remove(0);
        let wait = Duration::from_secs(600);
        entry.complete(
            resent,
            Done::CodeSent(carol(), Err(rate_limited(wait))),
            later,
        );

        let view = entry.view(later);
        assert!(matches!(view.step, EntryStep::Code { .. }));
        assert_eq!(
            view.said,
            Some(Said::Error(copy::request_code_error(&rate_limited(wait))))
        );
        assert!(!view.held, "the code's button does not wait");
        assert_eq!(view.resend, Resend::Wait);
        assert_eq!(entry.next_deadline(later), Some(later + wait));

        entry.dispatch(EntryIntent::Resend, later);
        entry.dispatch(EntryIntent::SubmitCode("12345678".into()), later);
        assert_eq!(ops(&mut entry), ["verify"], "checked; no new code asked");

        let over = later + wait;
        entry.tick(over);
        let view = entry.view(over);
        assert_eq!((view.resend, view.said), (Resend::Ready, None));
    }

    /// A refused code, unlike a refused send, pauses the code's button and not the resend link.
    #[test]
    fn a_refused_check_holds_the_check_and_not_the_resend() {
        let now = Instant::now();
        let mut entry = reached();
        entry.dispatch(EntryIntent::SendCode(carol().as_str().into()), now);
        let (sent, _) = entry.take_ops().remove(0);
        entry.complete(sent, Done::CodeSent(carol(), Ok(requested("KQV"))), now);
        entry.dispatch(EntryIntent::SubmitCode("12345678".into()), now);
        let (checked, _) = entry.take_ops().remove(0);
        entry.complete(
            checked,
            Done::Verified(Err(rate_limited(Duration::from_secs(600)))),
            now,
        );
        let later = now + RESEND_AFTER;
        assert!(entry.view(later).held);
        entry.dispatch(EntryIntent::SubmitCode("12345678".into()), later);
        assert!(ops(&mut entry).is_empty());
        entry.dispatch(EntryIntent::Resend, later);
        assert_eq!(ops(&mut entry), ["code"]);
    }
}

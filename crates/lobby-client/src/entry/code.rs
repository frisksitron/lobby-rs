//! The Magic Code step: asking for a code and for a new one, checking it, and what the Lobby says
//! comes next. Each refusal that keeps the person on the code step is counted, a new code waits a
//! minute after the last, and none is offered past the Lobby's cap. A Lobby that refuses to send
//! (429) pauses sending alone: the code already sent can still be checked.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use lobby_protocol::{
    Email, ErrorCode, ErrorDetail, JoinTicket, MagicCode, MagicCodeRequested, MagicCodeVerify,
    RequestLabel, Username, VerifyOutcome, limits,
};
use tracing::{info, warn};

use super::{Closed, Entry, EntryStep, Fill, Flight, NamePath, Op, RATE_LIMIT_FALLBACK, Said};
use crate::{ApiError, ApiKind, LobbyOrigin, copy};

/// A new code is offered this long after the last one.
pub const RESEND_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resend {
    Ready,
    Wait,
    /// The server's cap is likely reached: no more until the window frees.
    Capped,
}

/// Whose codes a count is of: each Lobby caps the codes it mails to an email.
pub(super) type CodesTo = (LobbyOrigin, Email);

/// The codes asked for in this Entry, per Lobby and email (its view of each Lobby's cap), the
/// challenge of the newest one, and how many were refused.
#[derive(Default)]
pub(super) struct Codes {
    sent: HashMap<CodesTo, Vec<Instant>>,
    /// A code is checked with the challenge its request returned, and only the newest request is
    /// kept, with the label its mail carries.
    requests: HashMap<CodesTo, MagicCodeRequested>,
    /// A Lobby told this device to send no more until then (429). It stops asking for codes and
    /// nothing else: checking the code already sent is another limit.
    send_hold: Option<(LobbyOrigin, Instant)>,
    pub wrong: u64,
}

impl Codes {
    /// When sending to `origin` resumes, if it is paused at `now`.
    pub fn sends_held_until(&self, origin: Option<&LobbyOrigin>, now: Instant) -> Option<Instant> {
        self.send_hold
            .as_ref()
            .filter(|(held, until)| Some(held) == origin && *until > now)
            .map(|(_, until)| *until)
    }

    fn hold_sends(&mut self, origin: LobbyOrigin, until: Instant) {
        self.send_hold = Some((origin, until));
    }

    /// Forgets a hold that ran out; `true` when one did.
    pub fn release_sends(&mut self, now: Instant) -> bool {
        let over = self
            .send_hold
            .as_ref()
            .is_some_and(|(_, until)| *until <= now);
        if over {
            self.send_hold = None;
        }
        over
    }

    fn recent(&self, to: &CodesTo, now: Instant) -> Vec<Instant> {
        self.sent.get(to).map_or_else(Vec::new, |sent| {
            sent.iter()
                .filter(|t| now.saturating_duration_since(**t) < limits::MAGIC_CODE_SEND_WINDOW)
                .copied()
                .collect()
        })
    }

    fn record(&mut self, to: CodesTo, now: Instant) {
        let sent = self.sent.entry(to).or_default();
        sent.retain(|t| now.saturating_duration_since(*t) < limits::MAGIC_CODE_SEND_WINDOW);
        sent.push(now);
    }

    /// The server mails at most `cap` codes per email in its window and skips the rest silently
    /// (no enumeration), so no more are offered once they are out.
    pub fn resend(&self, to: &CodesTo, cap: usize, now: Instant) -> Resend {
        let held = self.sends_held_until(Some(&to.0), now).is_some();
        let recent = self.recent(to, now);
        if recent.len() >= cap {
            Resend::Capped
        } else if held
            || recent
                .last()
                .is_some_and(|last| now.saturating_duration_since(*last) < RESEND_AFTER)
        {
            Resend::Wait
        } else {
            Resend::Ready
        }
    }

    /// When [`Self::resend`] next changes, if it will.
    pub fn changes_at(&self, to: &CodesTo, cap: usize, now: Instant) -> Option<Instant> {
        let held = self.sends_held_until(Some(&to.0), now);
        let recent = self.recent(to, now);
        let spaced = if recent.len() >= cap {
            recent.first().map(|t| *t + limits::MAGIC_CODE_SEND_WINDOW)
        } else {
            recent.last().map(|t| *t + RESEND_AFTER)
        };
        [spaced, held]
            .into_iter()
            .flatten()
            .filter(|at| *at > now)
            .min()
    }
}

impl Entry {
    /// The codes of `email` at the Lobby this Entry reached.
    fn codes_to(&self, email: &Email) -> CodesTo {
        (self.reached_origin(), email.clone())
    }

    /// The label of the request `email`'s newest code answers.
    pub(super) fn code_label(&self, email: &Email) -> Option<RequestLabel> {
        self.codes
            .requests
            .get(&self.codes_to(email))
            .map(|r| r.label.clone())
    }

    pub(super) fn sends_held(&self, now: Instant) -> bool {
        self.codes
            .sends_held_until(self.origin.as_ref(), now)
            .is_some()
    }

    pub(super) fn resend_state(&self, email: &Email, now: Instant) -> Resend {
        self.codes.resend(&self.codes_to(email), self.cap(), now)
    }

    pub(super) fn resend_changes_at(&self, email: &Email, now: Instant) -> Option<Instant> {
        self.codes
            .changes_at(&self.codes_to(email), self.cap(), now)
    }

    pub(super) fn send_code(&mut self, typed: &str, now: Instant) {
        if self.step != EntryStep::Email || self.flight.busy() || self.sends_held(now) {
            return;
        }
        let Ok(email) = Email::parse(typed) else {
            return self.fail(copy::ENTER_EMAIL);
        };
        if self.resend_state(&email, now) == Resend::Capped {
            return self.go(EntryStep::Code { email }, now);
        }
        self.begin(now);
        let origin = self.reached_origin();
        self.ask(Op::RequestCode(origin, email));
    }

    pub(super) fn resend(&mut self, now: Instant) {
        let EntryStep::Code { email } = &self.step else {
            return;
        };
        let email = email.clone();
        // A pause on checking the code (`held`) is not one on sending, which `resend_state` has.
        if self.resend_state(&email, now) != Resend::Ready || self.flight.busy() {
            return;
        }
        self.flight = Flight::Asked;
        self.said = None;
        let origin = self.reached_origin();
        self.ask(Op::RequestCode(origin, email));
    }

    /// A code was asked for `email`: from the email step, or a new one on the code step.
    pub(super) fn code_sent(
        &mut self,
        email: Email,
        result: Result<MagicCodeRequested, ApiError>,
        now: Instant,
    ) {
        let resent = match &self.step {
            EntryStep::Email => false,
            EntryStep::Code { .. } => true,
            _ => return,
        };
        let requested = match result {
            Ok(requested) => requested,
            Err(e) => {
                warn!(error = %e, "magic code request failed");
                if e.kind == ApiKind::Server(ErrorCode::RateLimited) {
                    let until = now + e.retry_after.unwrap_or(RATE_LIMIT_FALLBACK);
                    self.codes.hold_sends(self.reached_origin(), until);
                }
                return self.fail(copy::request_code_error(&e));
            }
        };
        info!(domain = email.domain(), "magic code requested");
        self.codes.requests.insert(self.codes_to(&email), requested);
        self.codes.record(self.codes_to(&email), now);
        let capped = self.resend_state(&email, now) == Resend::Capped;
        let label = self.code_label(&email);
        self.go(EntryStep::Code { email }, now);
        if resent {
            self.said = Some(if capped {
                Said::Info(copy::codes_capped(self.cap() as u32, label.as_ref()))
            } else {
                Said::Notice(copy::CODE_RESENT.into())
            });
        }
    }

    pub(super) fn submit_code(&mut self, typed: String, now: Instant) {
        let EntryStep::Code { email } = &self.step else {
            return;
        };
        let Ok(code) = MagicCode::new(typed) else {
            return;
        };
        let Some(requested) = self.codes.requests.get(&self.codes_to(email)) else {
            // No code was asked for from here: only a new one can be checked.
            return self.fail(copy::CODE_EXPIRED);
        };
        let body = MagicCodeVerify {
            challenge: requested.challenge.clone(),
            code,
        };
        if !self.begin(now) {
            return;
        }
        let origin = self.reached_origin();
        self.ask(Op::Verify(origin, body));
    }

    pub(super) fn verified(&mut self, result: Result<VerifyOutcome, ApiError>, now: Instant) {
        let EntryStep::Code { email } = &self.step else {
            return;
        };
        let email = email.clone();
        match result {
            Ok(VerifyOutcome::Session { session }) => self.store(session),
            Ok(VerifyOutcome::PendingApproval { wait_token }) => self.go(
                EntryStep::AwaitingVouch {
                    email,
                    wait: wait_token,
                    name: None,
                },
                now,
            ),
            Ok(VerifyOutcome::CreateLobby { ticket }) => {
                self.ask_for_name(email, ticket, NamePath::CreateLobby, now);
            }
            Ok(VerifyOutcome::AskToJoin { ticket }) => {
                self.ask_for_name(email, ticket, NamePath::AskToJoin, now);
            }
            Err(e) if matches!(e.detail, Some(ErrorDetail::Locked | ErrorDetail::NotSetUp)) => {
                let why = if e.detail == Some(ErrorDetail::NotSetUp) {
                    Closed::NotOpenYet
                } else {
                    Closed::Locked
                };
                info!(?why, "this Lobby does not take this email in");
                let name = None;
                self.go(EntryStep::Closed { why, email, name }, now);
            }
            Err(e) => {
                warn!(error = %e, "magic code refused");
                self.codes.wrong += 1;
                self.fail_api(&e, copy::code_error(&e), now);
            }
        }
    }

    fn ask_for_name(&mut self, email: Email, ticket: JoinTicket, path: NamePath, now: Instant) {
        let suggestion = suggest_username(&email);
        self.go(
            EntryStep::Name {
                email,
                ticket,
                path,
            },
            now,
        );
        self.fill = Some(Fill::Username(suggestion));
    }
}

/// A username from an email's local part: the allowed characters, 3–32 of them
/// ("jo.doe+lobby" becomes "jodoelobby"); empty if too little is left.
fn suggest_username(email: &Email) -> String {
    let local = email.as_str().split('@').next().unwrap_or_default();
    let name: String = local
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(*limits::USERNAME_LEN.end())
        .collect();
    if Username::parse(&name).is_ok() {
        name
    } else {
        String::new()
    }
}

//! The redial rules, without IO: what the end of one attempt means for the next.

use std::time::Duration;

use lobby_protocol::close;

use super::{Phase, SignedOutWhy};

/// A session that lasted this long was a real recovery: the attempt count starts over. One that
/// ended sooner keeps counting, so a Lobby that accepts and then drops every connection backs off
/// to Unavailable instead of being redialed in a loop.
const STABLE_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub struct ReconnectPolicy {
    pub tiers: [Duration; 4],
    pub max_attempts: u32,
    /// ±this fraction of the wait, so a Lobby's clients do not all return at the same instant
    /// after a server restart.
    pub jitter: f64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            tiers: [2, 5, 10, 30].map(Duration::from_secs),
            max_attempts: 10,
            jitter: 0.2,
        }
    }
}

impl ReconnectPolicy {
    pub fn delay_before(&self, attempt: u32, unit: f64) -> Option<Duration> {
        if attempt == 0 || attempt > self.max_attempts {
            return None;
        }
        let tier = usize::try_from(attempt - 1)
            .unwrap_or(usize::MAX)
            .min(self.tiers.len() - 1);
        let factor = 1.0 + self.jitter * (2.0 * unit.clamp(0.0, 1.0) - 1.0);
        Some(self.tiers[tier].mul_f64(factor.max(0.0)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Ended {
    NeedsAuth,
    ProtocolMismatch,
    Failed { no_network: bool },
    Dead { up_for: Duration },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum AfterClose {
    /// A close of 4003: refresh the token, then call `refreshed`.
    Refresh,
    Step(Step),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refresh {
    Done,
    SessionExpired,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Woken {
    Timer,
    Asked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    ProtocolMismatch,
    /// No session to resume, or a refresh the Lobby refused.
    Expired,
    /// The Lobby ended this session (4005 or 4006), so its tokens go too.
    Ended(SignedOutWhy),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hold {
    Replaced,
    /// The Lobby closed 4000 twice.
    Refused {
        attempts: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Step {
    /// Final: the actor tells the Session and returns.
    Stop(Stop),
    /// Wait for a click only, then call `held`.
    Hold(Hold),
    Now,
    /// Emit the phase and wait `delay` (a click or a resume ends it early), then call `woke`.
    Wait {
        phase: Phase,
        delay: Duration,
        attempt: u32,
    },
    /// Emit Unavailable and wait for a click or a resume, then call `asked`.
    GiveUp(Phase),
}

pub(super) struct Redial {
    policy: ReconnectPolicy,
    attempt: u32,
    /// One immediate retry after a 4003 (refresh, then reconnect), not counted as an attempt.
    auth_retry: bool,
    violations: u32,
    /// Redials without a wait since the last stable session: one is allowed, then backoff.
    quick: u32,
}

impl Redial {
    pub(super) fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            attempt: 0,
            auth_retry: true,
            violations: 0,
            quick: 0,
        }
    }

    pub(super) fn attempt(&self) -> u32 {
        self.attempt
    }

    /// `unit` is the jitter draw, in `0.0..=1.0`.
    pub(super) fn ended(&mut self, end: Ended, unit: f64) -> Step {
        match end {
            Ended::NeedsAuth => Step::Stop(Stop::Expired),
            Ended::ProtocolMismatch => Step::Stop(Stop::ProtocolMismatch),
            Ended::Failed { no_network } => self.backoff(no_network, unit),
            Ended::Dead { up_for } => {
                if up_for >= STABLE_AFTER {
                    self.recovered();
                }
                self.redial(unit)
            }
        }
    }

    /// The Lobby closed the socket, or it ended without a close. `up_for` is `None` when that
    /// came before READY.
    pub(super) fn closed(
        &mut self,
        code: Option<u16>,
        up_for: Option<Duration>,
        unit: f64,
    ) -> AfterClose {
        let after_ready = up_for.is_some();
        let stable = up_for.is_some_and(|d| d >= STABLE_AFTER);
        if stable {
            self.recovered();
        }
        if code == Some(close::PROTOCOL_VIOLATION) {
            self.violations += 1;
        } else if after_ready {
            self.violations = 0;
        }
        AfterClose::Step(match code {
            Some(close::REPLACED) => Step::Hold(Hold::Replaced),
            Some(close::SESSION_REVOKED) => Step::Stop(Stop::Ended(SignedOutWhy::Revoked)),
            Some(close::REMOVED) => Step::Stop(Stop::Ended(SignedOutWhy::Removed)),
            Some(close::AUTH_INVALID) if self.auth_retry || after_ready => {
                self.auth_retry = false;
                return AfterClose::Refresh;
            }
            // The server was not reading fast enough for us: READY resyncs.
            Some(close::SLOW_CONSUMER) => self.redial(unit),
            // Once may be a fluke; twice is this client's bug, and retrying would only repeat it.
            Some(close::PROTOCOL_VIOLATION) if self.violations >= 2 => Step::Hold(Hold::Refused {
                attempts: self.attempt,
            }),
            // A reset, an end without a close, or a server restart, on a session that was stable:
            // the path or the Lobby is likely back already.
            None | Some(close::GOING_AWAY) if stable => self.redial(unit),
            _ => self.backoff(false, unit),
        })
    }

    pub(super) fn refreshed(&mut self, outcome: Refresh, unit: f64) -> Step {
        match outcome {
            Refresh::Done => self.redial(unit),
            Refresh::SessionExpired => Step::Stop(Stop::Expired),
            Refresh::Failed => self.backoff(false, unit),
        }
    }

    /// After a `Step::Wait`.
    pub(super) fn woke(&mut self, how: Woken) {
        match how {
            Woken::Timer => self.attempt += 1,
            Woken::Asked => self.asked(),
        }
    }

    /// After a `Step::Hold`: a click starts everything over.
    pub(super) fn held(&mut self) {
        self.asked();
        self.violations = 0;
    }

    /// After a `Step::GiveUp`, or a click or a resume during a wait. The count of 4000 closes
    /// stays: only the click that ends a hold forgives them.
    pub(super) fn asked(&mut self) {
        self.recovered();
        self.auth_retry = true;
    }

    fn recovered(&mut self) {
        self.attempt = 0;
        self.quick = 0;
    }

    /// Dial again at once, unless that was already done since the last stable session: a path
    /// that keeps failing right after READY gets the normal backoff.
    fn redial(&mut self, unit: f64) -> Step {
        self.quick += 1;
        if self.quick > 1 {
            self.backoff(false, unit)
        } else {
            Step::Now
        }
    }

    fn backoff(&mut self, no_network: bool, unit: f64) -> Step {
        let upcoming = self.attempt + 1;
        let Some(delay) = self.policy.delay_before(upcoming, unit) else {
            return Step::GiveUp(Phase::Unavailable {
                attempts: self.attempt,
            });
        };
        let phase = if no_network {
            Phase::Offline { retry_in: delay }
        } else {
            Phase::Reconnecting {
                attempt: upcoming,
                max: self.policy.max_attempts,
                retry_in: delay,
            }
        };
        Step::Wait {
            phase,
            delay,
            attempt: upcoming,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIDDLE: f64 = 0.5;
    const SHORT: Duration = Duration::from_secs(1);

    fn redial() -> Redial {
        Redial::new(ReconnectPolicy::default())
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn closed(r: &mut Redial, code: Option<u16>, up_for: Option<Duration>) -> Step {
        match r.closed(code, up_for, MIDDLE) {
            AfterClose::Step(step) => step,
            AfterClose::Refresh => panic!("expected a step, got a refresh"),
        }
    }

    fn waits(step: &Step) -> Duration {
        match step {
            Step::Wait { delay, .. } => *delay,
            other => panic!("expected a wait, got {other:?}"),
        }
    }

    fn lost() -> Ended {
        Ended::Failed { no_network: false }
    }

    #[test]
    fn backoff_climbs_the_tiers_and_gives_up_at_max_attempts() {
        let mut r = redial();
        let mut seen = Vec::new();
        for n in 1..=10 {
            let step = r.ended(lost(), MIDDLE);
            assert_eq!(
                step,
                Step::Wait {
                    phase: Phase::Reconnecting {
                        attempt: n,
                        max: 10,
                        retry_in: waits(&step),
                    },
                    delay: waits(&step),
                    attempt: n,
                }
            );
            seen.push(waits(&step));
            r.woke(Woken::Timer);
            assert_eq!(r.attempt(), n);
        }
        let tiers = [2, 5, 10, 30, 30, 30, 30, 30, 30, 30].map(secs);
        assert_eq!(seen, tiers);
        assert_eq!(
            r.ended(lost(), MIDDLE),
            Step::GiveUp(Phase::Unavailable { attempts: 10 })
        );
    }

    #[test]
    fn a_dial_without_network_waits_as_offline() {
        let mut r = redial();
        let step = r.ended(Ended::Failed { no_network: true }, MIDDLE);
        assert_eq!(
            step,
            Step::Wait {
                phase: Phase::Offline { retry_in: secs(2) },
                delay: secs(2),
                attempt: 1,
            }
        );
    }

    #[test]
    fn a_stable_session_starts_the_count_over_and_a_short_one_does_not() {
        let mut r = redial();
        for _ in 0..3 {
            r.ended(lost(), MIDDLE);
            r.woke(Woken::Timer);
        }
        let step = closed(&mut r, None, Some(SHORT));
        assert_eq!(waits(&step), secs(30), "the fourth attempt, not the first");

        assert_eq!(closed(&mut r, None, Some(STABLE_AFTER)), Step::Now);
        assert_eq!(r.attempt(), 0);
        let step = r.ended(lost(), MIDDLE);
        assert_eq!(waits(&step), secs(2));
    }

    #[test]
    fn one_quick_redial_then_a_wait() {
        let mut r = redial();
        let dead = Ended::Dead { up_for: SHORT };
        assert_eq!(r.ended(dead, MIDDLE), Step::Now);
        assert_eq!(waits(&r.ended(dead, MIDDLE)), secs(2));

        let stable = Ended::Dead {
            up_for: STABLE_AFTER,
        };
        assert_eq!(
            r.ended(stable, MIDDLE),
            Step::Now,
            "a recovery earns another"
        );
        let slow = closed(&mut r, Some(close::SLOW_CONSUMER), Some(SHORT));
        assert_eq!(waits(&slow), secs(2));
    }

    #[test]
    fn a_close_of_4000_twice_holds_unless_something_else_came_between() {
        let mut r = redial();
        let violation = Some(close::PROTOCOL_VIOLATION);
        assert_eq!(waits(&closed(&mut r, violation, Some(SHORT))), secs(2));
        r.woke(Woken::Timer);
        assert_eq!(
            closed(&mut r, violation, Some(SHORT)),
            Step::Hold(Hold::Refused { attempts: 1 })
        );

        r.held();
        assert_eq!(
            waits(&closed(&mut r, violation, Some(SHORT))),
            secs(2),
            "a click forgives"
        );

        closed(&mut r, Some(close::GOING_AWAY), Some(SHORT));
        assert_eq!(
            waits(&closed(&mut r, violation, Some(SHORT))),
            secs(2),
            "a session that got to READY in between"
        );
    }

    #[test]
    fn a_click_does_not_forgive_a_client_the_lobby_refuses() {
        let mut r = redial();
        let violation = Some(close::PROTOCOL_VIOLATION);
        closed(&mut r, violation, Some(SHORT));
        r.asked();
        assert_eq!(
            closed(&mut r, violation, Some(SHORT)),
            Step::Hold(Hold::Refused { attempts: 0 })
        );
    }

    #[test]
    fn the_session_is_refreshed_once_before_ready_and_once_per_session_after() {
        let mut r = redial();
        let refused = Some(close::AUTH_INVALID);
        assert_eq!(r.closed(refused, None, MIDDLE), AfterClose::Refresh);
        assert_eq!(r.refreshed(Refresh::Done, MIDDLE), Step::Now);
        assert_eq!(
            waits(&closed(&mut r, refused, None)),
            secs(2),
            "refused again before READY: backoff"
        );
        assert_eq!(
            r.closed(refused, Some(SHORT), MIDDLE),
            AfterClose::Refresh,
            "a session that got to READY is refreshed again"
        );
        assert_eq!(
            waits(&r.refreshed(Refresh::Done, MIDDLE)),
            secs(2),
            "the quick redial was spent"
        );
    }

    #[test]
    fn a_refresh_that_fails_waits_and_one_the_lobby_refuses_stops() {
        let mut r = redial();
        r.closed(Some(close::AUTH_INVALID), None, MIDDLE);
        assert_eq!(waits(&r.refreshed(Refresh::Failed, MIDDLE)), secs(2));

        r.asked();
        r.closed(Some(close::AUTH_INVALID), None, MIDDLE);
        assert_eq!(
            r.refreshed(Refresh::SessionExpired, MIDDLE),
            Step::Stop(Stop::Expired)
        );
    }

    #[test]
    fn a_click_after_giving_up_starts_over() {
        let mut r = redial();
        r.closed(Some(close::AUTH_INVALID), None, MIDDLE);
        for _ in 0..10 {
            r.ended(lost(), MIDDLE);
            r.woke(Woken::Timer);
        }
        assert!(matches!(r.ended(lost(), MIDDLE), Step::GiveUp(_)));

        r.asked();
        assert_eq!(r.attempt(), 0);
        assert_eq!(waits(&r.ended(lost(), MIDDLE)), secs(2));
        assert_eq!(
            r.closed(Some(close::AUTH_INVALID), None, MIDDLE),
            AfterClose::Refresh,
            "the refresh is allowed again"
        );
    }

    #[test]
    fn a_click_during_a_wait_starts_over() {
        let mut r = redial();
        r.ended(lost(), MIDDLE);
        r.woke(Woken::Timer);
        r.ended(lost(), MIDDLE);
        r.woke(Woken::Asked);
        assert_eq!(r.attempt(), 0);
    }

    #[test]
    fn closes_that_end_the_session_for_good() {
        let mut r = redial();
        assert_eq!(
            closed(&mut r, Some(close::SESSION_REVOKED), Some(SHORT)),
            Step::Stop(Stop::Ended(SignedOutWhy::Revoked))
        );
        assert_eq!(
            closed(&mut r, Some(close::REMOVED), None),
            Step::Stop(Stop::Ended(SignedOutWhy::Removed))
        );
        assert_eq!(r.ended(Ended::NeedsAuth, MIDDLE), Step::Stop(Stop::Expired));
        assert_eq!(
            r.ended(Ended::ProtocolMismatch, MIDDLE),
            Step::Stop(Stop::ProtocolMismatch)
        );
    }

    #[test]
    fn a_replaced_session_holds_until_a_click() {
        let mut r = redial();
        r.ended(lost(), MIDDLE);
        r.woke(Woken::Timer);
        assert_eq!(
            closed(&mut r, Some(close::REPLACED), Some(SHORT)),
            Step::Hold(Hold::Replaced)
        );
        r.held();
        assert_eq!(r.attempt(), 0);
    }
}

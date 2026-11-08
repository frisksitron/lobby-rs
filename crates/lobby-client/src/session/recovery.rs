//! Getting a call's media path back without leaving Voice. Once the server has the session
//! Active, a lost path (ICE reports it down) or a network change that moves the route to the SFU
//! starts an attempt: an ICE restart, given the policy's `timeout` to bring the path back. After
//! the policy's `attempts` failed ones, Voice ends.

use std::time::Instant;

use lobby_protocol::RestartPolicy;

use crate::media::RouteChange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Restart ICE; `1` is the first attempt.
    Attempt(u32),
    Recovered,
    GiveUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    /// ICE has not connected yet.
    Pending,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy)]
struct Attempt {
    n: u32,
    deadline: Instant,
}

#[derive(Debug)]
pub(super) struct PathRecovery {
    policy: RestartPolicy,
    /// The server has the session Active: before that, a failing path is the join's to end.
    active: bool,
    path: Path,
    attempt: Option<Attempt>,
}

impl PathRecovery {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            active: false,
            path: Path::Pending,
            attempt: None,
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.attempt.map(|a| a.deadline)
    }

    /// What the Member is told: an attempt runs and the path is still down. A restart on a
    /// healthy path (a network change) is not shown.
    pub fn reconnecting(&self) -> bool {
        self.attempt.is_some() && self.path == Path::Down
    }

    /// The server has the session Active. A path that dropped before it is restarted now.
    pub fn activate(&mut self, now: Instant) -> Option<Step> {
        self.active = true;
        self.restart_if_down(now)
    }

    /// A path that drops again during an attempt waits for that attempt's deadline.
    pub fn path_down(&mut self, now: Instant) -> Option<Step> {
        self.path = Path::Down;
        self.restart_if_down(now)
    }

    pub fn path_up(&mut self) -> Option<Step> {
        self.path = Path::Up;
        self.attempt.take().map(|_| Step::Recovered)
    }

    /// A network change restarts a healthy path only if it moved the route. During an attempt
    /// it gives the network time to settle: the attempt's deadline moves back, and a moved route
    /// restarts the same attempt over the new one.
    pub fn network_changed(&mut self, change: RouteChange, now: Instant) -> Option<Step> {
        if !self.active {
            return None;
        }
        let deadline = now + self.policy.timeout;
        match (&mut self.attempt, change) {
            (Some(a), RouteChange::Same) => {
                a.deadline = deadline;
                None
            }
            (Some(a), RouteChange::Moved) => {
                a.deadline = deadline;
                Some(Step::Attempt(a.n))
            }
            (None, RouteChange::Moved) => Some(self.start(1, now)),
            (None, RouteChange::Same) => None,
        }
    }

    /// An attempt whose deadline passed succeeded if ICE is up (a restart on a healthy path
    /// reports nothing new), else the next one starts, or there is none left.
    pub fn tick(&mut self, now: Instant) -> Option<Step> {
        let a = self.attempt.filter(|a| a.deadline <= now)?;
        if self.path != Path::Down {
            self.attempt = None;
            return Some(Step::Recovered);
        }
        if a.n < self.policy.attempts.get() {
            return Some(self.start(a.n + 1, now));
        }
        self.attempt = None;
        Some(Step::GiveUp)
    }

    fn restart_if_down(&mut self, now: Instant) -> Option<Step> {
        (self.active && self.path == Path::Down && self.attempt.is_none())
            .then(|| self.start(1, now))
    }

    fn start(&mut self, n: u32, now: Instant) -> Step {
        self.attempt = Some(Attempt {
            n,
            deadline: now + self.policy.timeout,
        });
        Step::Attempt(n)
    }
}

//! `IssueCenter`: the window's notifications. Every Issue the Session raises, and the desktop's
//! own, lands here and in a toast. An Issue with a retry instant goes when it ends, any other after
//! [`KEEP_FOR`] or when dismissed. Time is the GPUI executor's clock, so `advance_clock` drives it
//! in tests.

use std::{collections::HashSet, time::Duration};

use gpui_kit::{Context, Task};
use lobby_client::{Issue, IssueKind, Issues, LobbyOrigin};

pub const KEEP_FOR: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
pub struct IssueCenter {
    issues: Issues,
    unseen: HashSet<IssueKind>,
    sweep: Option<Task<()>>,
}

impl IssueCenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issues(&self) -> &Issues {
        &self.issues
    }

    pub fn unseen(&self) -> usize {
        self.unseen
            .iter()
            .filter(|kind| self.issues.get(**kind).is_some())
            .count()
    }

    pub fn is_unseen(&self, kind: IssueKind) -> bool {
        self.unseen.contains(&kind)
    }

    pub fn raise(&mut self, mut issue: Issue, cx: &mut Context<Self>) {
        tracing::info!(
            kind = issue.kind.key(),
            severity = ?issue.severity(),
            message = %issue.message,
            "issue"
        );
        if issue.expires_at.is_none() {
            issue.expires_at = Some(cx.background_executor().now() + KEEP_FOR);
        }
        self.unseen.insert(issue.kind);
        self.issues.raise(issue);
        self.sweep = None;
        self.arm(cx);
        cx.notify();
    }

    pub fn mark_seen(&mut self, cx: &mut Context<Self>) {
        if !self.unseen.is_empty() {
            self.unseen.clear();
            cx.notify();
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.issues.is_empty() || !self.unseen.is_empty() {
            self.issues = Issues::default();
            self.unseen.clear();
            cx.notify();
        }
    }

    /// The Lobby was left: what was about it goes, what is about this device stays.
    pub fn clear_lobby(&mut self, lobby: &LobbyOrigin, cx: &mut Context<Self>) {
        if self.issues.retain(|i| i.lobby.as_ref() != Some(lobby)) {
            let issues = &self.issues;
            self.unseen.retain(|kind| issues.get(*kind).is_some());
            cx.notify();
        }
    }

    pub fn dismiss(&mut self, kind: IssueKind, cx: &mut Context<Self>) {
        self.unseen.remove(&kind);
        if self.issues.dismiss(kind) {
            cx.notify();
        }
    }

    fn arm(&mut self, cx: &mut Context<Self>) {
        if self.sweep.is_some() || self.issues.next_expiry().is_none() {
            return;
        }
        self.sweep = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = this.update(cx, |center, cx| {
                    let now = cx.background_executor().now();
                    let wait = center
                        .issues
                        .next_expiry()
                        .map(|at| at.saturating_duration_since(now));
                    if wait.is_none() {
                        center.sweep = None;
                    }
                    wait
                });
                let Ok(Some(wait)) = wait else { break };
                cx.background_executor().timer(wait).await;
                let more = this.update(cx, |center, cx| {
                    let now = cx.background_executor().now();
                    if center.issues.sweep(now) {
                        cx.notify();
                    }
                    let more = center.issues.next_expiry().is_some();
                    if !more {
                        center.sweep = None;
                    }
                    more
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        }));
    }
}

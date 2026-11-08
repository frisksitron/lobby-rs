//! Vouching for and declining access requests.

use lobby_protocol::AccessRequestId;
use tracing::{info, warn};

use super::io::Op;
use super::{Session, UiEffect, UiEffects};
use crate::{
    ApiError, copy,
    issues::{Issue, IssueKind},
};

impl Session {
    /// A Vouch or Decline came back. Success shows when the server's `ACCESS_REQUEST_REMOVED`
    /// arrives (to every Member at once); a request someone else handled first (404) just goes
    /// the same way; anything else puts the buttons back and says why.
    pub(super) fn decided(
        &mut self,
        id: &AccessRequestId,
        vouch: bool,
        result: Result<(), ApiError>,
    ) -> UiEffects {
        let mut fx = UiEffects::default();
        match result {
            Ok(()) => info!(request = id.as_str(), vouch, "decision accepted"),
            Err(e) if e.status == Some(404) => {
                info!(request = id.as_str(), "already handled by another Member");
                self.state.deciding.remove(id);
            }
            Err(e) => {
                warn!(request = id.as_str(), vouch, error = %e, "decision failed");
                self.state.deciding.remove(id);
                let name = self
                    .state
                    .access_requests
                    .iter()
                    .find(|r| &r.id == id)
                    .map(|r| r.username.to_string());
                let kind = if vouch {
                    IssueKind::VouchFailed
                } else {
                    IssueKind::DeclineFailed
                };
                fx.0.push(UiEffect::Issue(Issue::new(
                    kind,
                    copy::decision_failed(name.as_deref(), &e),
                )));
            }
        }
        fx
    }

    /// A Vouch, or a Decline: one answer per request until it comes back.
    pub(super) fn decide(&mut self, id: &AccessRequestId, vouch: bool) {
        if !self.state.access_requests.iter().any(|r| &r.id == id)
            || !self.state.deciding.insert(id.clone())
        {
            return;
        }
        info!(request = id.as_str(), vouch, "deciding");
        self.request(Op::Decide {
            id: id.clone(),
            vouch,
        });
    }
}

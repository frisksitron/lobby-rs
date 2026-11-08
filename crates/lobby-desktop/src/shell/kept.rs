//! What the Shell keeps of the Lobbies it left, so a switch back draws their feed, avatars and
//! pictures on its first frame instead of a skeleton and initials. Only the newest
//! [`KEPT_LOBBIES`]: each holds a feed and up to `DECODED_BUDGET_BYTES` of decoded pictures.

use std::{collections::VecDeque, sync::Arc};

use gpui_kit::Entity;
use lobby_client::{LobbyOrigin, LobbyState};

use crate::components::MediaImages;

pub const KEPT_LOBBIES: usize = 3;

pub struct KeptLobby {
    /// What its last Session showed. `None` when that Session ended signed out, so the next
    /// account to sign in there starts from nothing of the last one's.
    pub state: Option<Arc<LobbyState>>,
    pub images: Entity<MediaImages>,
}

/// The newest first.
#[derive(Default)]
pub struct Kept(VecDeque<(LobbyOrigin, KeptLobby)>);

impl Kept {
    pub fn keep(&mut self, origin: LobbyOrigin, lobby: KeptLobby) {
        self.take(&origin);
        self.0.push_front((origin, lobby));
        self.0.truncate(KEPT_LOBBIES);
    }

    pub fn take(&mut self, origin: &LobbyOrigin) -> Option<KeptLobby> {
        let at = self.0.iter().position(|(o, _)| o == origin)?;
        self.0.remove(at).map(|(_, lobby)| lobby)
    }

    pub fn state(&self, origin: &LobbyOrigin) -> Option<&LobbyState> {
        let (_, lobby) = self.0.iter().find(|(o, _)| o == origin)?;
        lobby.state.as_deref()
    }
}

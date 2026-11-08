//! The icons of the Lobbies on this device as GPU images: one [`MediaImages`] for each Lobby, made
//! when its icon is first asked for. The switcher draws every saved Lobby's icon, not only the one
//! the Member is in, and a Lobby's media needs no Sign In (a blob's id is its capability).

use std::{collections::HashMap, rc::Rc};

use gpui_kit::{App, AppContext as _, Context, Entity, ImageSource};
use lobby_client::{Client, LobbyOrigin, Paths, Variant};
use lobby_protocol::BlobId;

use crate::components::{IconOf, MediaImages, picture};

pub struct LobbyIcons {
    client: Client,
    paths: Paths,
    by_origin: HashMap<LobbyOrigin, Entity<MediaImages>>,
}

impl LobbyIcons {
    pub fn new(client: Client, paths: Paths) -> Self {
        Self {
            client,
            paths,
            by_origin: HashMap::new(),
        }
    }

    /// The icon's picture once decoded; the loading starts with the first ask.
    pub fn picture(
        &mut self,
        origin: &LobbyOrigin,
        icon: Option<&BlobId>,
        cx: &mut Context<Self>,
    ) -> Option<ImageSource> {
        let icon = icon?;
        if !self.by_origin.contains_key(origin) {
            let media = self.client.media(origin, &self.paths);
            let images = cx.new(|cx| MediaImages::new(media, cx));
            // A picture arrived: whoever draws a mark needs a frame.
            cx.observe(&images, |_, _, cx| cx.notify()).detach();
            self.by_origin.insert(origin.clone(), images);
        }
        picture(&self.by_origin[origin], Some(icon), cx)
    }

    pub fn shown(&self, origin: &LobbyOrigin, icon: &BlobId, cx: &App) -> bool {
        self.by_origin
            .get(origin)
            .is_some_and(|images| images.read(cx).shown(icon, Variant::Full))
    }
}

pub fn icons_of(icons: Entity<LobbyIcons>) -> IconOf {
    Rc::new(move |origin, icon, cx| icons.update(cx, |i, cx| i.picture(origin, icon, cx)))
}

//! The feed as a view of its own. The Lobby view embeds it cached, so a keystroke in the composer,
//! a pulse in the sidebar or a frame of a watched share redraws no feed rows. It renders again
//! only when the Session, the pictures or the feed itself (a scroll, a hover) changed, when its
//! bounds did, or at local midnight, when "Today" and "Yesterday" move on.

use std::rc::Rc;

use gpui_kit::{Context, Entity, IntoElement, Render, Subscription, Task, WeakEntity, Window};
use lobby_protocol::{Attachment, MessageId};

use super::LobbyView;
use crate::{
    components::{Cards, Feed, FeedAction, MediaImages, card_max_w, pictures, previews},
    session_entity::SessionEntity,
};

pub struct FeedView {
    pub feed: Feed,
    session: Entity<SessionEntity>,
    images: Entity<MediaImages>,
    lobby: WeakEntity<LobbyView>,
    /// The main column's width in design pixels, set by the Lobby view as it lays the column
    /// out. A new width is a new size for this view too, so it renders again with it.
    pub column_w: f32,
    _subscriptions: [Subscription; 2],
    _midnight: Task<()>,
}

impl FeedView {
    /// `seen_before`: see [`Feed::new`].
    pub fn new(
        session: Entity<SessionEntity>,
        images: Entity<MediaImages>,
        lobby: WeakEntity<LobbyView>,
        seen_before: Option<MessageId>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = [
            // Synced here as well as in the render, so the Lobby view, which decides between the
            // rows and the empty feed, sees this state's rows.
            cx.observe(&session, |this, _, cx| {
                this.sync(cx);
                cx.notify();
            }),
            cx.observe(&images, |_, _, cx| cx.notify()),
        ];
        let midnight = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(lobby_client::until_tomorrow())
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            feed: Feed::new(seen_before, cx),
            session,
            images,
            lobby,
            column_w: 0.,
            _subscriptions: subscriptions,
            _midnight: midnight,
        };
        // A Lobby switched back to starts from the feed it kept. Without rows the Lobby view
        // draws the empty feed in place of this view, so nothing would sync them until the
        // Session's first update, which waits on the network.
        this.sync(cx);
        this
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.session.read(cx).shared_state().cloned() {
            self.feed.sync(&state, lobby_client::today(), cx);
        }
    }

    pub fn jump(&mut self, cx: &mut Context<Self>) {
        self.feed.jump(cx);
        cx.notify();
    }

    pub fn sent(&mut self, cx: &mut Context<Self>) {
        self.feed.sent(cx);
        cx.notify();
    }
}

impl Render for FeedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(cx);
        let Some(older) = self.session.read(cx).state().map(|s| s.older) else {
            return gpui_kit::Empty.into_any_element();
        };
        let lobby = self.lobby.clone();
        let on_load: FeedAction = Rc::new(move |_, cx| {
            let _ = lobby.update(cx, |v, cx| v.load_older(cx));
        });
        let this = cx.weak_entity();
        let on_jump: FeedAction = Rc::new(move |_, cx| {
            let _ = this.update(cx, |v, cx| v.jump(cx));
        });
        let on_link: fn(&str, &mut gpui_kit::App) = crate::links::open_external;
        let lobby = self.lobby.clone();
        let changing = self.lobby.clone();
        let cards = Cards {
            images: previews(self.images.clone()),
            avatars: pictures(self.images.clone()),
            max_w: card_max_w(self.column_w),
            on_open: Rc::new(move |a: &Attachment, action, window, cx| {
                let a = a.clone();
                let _ = lobby.update(cx, |v, cx| v.attachment_action(a, action, window, cx));
            }),
            on_change: Rc::new(move |id, change, window, cx| {
                let _ = changing.update(cx, |v, cx| v.change_message(id, change, window, cx));
            }),
        };
        self.feed
            .render(older, on_load, on_jump, on_link, cards, window, cx)
    }
}

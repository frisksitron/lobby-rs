//! Screen Share from the Lobby view: the source picker, sharing, and the viewer of the share on
//! screen (another Member's, or one's own), above the feed or filling the view.

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Pixels, Size, Task, TestSupportExt as _, WeakEntity, Window,
    px, size,
};
use lobby_client::{
    Intent, LobbyState, ScreenSource, SourceId, StrainWarning, Thumbnails, list_sources,
    request_thumbnails,
};
use lobby_protocol::Quality;

use super::{LobbyView, SHARE_SETTLE, forward, sidebar::OnScreen};
use crate::components::{
    PICKER_W, PickerModel, PickerView, Place, Pointer, ShareToolbar, ShareView, Sharer, ToolbarAt,
    Whose, esc_hint, fullscreen_frame, panel_fit, panel_frame,
};
use crate::kit::WindowExt as _;
use crate::scale::device;

/// How long the Esc hint stays up. A second Esc inside it leaves fullscreen.
pub const ESC_HINT_FOR: Duration = Duration::from_millis(2500);

/// What this Member is looking at.
pub enum Viewing {
    Nothing,
    /// A share in the panel above the feed.
    Panel(Shown),
    /// The same share filling the Lobby view below the title bar.
    Fullscreen {
        shown: Shown,
        esc: Esc,
    },
}

/// A share on screen, and the view that draws it.
pub struct Shown {
    /// The Member's own, or the running Watch's, whose Sharer only the Session says.
    pub own: bool,
    pub view: Entity<ShareView>,
    /// Drawing mode: the pointer on the picture draws.
    pub pen: bool,
}

impl Shown {
    /// Whose share is on screen: the Session's answer for a Watch.
    pub fn whose(&self, state: &LobbyState) -> Option<Whose> {
        if self.own {
            return Some(Whose::Mine);
        }
        Some(Whose::Member(state.share.watch.as_ref()?.sharer.clone()))
    }
}

/// Where a fullscreen share stands on Esc: one press shows the hint, a second leaves.
pub enum Esc {
    Quiet,
    /// The task takes the hint down again.
    Hinted(Task<()>),
}

impl Viewing {
    pub fn shown(&self) -> Option<&Shown> {
        match self {
            Self::Nothing => None,
            Self::Panel(shown) | Self::Fullscreen { shown, .. } => Some(shown),
        }
    }

    pub fn shown_mut(&mut self) -> Option<&mut Shown> {
        match self {
            Self::Nothing => None,
            Self::Panel(shown) | Self::Fullscreen { shown, .. } => Some(shown),
        }
    }

    pub fn whose(&self, state: &LobbyState) -> Option<Whose> {
        self.shown()?.whose(state)
    }

    pub fn view(&self) -> Option<&Entity<ShareView>> {
        self.shown().map(|s| &s.view)
    }

    pub fn is_fullscreen(&self) -> bool {
        matches!(self, Self::Fullscreen { .. })
    }

    pub fn hint_shown(&self) -> bool {
        matches!(
            self,
            Self::Fullscreen {
                esc: Esc::Hinted(_),
                ..
            }
        )
    }
}

pub struct Picker {
    view: Entity<PickerView>,
    _load: Option<Task<()>>,
    /// Owns the thumbnail request: closing the picker drops it, which cancels the captures.
    _thumbs: Option<Task<()>>,
}

impl Picker {
    pub fn has_thumbnail(&self, id: &SourceId, cx: &gpui_kit::App) -> bool {
        self.view.read(cx).has_thumbnail(id)
    }
}

impl LobbyView {
    /// The picker lists the sources off the UI thread. Share Screen sits where Join Voice's middle
    /// button was, so the second click of a double-click on Join could land on it: a click within
    /// [`SHARE_SETTLE`] of Voice connecting is dropped.
    pub fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        if self
            .in_voice_since
            .is_some_and(|since| now.duration_since(since) < SHARE_SETTLE)
        {
            tracing::debug!("Share Screen right after Voice connected: taken as a double-click");
            return;
        }
        let test_patterns = self.devices.test_patterns();
        self.close_picker(window, cx);
        let lobby = cx.weak_entity();
        let last = self.lobbies.read(cx).settings().share_quality;
        let view = cx.new(|cx| {
            PickerView::new(
                Rc::new(move |source: &ScreenSource, quality, window, cx| {
                    let source = source.clone();
                    let _ = lobby.update(cx, |v, cx| v.share(source, quality, window, cx));
                }),
                last,
                window,
                cx,
            )
        });
        let body = view.clone();
        let lobby = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let lobby = lobby.clone();
            dialog
                .close_button(false)
                .w(device(PICKER_W, window))
                .child(body.clone())
                .on_close(move |_, _, cx| {
                    let _ = lobby.update(cx, |v, cx| {
                        v.picker = None;
                        cx.notify();
                    });
                })
        });
        let listing = cx
            .background_executor()
            .spawn(async move { list_sources(test_patterns) });
        let load = cx.spawn_in(window, async move |this, cx| {
            let listed = listing.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.show_sources(listed.map_err(|e| e.to_string()), cx);
                if let Some(picker) = &view.picker {
                    picker.view.update(cx, |p, cx| p.focus_first(window, cx));
                }
            });
        });
        self.picker = Some(Picker {
            view,
            _load: Some(load),
            _thumbs: None,
        });
        cx.notify();
    }

    pub fn show_sources(
        &mut self,
        listed: Result<Vec<ScreenSource>, String>,
        cx: &mut Context<Self>,
    ) {
        let test_patterns = self.devices.test_patterns();
        if let Some(picker) = &mut self.picker {
            let model = match listed {
                Ok(sources) => PickerModel::Sources(sources),
                Err(e) => PickerModel::Failed(e),
            };
            picker._load = None;
            picker._thumbs = match &model {
                PickerModel::Sources(sources) => Some(Self::fill_thumbnails(
                    request_thumbnails(sources, test_patterns),
                    picker.view.downgrade(),
                    cx,
                )),
                _ => None,
            };
            picker.view.update(cx, |v, cx| v.set_model(model, cx));
        }
        cx.notify();
    }

    /// Hands each thumbnail to the picker as it arrives. The task owns the request.
    fn fill_thumbnails(
        mut thumbs: Thumbnails,
        view: WeakEntity<PickerView>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |_, cx| {
            while let Some(update) = thumbs.updates.recv().await {
                let Some(thumbnail) = update.thumbnail else {
                    continue;
                };
                if view
                    .update(cx, |v, cx| v.set_thumbnail(&update.id, thumbnail, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
    }

    pub fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            window.close_dialog(cx);
            cx.notify();
        }
    }

    /// A share that starts while nothing is on screen shows in the panel: the moment to check that
    /// the right screen or window goes out. A share watched already stays; "You" is one click away.
    pub fn share(
        &mut self,
        source: ScreenSource,
        quality: Quality,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_picker(window, cx);
        self.lobbies.update(cx, |l, cx| {
            l.update_settings(|s| s.share_quality = quality, cx)
        });
        // A refusal is the Session's Issue.
        let start = Intent::StartShare { source, quality };
        let n = self.session.update(cx, |s, cx| s.dispatch(start, cx));
        self.share_asked = Some(n);
        cx.notify();
    }

    /// The share asked for, once the Session has taken it: one that started shows if nothing else
    /// is on screen.
    pub(super) fn follow_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        if !self.share_asked.is_some_and(|n| session.taken(n)) {
            return;
        }
        self.share_asked = None;
        let started = session.state().is_some_and(|s| s.share.sharing.is_some());
        if started && matches!(self.viewing, Viewing::Nothing) {
            self.show(Sharer::mine(), window, cx)
        }
    }

    /// A Watch that started takes the place of what is on screen, with its feed. A Watch of
    /// another Sharer while one runs needs nothing here: the view follows the Session's answer.
    pub(super) fn follow_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let feed = self.session.update(cx, |s, _| s.take_watch_feed());
        let on = self
            .session
            .read(cx)
            .state()
            .is_some_and(|s| s.share.watch.is_some());
        if let Some(feed) = feed
            && on
        {
            let view = cx.new(|cx| ShareView::new("", feed, cx));
            self.put_on_screen(false, view, window, cx);
        }
    }

    fn follow_sharer(&self, shown: &Shown, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let watched = session.state().and_then(|s| {
            let watch = s.share.watch.as_ref()?;
            let name = s.members.get(&watch.sharer)?.username.to_string();
            Some((watch.sharer.clone(), name))
        });
        if let (false, Some((sharer, name))) = (shown.own, watched) {
            let now = cx.background_executor().now();
            shown
                .view
                .update(cx, |v, _| v.set_sharer(sharer, name.into(), now));
        }
    }

    /// The own share's view closes once the Session says the share is over.
    pub fn stop_share(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::StopShare, cx));
        cx.notify();
    }

    /// Shows `to`'s share where the current one is (the panel or fullscreen). One's own comes from
    /// this computer, so the network watch, if any, stops. A Watch of the Sharer shown still goes
    /// to the Session, which knows whether it takes back a switch.
    pub fn show(&mut self, to: Sharer, window: &mut Window, cx: &mut Context<Self>) {
        match &to.whose {
            Whose::Mine if self.viewing.shown().is_some_and(|s| s.own) => {}
            Whose::Mine => {
                self.session
                    .update(cx, |s, cx| s.dispatch(Intent::Unwatch, cx));
                if let Some(feed) = self.session.read(cx).own_share_feed() {
                    let view = cx.new(|cx| ShareView::own(feed, cx));
                    self.put_on_screen(true, view, window, cx);
                }
            }
            Whose::Member(id) => {
                let watch = Intent::Watch(id.clone());
                self.session.update(cx, |s, cx| s.dispatch(watch, cx));
            }
        }
        cx.notify();
    }

    /// `view` where the current share is (the panel or fullscreen).
    fn put_on_screen(
        &mut self,
        own: bool,
        view: Entity<ShareView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fullscreen = self.viewing.is_fullscreen();
        self.release_viewer(window, cx);
        let shown = Shown {
            own,
            view,
            pen: false,
        };
        if fullscreen {
            self.show_fullscreen(shown, window, cx);
        } else {
            self.set_viewing(Viewing::Panel(shown), cx);
        }
        cx.notify();
    }

    /// Closing a view stops a watch; one's own share goes on.
    pub fn close_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::Unwatch, cx));
        self.release_viewer(window, cx);
        cx.notify();
    }

    /// Every change of [`Viewing`] goes through here: the sidebar's own row says Watching from it,
    /// and its speaking rings hold still while any share is shown.
    fn set_viewing(&mut self, viewing: Viewing, cx: &mut Context<Self>) -> Viewing {
        let on_screen = match viewing.shown() {
            None => OnScreen::Nothing,
            Some(shown) if shown.own => OnScreen::Own,
            Some(_) => OnScreen::Watch,
        };
        self.sidebar
            .update(cx, |s, cx| s.set_on_screen(on_screen, cx));
        std::mem::replace(&mut self.viewing, viewing)
    }

    pub(super) fn release_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was = self.set_viewing(Viewing::Nothing, cx);
        if let Some(viewer) = was.view() {
            viewer.update(cx, |v, _| v.release(window));
        }
        if was.is_fullscreen() {
            self.focus_composer(window, cx);
        }
    }

    /// The overlay holds the focus, so Esc reaches it wherever the focus was.
    fn show_fullscreen(&mut self, shown: Shown, window: &mut Window, cx: &mut Context<Self>) {
        self.set_viewing(
            Viewing::Fullscreen {
                shown,
                esc: Esc::Quiet,
            },
            cx,
        );
        window.focus(&self.fullscreen_focus, cx);
    }

    /// The pointer stays where the button was, which is on the picture now: the controls show
    /// there for a moment, so the way back is in sight.
    pub fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.set_viewing(Viewing::Nothing, cx) {
            Viewing::Nothing => {}
            Viewing::Panel(shown) => self.show_fullscreen(shown, window, cx),
            Viewing::Fullscreen { shown, .. } => {
                self.set_viewing(Viewing::Panel(shown), cx);
                self.focus_composer(window, cx);
            }
        }
        if let Some(view) = self.viewing.view() {
            view.update(cx, |v, cx| v.pointer_at(Pointer::Picture, cx));
        }
        cx.notify();
    }

    /// The first Esc shows the hint for [`ESC_HINT_FOR`]; one while it shows leaves fullscreen.
    pub fn escape_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &mut self.viewing {
            Viewing::Fullscreen { esc, .. } if matches!(esc, Esc::Quiet) => {
                let timer = cx.background_executor().timer(ESC_HINT_FOR);
                *esc = Esc::Hinted(cx.spawn(async move |this, cx| {
                    timer.await;
                    let _ = this.update(cx, |v, cx| v.hint_over(cx));
                }));
                cx.notify();
            }
            Viewing::Fullscreen { .. } => self.toggle_fullscreen(window, cx),
            Viewing::Nothing | Viewing::Panel(_) => {}
        }
    }

    fn hint_over(&mut self, cx: &mut Context<Self>) {
        if let Viewing::Fullscreen { esc, .. } = &mut self.viewing {
            *esc = Esc::Quiet;
            cx.notify();
        }
    }

    /// The share on screen above the feed: the picture's box between the column's gutters
    /// (`main_w` wide, `main_h` high), as [`panel_fit`] says, with its toolbar over its top.
    pub(super) fn viewer_panel(
        &self,
        main_w: f32,
        main_h: f32,
        folded: bool,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Viewing::Panel(shown) = &self.viewing else {
            return None;
        };
        self.follow_sharer(shown, cx);
        let viewer = shown.view.clone();
        let (tile_w, tile_h) = panel_fit(main_w, main_h, folded, viewer.read(cx).stats().size);
        let strain = self.strain_of(shown, cx);
        viewer.update(cx, |v, _| {
            v.set_place(Place::Panel(window_px(tile_w, tile_h, scale)));
            v.set_strain(strain);
        });
        self.show_ink(shown, cx);
        let toolbar = self.toolbar(shown, ToolbarAt::Panel, cx);
        Some(
            panel_frame(toolbar, viewer, (tile_w, tile_h))
                .id("viewer-panel")
                .debug_selector(|| "viewer-panel".into())
                .test_support()
                .into_any_element(),
        )
    }

    fn strain_of(&self, shown: &Shown, cx: &Context<Self>) -> Option<StrainWarning> {
        let share = &self.session.read(cx).state()?.share;
        if shown.own {
            share.strain
        } else {
            share.watch.as_ref()?.strain
        }
    }

    /// The toolbar over the picture of `shown`.
    fn toolbar(&self, shown: &Shown, at: ToolbarAt, cx: &Context<Self>) -> ShareToolbar {
        let (state, draw) = self.toolbar_state(shown, cx);
        ShareToolbar::new(
            state,
            at,
            draw,
            forward(cx, |v: &mut Self, (), window, cx| {
                v.toggle_fullscreen(window, cx)
            }),
            forward(cx, |v: &mut Self, (), window, cx| {
                v.close_viewer(window, cx)
            }),
        )
    }

    /// The share on screen over the whole view under the title bar, which keeps the window's own
    /// controls: the picture, with its toolbar over its top. Esc once says how to leave; Esc
    /// again does. While the pen is down, Esc first ends the stroke.
    pub(super) fn fullscreen_overlay(
        &self,
        width: f32,
        height: f32,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Viewing::Fullscreen { shown, .. } = &self.viewing else {
            return None;
        };
        self.follow_sharer(shown, cx);
        let view = shown.view.clone();
        let strain = self.strain_of(shown, cx);
        view.update(cx, |v, _| {
            v.set_place(Place::Fullscreen(window_px(width, height, scale)));
            v.set_strain(strain);
        });
        self.show_ink(shown, cx);
        let toolbar = self.toolbar(shown, ToolbarAt::Fullscreen, cx);
        let hint = self.viewing.hint_shown().then(|| {
            esc_hint(cx)
                .id("fullscreen-hint")
                .debug_selector(|| "fullscreen-hint".into())
                .test_support()
        });
        Some(
            fullscreen_frame(toolbar, view, cx)
                .id("share-fullscreen-overlay")
                .debug_selector(|| "share-fullscreen-overlay".into())
                .track_focus(&self.fullscreen_focus)
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key != "escape" {
                        return;
                    }
                    cx.stop_propagation();
                    if this.end_stroke(cx) {
                        return;
                    }
                    this.escape_fullscreen(window, cx);
                }))
                .children(hint)
                .test_support()
                .into_any_element(),
        )
    }
}

fn window_px(w: f32, h: f32, scale: f32) -> Size<Pixels> {
    size(px(w * scale), px(h * scale)) // px: the picture is fitted in window pixels
}

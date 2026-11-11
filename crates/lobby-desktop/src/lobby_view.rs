//! The Lobby view: the title bar (the Lobby, tinted while the connection is not calm, and the
//! Notification Center), the feed plus the composer, and the right sidebar (one list of sections,
//! with the Voice controls pinned at its foot). The Screen Share on screen, another Member's or
//! one's own, has its viewer above the feed. What goes wrong is a notification (a toast plus a
//! Center entry), never a line that moves the content. The connection says itself by the bar's
//! tint and a live Center entry, with a toast only where the person must act.
//!
//! Below [`NARROW_BELOW`] the sidebar folds behind a title-bar button and Voice's controls dock in
//! a strip above the composer. The view observes the `SessionEntity` and forwards intents; the
//! [`ComposerView`] and the [`MemberSidebar`] own their own state and report up by events.
mod chrome;
mod composer;
mod decisions;
mod feed_placeholder;
mod feed_view;
mod ink;
mod member_list;
mod notifications;
mod overlay;
mod own_card;
mod share;
mod sidebar;
mod toasts;
mod voice;

use std::{rc::Rc, time::Duration};

use gpui_kit::{
    AnyElement, AnyView, AppContext as _, ClipboardItem, Context, DragMoveEvent, Entity,
    EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, MouseDownEvent,
    ParentElement as _, Render, SharedString, StyleRefinement, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, actions, base::GlobalState, canvas, div, linear_color_stop,
    linear_gradient, prelude::FluentBuilder as _,
};
use lobby_client::{Intent, Issue, IssueKind, LobbyState, Phase, UiEffect, local_time};
use lobby_protocol::{AccessRequestId, Attachment, MessageId, UnixMs};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use composer::composer_placeholder;
pub use composer::{ComposerEvent, ComposerView, RESTORE_AFTER_PAUSE};
pub(crate) use feed_placeholder::feed_placeholder;
pub use feed_view::FeedView;
pub use member_list::{MemberGroups, OpenPanel, member_groups, sharers};
pub use share::{ESC_HINT_FOR, Esc, Picker, Shown, Viewing};
pub use sidebar::{
    FOLDED_SHEET_W, MAIN_MIN, MemberSidebar, NARROW_BELOW, SHEET_INSET, SIDEBAR_MAX, SIDEBAR_MIN,
    SIDEBAR_SLIM_W, SIDEBAR_STEP, SIDEBAR_W, SidebarEvent, SidebarLayout,
};
pub use toasts::toast_right_margin;
pub(crate) use toasts::{connection_toast, issue_toast};
pub use voice::VoiceDevices;

use crate::kit::{ActiveTheme as _, Theme, WindowExt as _};
use crate::scale::{design_viewport, device, u, window_scale};
use crate::{
    components::attachment_dialog::dialog_width,
    components::{
        ActionContext, AppTitleBar, AttachmentDialog, Callback, CardAction, ConnectionNotice,
        DecisionRow, LobbySwitcher, MediaImages, MessageChange, SwitcherCurrent, SwitcherPick,
        TITLE_BAR_HEIGHT, VoiceBarActions, VoiceBarModel, VoiceStatsDialog, busy_screen,
        connection_notice, typing_indicator, voice_bar_model,
    },
    issues::IssueCenter,
    lobbies::Lobbies,
    lobby_icons::LobbyIcons,
    session_entity::{SessionEntity, SessionUiEvent},
};

/// A child's callback that reaches the view `cx` belongs to while it lives.
fn forward<V: 'static, A: 'static>(
    cx: &Context<V>,
    f: impl Fn(&mut V, A, &mut Window, &mut Context<V>) + 'static,
) -> Callback<A> {
    let this = cx.weak_entity();
    Rc::new(move |a, window, cx| {
        let _ = this.update(cx, |v, cx| f(v, a, window, cx));
    })
}

/// A click on Share Screen this soon after Voice connected is the second half of a double-click
/// on Join Voice, which stood in the same place.
pub const SHARE_SETTLE: Duration = Duration::from_millis(500);

actions!(lobby, [WidenSidebar, NarrowSidebar]);

const COPIED_FOR: Duration = Duration::from_millis(1500);
/// The air between the title bar and the tops of the share and the member sheet.
pub const BODY_TOP: f32 = 4.;
/// The window height, in design pixels, below which the empty feed is one line: the icon, title
/// and description no longer fit between the title bar and the composer.
const EMPTY_STATE_MIN_H: f32 = 300.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LobbyViewEvent {
    ChangeLobby,
    Switch(SwitcherPick),
    SignOut,
    OpenSettings,
    /// The Members button was pressed under the Settings page: the page gives way to them.
    LeaveSettings,
}

fn voice_active(state: &LobbyState) -> bool {
    matches!(voice_bar_model(state), VoiceBarModel::InVoice { .. })
}

fn saved_or_live_name(
    session: &Entity<SessionEntity>,
    lobbies: &Entity<Lobbies>,
    cx: &gpui_kit::App,
) -> String {
    let session = session.read(cx);
    let Some(origin) = session.origin() else {
        return String::new();
    };
    let live = session
        .state()
        .and_then(|s| s.lobby.as_ref())
        .map(|l| l.name.as_str());
    lobbies
        .read(cx)
        .name_or(origin, live)
        .unwrap_or_default()
        .to_owned()
}

pub struct LobbyView {
    session: Entity<SessionEntity>,
    issues: Entity<IssueCenter>,
    lobbies: Entity<Lobbies>,
    devices: VoiceDevices,
    pub composer: Entity<ComposerView>,
    pub sidebar: Entity<MemberSidebar>,
    pub feed: Entity<FeedView>,
    /// The decoded attachment images: one owner, which frees their atlas tiles.
    pub images: Entity<MediaImages>,
    /// The saved Lobbies' icons, for the title bar's switcher.
    icons: Entity<LobbyIcons>,
    pub attachment_dialog: Option<Entity<AttachmentDialog>>,
    pub voice_stats: Option<Entity<VoiceStatsDialog>>,
    pub viewing: Viewing,
    /// The share asked for (its Intent's number), until the Session has taken it.
    share_asked: Option<u64>,
    /// Held by the fullscreen overlay, so its Esc is heard wherever the focus was.
    fullscreen_focus: FocusHandle,
    pub picker: Option<Picker>,
    /// A page that takes the place of the body under the title bar (Settings). The feed and the
    /// composer stay as they were.
    page: Option<AnyView>,
    /// Join, Leave, Mute and Share, for the sidebar's controls and the folded strip alike.
    voice: VoiceBarActions,
    last_connected: Option<SharedString>,
    was_connected: bool,
    reconnects: u32,
    /// The phase whose toast was raised, so a state that needs the person toasts once.
    connection_toasted: Option<std::mem::Discriminant<Phase>>,
    history_notified: bool,
    /// Wakes the controls when a Join or unmute cooldown ends.
    waits_timer: Option<Task<()>>,
    pub center_open: bool,
    in_voice_since: Option<std::time::Instant>,
    /// The Lobby the title bar showed at the last render, kept while the Session closes.
    bar_lobby: Option<SwitcherCurrent>,
    vouching: HashMap<AccessRequestId, DecisionRow>,
    /// The Ask to Join toasts on screen, so a request that was handled takes its toast with it.
    decision_toasts: BTreeSet<AccessRequestId>,
    /// Requests this Member just handled: their rows say so for [`SETTLE_FOR`], then fold away
    /// (a Decline goes out only then, so Undo can take it back). Each with its timer.
    ///
    /// [`SETTLE_FOR`]: crate::components::SETTLE_FOR
    settled: BTreeMap<AccessRequestId, (DecisionRow, Task<()>)>,
    copied_until: Option<std::time::Instant>,
    copied_timer: Option<Task<()>>,
    /// Over the monitor or window this Member shares, while they share one.
    overlay: Option<overlay::Overlay>,
    /// The OS window this view is in, which may be on the shared monitor.
    host_window: Option<crate::host::NativeWindow>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LobbyViewEvent> for LobbyView {}

/// What the Shell shares with every Lobby view it opens.
pub struct Shared {
    pub issues: Entity<IssueCenter>,
    pub lobbies: Entity<Lobbies>,
    pub icons: Entity<LobbyIcons>,
    pub devices: VoiceDevices,
}

impl LobbyView {
    pub fn new(
        session: Entity<SessionEntity>,
        images: Entity<MediaImages>,
        shared: Shared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Shared {
            issues,
            lobbies,
            icons,
            devices,
        } = shared;
        let placeholder = composer_placeholder(&saved_or_live_name(&session, &lobbies, cx));
        let seen_before = session
            .read(cx)
            .origin()
            .and_then(|origin| lobbies.read(cx).seen(origin));
        // Before this view's own observer of the Session, so the rows are synced when it runs.
        let lobby = cx.weak_entity();
        let feed =
            cx.new(|cx| FeedView::new(session.clone(), images.clone(), lobby, seen_before, cx));
        let scroller = feed.read(cx).feed.scroller().clone();
        let composer = cx.new(|cx| {
            ComposerView::new(
                session.clone(),
                lobbies.clone(),
                images.clone(),
                placeholder,
                window,
                cx,
            )
        });
        composer.update(cx, |c, cx| c.focus(window, cx));
        let voice = VoiceBarActions {
            join: Rc::new(cx.listener(|this, _, window, cx| this.join_voice(window, cx))),
            leave: Rc::new(cx.listener(|this, _, window, cx| this.leave_voice(window, cx))),
            mute: Rc::new(cx.listener(|this, _, _, cx| this.toggle_mute(cx))),
            deafen: Rc::new(cx.listener(|this, _, _, cx| this.toggle_deafen(cx))),
            share: Rc::new(cx.listener(|this, _, window, cx| this.open_picker(window, cx))),
            stop_share: Rc::new(cx.listener(|this, _, window, cx| this.stop_share(window, cx))),
        };
        let sidebar = cx.new(|cx| {
            MemberSidebar::new(
                session.clone(),
                lobbies.clone(),
                images.clone(),
                voice.clone(),
                cx,
            )
        });
        let mut subscriptions = vec![
            cx.observe_in(&session, window, |this, _, window, cx| {
                this.on_session_changed(window, cx);
            }),
            cx.observe(&issues, |_, _, cx| cx.notify()),
            // The feed's tail is in front of the Member when they scroll back to it, and when
            // the window comes back.
            cx.observe_in(&scroller, window, |this, _, window, cx| {
                this.remember_seen(window, cx);
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.remember_seen(window, cx);
            }),
            // An image arrived: its card already has its size, so a frame is all it needs.
            cx.observe(&images, |_, _, cx| cx.notify()),
            cx.observe(&icons, |_, _, cx| cx.notify()),
            cx.observe(&lobbies, |_, _, cx| cx.notify()),
            // The bell's "Send again" and the members button follow these two.
            cx.observe(&composer, |_, _, cx| cx.notify()),
            cx.observe(&sidebar, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &session,
                window,
                |this, _, ev: &SessionUiEvent, window, cx| {
                    this.on_effects(ev, window, cx);
                },
            ),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, ev: &ComposerEvent, window, cx| {
                    match ev {
                        ComposerEvent::Notify(issue) => this.notify(issue.clone(), window, cx),
                        ComposerEvent::Dismiss(kind) => this.dismiss(*kind, window, cx),
                        ComposerEvent::Sent => this.feed.update(cx, |f, cx| f.sent(cx)),
                    }
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &sidebar,
                window,
                |this, _, ev: &SidebarEvent, window, cx| match ev {
                    SidebarEvent::Watch(sharer) => {
                        // A folded sheet would cover the picture it was just asked for.
                        this.sidebar
                            .update(cx, |s, cx| s.set_open(false, window, cx));
                        this.show(sharer.clone(), window, cx)
                    }
                    SidebarEvent::StopWatching => this.close_viewer(window, cx),
                    SidebarEvent::VoiceStats => this.open_voice_stats(window, cx),
                    SidebarEvent::SignOut => this.sign_out(cx),
                    SidebarEvent::Folded | SidebarEvent::CardClosed => {
                        this.focus_composer(window, cx)
                    }
                },
            ),
        ];
        subscriptions.push(cx.on_release(|this, cx| this.close_overlay(cx)));
        // A move onto or off the shared monitor changes where ink is painted.
        subscriptions.push(cx.observe_window_bounds(window, |this, _, cx| {
            if this.overlay.is_some() {
                cx.notify();
            }
        }));
        Self {
            session,
            issues,
            lobbies,
            devices,
            composer,
            sidebar,
            feed,
            images,
            icons,
            attachment_dialog: None,
            voice_stats: None,
            viewing: Viewing::Nothing,
            share_asked: None,
            fullscreen_focus: cx.focus_handle(),
            picker: None,
            page: None,
            voice,
            last_connected: None,
            was_connected: false,
            reconnects: 0,
            connection_toasted: None,
            history_notified: false,
            waits_timer: None,
            center_open: false,
            in_voice_since: None,
            bar_lobby: None,
            vouching: HashMap::new(),
            decision_toasts: BTreeSet::new(),
            settled: BTreeMap::new(),
            copied_until: None,
            copied_timer: None,
            overlay: None,
            host_window: crate::host::NativeWindow::of(window),
            _subscriptions: subscriptions,
        }
    }

    pub fn lobby_name(&self, cx: &gpui_kit::App) -> String {
        saved_or_live_name(&self.session, &self.lobbies, cx)
    }

    pub fn session_entity(&self) -> &Entity<SessionEntity> {
        &self.session
    }

    pub fn issues_center<'a>(&self, cx: &'a gpui_kit::App) -> &'a IssueCenter {
        self.issues.read(cx)
    }

    pub fn reconnects(&self) -> u32 {
        self.reconnects
    }

    /// The live connection entry: `None` while the Lobby is calm.
    pub fn connection_notice(&self, cx: &gpui_kit::App) -> Option<ConnectionNotice> {
        let state = self.session.read(cx).state()?;
        connection_notice(state, &self.lobby_name(cx), self.last_connected.as_deref())
    }

    fn connected(&self, cx: &gpui_kit::App) -> bool {
        self.session
            .read(cx)
            .state()
            .is_some_and(|s| s.phase == Phase::Connected)
    }

    /// What the Member has seen of this Lobby: the newest Message while the feed is at its tail in
    /// the focused window with no page over it. The feed marks what came after it the next time
    /// the Lobby opens. This is the one place that decides "seen".
    fn remember_seen(&self, window: &Window, cx: &mut Context<Self>) {
        if !window.is_window_active()
            || self.page.is_some()
            || !self.feed.read(cx).feed.at_bottom(cx)
        {
            return;
        }
        let session = self.session.read(cx);
        let (Some(origin), Some(newest)) = (
            session.origin().cloned(),
            session.state().and_then(LobbyState::newest_message),
        ) else {
            return;
        };
        self.lobbies
            .update(cx, |lobbies, cx| lobbies.set_seen(&origin, newest, cx));
    }

    /// What follows from a Session change outside the render: a share on screen that ended (a
    /// watch, or one's own) closes the viewer, the picker closes once nothing can be shared, and a
    /// failed history load is notified once.
    fn on_session_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_seen(window, cx);
        self.follow_watch(window, cx);
        self.follow_share(window, cx);
        self.settle_vouches(cx);
        self.follow_drawing(cx);
        self.follow_overlay(cx);
        let Some(s) = self.session.read(cx).state() else {
            return;
        };
        let ended = self.viewing.shown().is_some_and(|shown| {
            if shown.own {
                s.share.sharing.is_none()
            } else {
                s.share.watch.is_none()
            }
        });
        let voice_ok = voice_active(s);
        let connected = s.phase == Phase::Connected;
        let placeholder = composer_placeholder(&self.lobby_name(cx));
        let history_failed = matches!(s.history, lobby_client::History::Failed(_));
        let waits = [s.voice.unmute_after, s.voice.join_after];
        if ended {
            self.release_viewer(window, cx);
        }
        if !voice_ok {
            self.close_picker(window, cx);
            self.close_voice_stats(window, cx);
            self.in_voice_since = None;
        } else if self.in_voice_since.is_none() {
            self.in_voice_since = Some(cx.background_executor().now());
        }
        if connected != self.was_connected {
            if self.was_connected {
                self.last_connected = Some(local_time(UnixMs::now()).into());
            }
            self.was_connected = connected;
        }
        self.composer
            .update(cx, |c, cx| c.set_placeholder(placeholder, window, cx));
        self.toast_connection(window, cx);
        if history_failed && !self.history_notified {
            self.history_notified = true;
            let issue = Issue::new(IssueKind::HistoryFailed, lobby_client::copy::HISTORY_FAILED);
            self.notify(issue, window, cx);
        } else if !history_failed {
            self.history_notified = false;
        }
        self.arm_waits(waits, cx);
        cx.notify();
    }

    /// A frame when the nearest of the Join and unmute cooldowns ends, so the control wakes up on
    /// its own; then the next one, if the other is still running.
    fn arm_waits(&mut self, waits: [Option<std::time::Instant>; 2], cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let next = waits.into_iter().flatten().filter(|at| *at > now).min();
        self.waits_timer = next.map(|at| {
            let timer = cx.background_executor().timer(at - now);
            cx.spawn(async move |this, cx| {
                timer.await;
                let _ = this.update(cx, |v, cx| {
                    v.arm_waits(waits, cx);
                    // The sidebar's Voice controls show the waits too, and are drawn cached.
                    v.sidebar.update(cx, |_, cx| cx.notify());
                    cx.notify();
                });
            })
        });
    }

    pub fn action_context(&self, cx: &gpui_kit::App) -> ActionContext {
        ActionContext {
            connected: self.connected(cx),
            can_resend: self.composer.read(cx).can_resend(cx),
        }
    }

    pub fn set_page(&mut self, page: Option<AnyView>, cx: &mut Context<Self>) {
        if page.is_some() {
            self.sidebar.update(cx, |s, cx| s.close_card(cx));
        }
        self.page = page;
        cx.notify();
    }

    pub fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        tracing::info!("sign out requested");
        cx.emit(LobbyViewEvent::SignOut);
    }

    pub fn copy_invite(&mut self, cx: &mut Context<Self>) {
        let Some(origin) = self.session.read(cx).origin() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(origin.to_string()));
        let until = cx.background_executor().now() + COPIED_FOR;
        self.copied_until = Some(until);
        let timer = cx.background_executor().timer(COPIED_FOR);
        self.copied_timer = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |v, cx| {
                v.copied_until = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn on_effects(&mut self, ev: &SessionUiEvent, window: &mut Window, cx: &mut Context<Self>) {
        for effect in &ev.0.0 {
            match effect {
                UiEffect::Issue(issue) => self.notify(issue.clone(), window, cx),
                UiEffect::Sent { draft } => {
                    self.composer.update(cx, |c, cx| c.sent(draft, window, cx));
                }
                UiEffect::SendFailed { draft, issue } => {
                    self.composer
                        .update(cx, |c, cx| c.restore(draft, window, cx));
                    self.notify(issue.clone(), window, cx);
                }
                UiEffect::SendRecovered { draft } => {
                    self.composer
                        .update(cx, |c, cx| c.recovered(draft, window, cx));
                }
                UiEffect::Reconnected => {
                    self.reconnects += 1;
                    // Voice that ended with the connection: now Rejoin works, so say it again
                    // (once; the Center has kept it all along).
                    let ended = self.rejoin_issue(cx);
                    if let Some(issue) = ended
                        && !self.toasts_held(cx)
                    {
                        let toast = issue_toast(&issue, cx.weak_entity());
                        window.push_notification(toast, cx);
                    }
                }
                // The sidebar closes the Member panel the owner acted from.
                UiEffect::OwnerActed { .. } => {}
                UiEffect::NeedsAuth(_) | UiEffect::PersistLobby(_) | UiEffect::Left => {}
                // The own Member Card follows its own answers.
                UiEffect::Renamed(_) | UiEffect::NameTaken => {}
                // The Shell owns the sounds: it knows the Settings.
                UiEffect::Sound(_) => {}
                // The Shell owns the OS toast: it knows the window and the tray.
                UiEffect::MessageArrived(_) => {}
                UiEffect::AccessRequested(request) => self.toast_decision(request, window, cx),
                UiEffect::AccessRequestGone(id) => self.drop_decision_toast(id, window, cx),
            }
        }
        cx.notify();
    }

    pub fn open_attachment(
        &mut self,
        attachment: Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<AttachmentDialog> {
        self.close_attachment(window, cx);
        let lobby = cx.weak_entity();
        let images = self.images.clone();
        let title: SharedString = attachment.name.clone().into();
        let view = cx.new(|cx| AttachmentDialog::new(attachment, images, lobby.clone(), cx));
        let body = view.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            let lobby = lobby.clone();
            // Not `.centered()`: the kit's slide-down entrance holds a centred dialog at the
            // window's top for most of its run, then drops it into place.
            dialog
                .title(title.clone())
                .w(device(dialog_width(design_viewport(window).0), window))
                .child(body.clone())
                .footer(AttachmentDialog::footer(&body, cx))
                .on_close(move |_, _, cx| {
                    let _ = lobby.update(cx, |v, cx| {
                        v.attachment_dialog = None;
                        cx.notify();
                    });
                })
        });
        self.attachment_dialog = Some(view.clone());
        cx.notify();
        view
    }

    /// A card's menu does what the preview dialog's buttons do. The dialog opens first: it holds
    /// the work's progress, its failure and "Show in folder".
    pub fn attachment_action(
        &mut self,
        attachment: Attachment,
        action: CardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = self.open_attachment(attachment, window, cx);
        dialog.update(cx, |d, cx| match action {
            CardAction::View => {}
            CardAction::Open => d.open(window, cx),
            CardAction::Save => d.save(window, cx),
        });
    }

    /// Edit puts the Message's text in the composer; Delete goes at once, with no question.
    pub fn change_message(
        &mut self,
        id: MessageId,
        change: MessageChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match change {
            MessageChange::Edit => {
                let content = self
                    .session
                    .read(cx)
                    .state()
                    .filter(|s| s.may_change(id))
                    .map(|s| s.chat[&id].content.clone());
                if let Some(content) = content {
                    self.composer
                        .update(cx, |c, cx| c.start_edit(id, content, window, cx));
                }
            }
            MessageChange::Delete => {
                self.session
                    .update(cx, |s, cx| s.dispatch(Intent::DeleteMessage(id), cx));
            }
        }
    }

    pub fn close_attachment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.attachment_dialog.take().is_some() {
            window.close_dialog(cx);
            cx.notify();
        }
    }

    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::LoadOlder, cx));
        cx.notify();
    }

    pub fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        self.feed.update(cx, |f, cx| f.jump(cx));
        cx.notify();
    }

    /// The window keeps its chrome while the Session closes, the Lobby's mark included.
    fn closing(&self, signing_out: bool, cx: &Context<Self>) -> AnyElement {
        let (id, label) = if signing_out {
            ("signing-out", "Signing out…")
        } else {
            ("leaving", "Leaving…")
        };
        div()
            .id("lobby")
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(AppTitleBar::new().left(LobbySwitcher::still(self.bar_lobby.clone())))
            .child(busy_screen(id, label, cx))
            .into_any_element()
    }

    fn placeholder(&self, state: &LobbyState, tall: bool, cx: &Context<Self>) -> AnyElement {
        let alone = state.members.len() <= 1;
        let origin = self.session.read(cx).origin();
        let invite: Option<SharedString> = alone
            .then(|| origin.map(|o| o.to_string().into()))
            .flatten();
        // A Lobby not reached since it was opened, after its first try: the feed says so calmly
        // instead of a skeleton that never resolves.
        let before_ready = state.me.is_none();
        let unreached = (before_ready && !matches!(state.phase, Phase::Connecting { attempt: 0 }))
            .then(|| self.connection_notice(cx))
            .flatten()
            .map(|notice| {
                let color = origin
                    .map_or_else(|| cx.theme().primary, |o| self.lobbies.read(cx).color_of(o));
                (color, notice)
            });
        let copied = self
            .copied_until
            .is_some_and(|u| u > cx.background_executor().now());
        // A watched share takes most of the column, and so does a big scale in a short window:
        // the empty state shrinks to one line.
        let roomy = self.viewing.view().is_none() && tall;
        feed_placeholder(
            &state.history,
            &self.lobby_name(cx),
            invite,
            copied,
            unreached,
            roomy,
            cx.weak_entity(),
            cx,
        )
    }

    /// The feed's area: whatever the feed holds stays in it, never over the viewer.
    fn feed_area(
        &self,
        feed: AnyElement,
        typing: Option<SharedString>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let background = cx.theme().background;
        div()
            .id("feed")
            .debug_selector(|| "feed".into())
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(feed)
            // Who is typing floats over the foot, of the rows or of an empty feed, so nothing
            // moves. Here, outside the cached feed, its dots render this view and not the rows.
            .children(typing.map(|line| typing_indicator(line, cx)))
            // A soft edge where the feed meets the viewer or the title bar, instead of a hard cut
            // through a line of text.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(u(12.))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(background, 0.),
                        linear_color_stop(background.opacity(0.), 1.),
                    )),
            )
            // The kit's text selection starts from a press anywhere in the window. Only the chat
            // has text to select, and when Windows moves or resizes the window the mouse-up never
            // reaches the app, so a selection started on the frame would follow the pointer.
            .child(
                canvas(
                    |_, _, _| {},
                    |chat, _, window, _| {
                        window.on_mouse_event(move |press: &MouseDownEvent, phase, _, cx| {
                            if phase.bubble() && !chat.contains(&press.position) {
                                GlobalState::suppress_text_selection(cx);
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .test_support()
            .into_any_element()
    }
}

impl Render for LobbyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // What left the decoded LRU is freed before any card asks for its image.
        self.images.update(cx, |m, _| m.release(window));
        self.paint_overlay(cx);
        let (width, height) = design_viewport(window);
        let scale = window_scale(window);
        let layout = self.sidebar.read(cx).layout(window, cx);
        let folded = layout.docked().is_none();
        let main_w = width - layout.taken();
        // Toasts sit in the main column, never over the member panel.
        let toast_right = toast_right_margin(layout, scale);
        if cx.theme().notification.margins.right != toast_right {
            cx.defer(move |cx| {
                Theme::global_mut(cx).notification.margins.right = toast_right;
                cx.refresh_windows();
            });
        }
        let session = self.session.read(cx);
        let Some(state) = session.state() else {
            return self.closing(session.signing_out, cx);
        };
        let title_bar = self.title_bar(state, layout, window, cx);
        let strip = folded.then(|| self.voice_strip(state, cx));
        let placeholder = self
            .feed
            .read(cx)
            .feed
            .rows()
            .is_empty()
            .then(|| self.placeholder(state, height >= EMPTY_STATE_MIN_H, cx));
        let now = cx.background_executor().now();
        let typing: Option<SharedString> = self
            .session
            .read(cx)
            .typing_line(now)
            .map(SharedString::from);
        let feed = match placeholder {
            Some(placeholder) => placeholder,
            None => {
                self.feed.update(cx, |f, _| f.column_w = main_w);
                AnyView::from(self.feed.clone())
                    .cached(StyleRefinement::default().size_full())
                    .into_any_element()
            }
        };
        let main_h = height - TITLE_BAR_HEIGHT;
        let column_h = main_h - BODY_TOP - SHEET_INSET;
        let viewer = self.viewer_panel(main_w, column_h, folded, scale, cx);
        // Fullscreen covers the body's padding too.
        let fullscreen = self.fullscreen_overlay(width, main_h, scale, cx);
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children(viewer)
            .child(self.feed_area(feed, typing, cx))
            .children(strip)
            .child(div().px_4().pt_2().child(self.composer.clone()));
        // One padding for both columns: the share's top lines up with the member sheet's, and the
        // composer's bottom with the sheet's.
        let body = div()
            .id("lobby-body")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .pt(u(BODY_TOP))
            .pb(u(SHEET_INSET))
            .child(main)
            .on_drag_move(cx.listener(
                |this, e: &DragMoveEvent<sidebar::SidebarDrag>, window, cx| {
                    let from_right = f32::from(e.bounds.right() - e.event.position.x);
                    this.sidebar
                        .update(cx, |s, cx| s.drag(from_right, window, cx));
                },
            ))
            .child(MemberSidebar::cached(&self.sidebar, window, cx))
            .children(fullscreen);
        let sheet_open = layout == SidebarLayout::Sheet
            && self.page.is_none()
            && !self.viewing.is_fullscreen()
            && self.sidebar.read(cx).is_open();
        let card_open = self.page.is_none() && self.sidebar.read(cx).member_panel.is_some();
        div()
            .id("lobby")
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &WidenSidebar, window, cx| {
                this.sidebar
                    .update(cx, |s, cx| s.step(SIDEBAR_STEP, window, cx));
            }))
            .on_action(cx.listener(|this, _: &NarrowSidebar, window, cx| {
                this.sidebar
                    .update(cx, |s, cx| s.step(-SIDEBAR_STEP, window, cx));
            }))
            // Escape reaches the open card or sheet wherever the keyboard is.
            .when(sheet_open || card_open, |d| {
                d.capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape" {
                        this.sidebar.update(cx, |s, cx| s.escape(window, cx));
                        cx.stop_propagation();
                    }
                }))
            })
            .child(title_bar)
            .child(match &self.page {
                Some(page) => page.clone().into_any_element(),
                None => body.into_any_element(),
            })
            .into_any_element()
    }
}

//! The Settings page: an in-window route that takes the place of the body under the title bar, so
//! it fits the small window. A header with the way back, then the Voice, Window and Updates
//! sections in a column that scrolls. Everything here is for this computer, none of it for a
//! Lobby. A change is written through `Lobbies` at once and the Shell applies what it shows.

use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _,
        button::ButtonVariants as _,
        scroll::{Scrollbar, ScrollbarAxis},
        switch::Switch,
    },
    div,
};
use lobby_client::{
    UI_SCALE_MAX, UI_SCALE_MIN, copy, step_ui_scale,
    update::{Check, UpdateStatus},
};

use crate::scale::u;
use crate::{
    components::{
        ICON_MD, PAGE_CONTROL_H, PERCENT_W, READOUT_GAP, page_button, page_icon_button,
        section_label,
    },
    lobbies::Lobbies,
    lobby_view::VoiceDevices,
    session_entity::SessionEntity,
    updates::AppUpdates,
    voice_settings::VoiceSettingsView,
};

/// The rows keep to a readable column on a wide window.
const COLUMN_MAX_W: f32 = 520.;
const HEADER_H: f32 = 44.;
const CLOSE_TO_TRAY: &str = "Keep Lobby running in the tray when the window closes";
/// The air left of the Back arrow in its square button, which the header takes back so the
/// arrow, not its hover box, lines up with the section labels: (36 - 16) / 2.
const BACK_INSET: f32 = (PAGE_CONTROL_H - ICON_MD) / 2.;
/// Sections stand well apart, and each heading sits close to what it names.
pub(crate) const SECTION_GAP: f32 = 44.;
pub(crate) const HEADING_GAP: f32 = 16.;
/// Groups inside a section stand apart by less, rows inside a group by less still.
pub(crate) const GROUP_GAP: f32 = 28.;
pub(crate) const LABEL_GAP: f32 = 8.;
pub(crate) const ROW_GAP: f32 = 8.;
/// A row is as tall as its tallest control, so a control that comes and goes moves nothing.
const ROW_H: f32 = PAGE_CONTROL_H;
/// What sits beside a row's label, and under it when they do not fit on one line.
const ROW_SIDE_GAP: f32 = 12.;
const ROW_WRAP_GAP: f32 = 6.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEvent {
    Close,
}

pub struct SettingsView {
    lobbies: Entity<Lobbies>,
    /// Whether the window has a tray icon at all: without one the close button always quits, so
    /// there is nothing to choose.
    tray: bool,
    /// Which devices Voice has here, and the Session whose call the Voice meter follows.
    devices: VoiceDevices,
    session: Option<Entity<SessionEntity>>,
    /// Made on the first draw and dropped with the page: it listens to the microphone and watches
    /// the device lists.
    voice: Option<Entity<VoiceSettingsView>>,
    /// None in a build that does not update itself.
    updates: Option<Entity<AppUpdates>>,
    _updates_seen: Option<Subscription>,
    /// The page's own: the overlay scrollbar reads where the body is scrolled to.
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(
        lobbies: Entity<Lobbies>,
        tray: bool,
        devices: VoiceDevices,
        session: Option<Entity<SessionEntity>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let updates = AppUpdates::entity(cx);
        let _updates_seen = updates
            .as_ref()
            .map(|updates| cx.observe(updates, |_, _, cx| cx.notify()));
        Self {
            lobbies,
            tray,
            devices,
            session,
            voice: None,
            updates,
            _updates_seen,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        }
    }

    /// The Voice meter follows the call of the route's Session.
    pub fn set_session(&mut self, session: Option<Entity<SessionEntity>>, cx: &mut Context<Self>) {
        if self.session != session {
            self.session = session;
            self.voice = None;
        }
        cx.notify();
    }

    pub fn voice(&self) -> Option<&Entity<VoiceSettingsView>> {
        self.voice.as_ref()
    }

    /// Esc closes the page, so the keyboard needs to be here.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(SettingsEvent::Close);
        }
    }

    /// What the updater is doing, in one line. Downloading, the title bar shows the progress too;
    /// ready, both offer the restart.
    fn updates_section(&self, cx: &Context<Self>) -> impl IntoElement {
        let version = env!("CARGO_PKG_VERSION");
        let Some(updates) = self.updates.clone() else {
            let body = row(copy::updates_local_build(version), div(), cx);
            return section(
                copy::UPDATES,
                div().id("updates").child(body).test_support(),
                cx,
            );
        };
        let check = |checking: bool| {
            let updates = updates.clone();
            page_button("update-check", cx)
                .debug_selector(|| "update-check".into())
                .outline()
                .label(copy::UPDATE_CHECK_NOW)
                .loading(checking)
                .disabled(checking)
                .on_click(move |_, _, cx| updates.read(cx).check_now())
                .into_any_element()
        };
        let (line, control): (String, _) = match updates.read(cx).status().clone() {
            UpdateStatus::Current(found) => (
                copy::updates_current(version, found),
                check(found == Check::Checking),
            ),
            UpdateStatus::Downloading {
                version,
                received,
                total,
            } => (
                copy::updates_downloading(&version.to_string(), received, total),
                div().into_any_element(),
            ),
            UpdateStatus::Ready { version, .. } => {
                let updates = updates.clone();
                (
                    copy::updates_ready(&version.to_string()),
                    page_button("settings-update-restart", cx)
                        .debug_selector(|| "settings-update-restart".into())
                        .primary()
                        .label(copy::UPDATE_RESTART)
                        .on_click(move |_, _, cx| updates.update(cx, |u, cx| u.restart(cx)))
                        .into_any_element(),
                )
            }
            UpdateStatus::Failed { version, .. } => {
                (copy::updates_failed(&version.to_string()), check(false))
            }
        };
        section(
            copy::UPDATES,
            div()
                .id("updates")
                .child(row(line, control, cx))
                .test_support(),
            cx,
        )
    }

    /// Size, and Close to tray where the window has a tray to keep running in.
    fn window_section(&self, cx: &Context<Self>) -> impl IntoElement {
        let settings = self.lobbies.read(cx).settings();
        let (ui_scale, close_to_tray) = (settings.ui_scale, settings.close_to_tray);
        let lobbies = self.lobbies.clone();
        let tray_row = self.tray.then(|| {
            row(
                CLOSE_TO_TRAY,
                Switch::new("close-to-tray")
                    .checked(close_to_tray)
                    .accessibility_label(CLOSE_TO_TRAY)
                    .on_click(move |on: &bool, _, cx| {
                        let on = *on;
                        lobbies.update(cx, |l, cx| l.update_settings(|s| s.close_to_tray = on, cx));
                    }),
                cx,
            )
        });
        section(
            "Window",
            div()
                .flex()
                .flex_col()
                .gap(u(ROW_GAP))
                .child(row(
                    "Size",
                    size_stepper(ui_scale, self.lobbies.clone(), cx),
                    cx,
                ))
                .children(tray_row),
            cx,
        )
    }
}

/// The column the header and the sections share, so their left edges line up.
fn column() -> gpui_kit::Div {
    div().w_full().max_w(u(COLUMN_MAX_W)).mx_auto().px_4()
}

/// A named run of settings: its heading, then what it holds.
pub(crate) fn section(
    title: &'static str,
    body: impl IntoElement,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(u(HEADING_GAP))
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(title),
        )
        .child(body)
}

/// A group of rows under its small heading, with a smaller gap between the rows.
pub(crate) fn group(
    label: &'static str,
    rows: impl IntoIterator<Item = gpui_kit::AnyElement>,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .gap(u(LABEL_GAP))
        .child(section_label(label, cx))
        .child(div().flex().flex_col().gap(u(ROW_GAP)).children(rows))
}

/// The UI scale as a percent between a Smaller and a Bigger button, each disabled at its end.
fn size_stepper(percent: u16, lobbies: Entity<Lobbies>, cx: &gpui_kit::App) -> impl IntoElement {
    let button = |id: &'static str, label: &'static str, icon: IconName, bigger: bool| {
        let lobbies = lobbies.clone();
        let at_end = if bigger {
            percent >= UI_SCALE_MAX
        } else {
            percent <= UI_SCALE_MIN
        };
        page_icon_button(id, icon, cx)
            .debug_selector(move || id.into())
            .accessibility_label(label)
            .outline()
            .tooltip(label)
            .disabled(at_end)
            .on_click(move |_, _, cx| {
                lobbies.update(cx, |l, cx| {
                    l.update_settings(|s| s.ui_scale = step_ui_scale(s.ui_scale, bigger), cx)
                });
            })
    };
    div()
        .flex()
        .items_center()
        .gap(u(READOUT_GAP))
        .child(button(
            "size-smaller",
            "Smaller (Ctrl+-)",
            IconName::Minus,
            false,
        ))
        .child(
            div()
                .id("size-percent")
                .w(u(PERCENT_W))
                .text_center()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(format!("{percent}%"))
                .test_support(),
        )
        .child(button(
            "size-larger",
            "Bigger (Ctrl++)",
            IconName::Plus,
            true,
        ))
}

/// One setting: a line that says what it does, and its control on the right. A control that does
/// not fit beside the line stands under it.
pub(crate) fn row(
    label: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    div()
        .w_full()
        .min_h(u(ROW_H))
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(u(ROW_SIDE_GAP))
        .gap_y(u(ROW_WRAP_GAP))
        .child(
            div()
                .flex_auto()
                .min_w_0()
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(label.into()),
        )
        .child(div().flex_shrink_0().child(control))
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.voice.is_none() {
            let (lobbies, devices, session) =
                (self.lobbies.clone(), self.devices, self.session.clone());
            self.voice =
                Some(cx.new(|cx| VoiceSettingsView::new(lobbies, devices, session, window, cx)));
        }
        let background = cx.theme().background;
        let window_section = self.window_section(cx);
        let updates_section = self.updates_section(cx);
        let focus = self.focus.clone();
        div()
            .id("settings")
            .debug_selector(|| "settings".into())
            .track_focus(&self.focus)
            .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
                window.focus(&focus, cx)
            })
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.on_key(ev, cx)))
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(
                div().h(u(HEADER_H)).flex_shrink_0().child(
                    column().h_full().flex().items_center().child(
                        div().ml(u(-BACK_INSET)).flex_shrink_0().child(
                            page_icon_button("settings-back", IconName::ArrowLeft, cx)
                                .debug_selector(|| "settings-back".into())
                                .accessibility_label("Back")
                                .ghost()
                                .tooltip("Back")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                        ),
                    ),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("settings-body")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(
                                column()
                                    .pb_4()
                                    .flex()
                                    .flex_col()
                                    .gap(u(SECTION_GAP))
                                    .children(self.voice.clone())
                                    .child(window_section)
                                    .child(updates_section),
                            )
                            .test_support(),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .child(Scrollbar::new(&self.scroll).axis(ScrollbarAxis::Vertical)),
                    ),
            )
            .test_support()
    }
}

//! The Settings page: an in-window route that takes the place of the body under the title bar, so
//! it fits the small window. A header with the way back, then the Voice and Window sections in a
//! column that scrolls. Everything here is for this computer, none of it for a Lobby. A change is
//! written through `Lobbies` at once and the Shell applies what it shows.

use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        switch::Switch,
    },
    div,
};
use lobby_client::{UI_SCALE_MAX, UI_SCALE_MIN, step_ui_scale};

use crate::scale::u;
use crate::{
    components::ICON_MD, lobbies::Lobbies, lobby_view::VoiceDevices, session_entity::SessionEntity,
    voice_settings::VoiceSettingsView,
};

/// The rows keep to a readable column on a wide window.
const COLUMN_MAX_W: f32 = 520.;
const HEADER_H: f32 = 44.;
/// The Back button's own padding, which the header takes back so its arrow, not its hover box,
/// lines up with the section labels.
const BACK_INSET: f32 = 7.;
/// Sections stand well apart, and each heading sits close to what it names.
const SECTION_GAP: f32 = 28.;
const HEADING_GAP: f32 = 10.;
const ROW_PAD_Y: f32 = 10.;

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
        Self {
            lobbies,
            tray,
            devices,
            session,
            voice: None,
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

    /// Size, and Close to tray where the window has a tray to keep running in.
    fn window_section(&self, cx: &Context<Self>) -> impl IntoElement {
        let settings = self.lobbies.read(cx).settings();
        let (ui_scale, close_to_tray) = (settings.ui_scale, settings.close_to_tray);
        let lobbies = self.lobbies.clone();
        let tray_row = self.tray.then(|| {
            row(
                "Close to tray",
                Some("The close button keeps Lobby running in the tray."),
                Switch::new("close-to-tray")
                    .checked(close_to_tray)
                    .accessibility_label("Close to tray")
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
                .child(row(
                    "Size",
                    None,
                    size_stepper(ui_scale, self.lobbies.clone()),
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

/// A named run of settings.
pub(crate) fn section(
    title: &'static str,
    body: impl IntoElement,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .pb(u(HEADING_GAP))
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(title),
        )
        .child(body)
}

/// The UI scale as a percent between a Smaller and a Bigger button, each disabled at its end.
fn size_stepper(percent: u16, lobbies: Entity<Lobbies>) -> impl IntoElement {
    let button = |id: &'static str, label: &'static str, icon: IconName, bigger: bool| {
        let lobbies = lobbies.clone();
        let at_end = if bigger {
            percent >= UI_SCALE_MAX
        } else {
            percent <= UI_SCALE_MIN
        };
        Button::new(id)
            .debug_selector(move || id.into())
            .accessibility_label(label)
            .ghost()
            .small()
            .icon(Icon::new(icon))
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
        .gap_1()
        .child(button(
            "size-smaller",
            "Smaller (Ctrl -)",
            IconName::Minus,
            false,
        ))
        .child(
            div()
                .id("size-percent")
                .w(u(48.))
                .text_center()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(format!("{percent}%"))
                .test_support(),
        )
        .child(button(
            "size-larger",
            "Bigger (Ctrl +)",
            IconName::Plus,
            true,
        ))
}

/// One setting: its name and, if it needs one, a line on what it does on the left, its control
/// on the right. A rule above each row makes a section's rows read as one list.
pub(crate) fn row(
    title: &'static str,
    detail: Option<&'static str>,
    control: impl IntoElement,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_4()
        .py(u(ROW_PAD_Y))
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(SharedString::from(title)),
                )
                .children(detail.map(|detail| {
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(detail))
                })),
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
                            Button::new("settings-back")
                                .debug_selector(|| "settings-back".into())
                                .accessibility_label("Back")
                                .ghost()
                                .small()
                                .icon(Icon::new(IconName::ArrowLeft).size(u(ICON_MD)))
                                .tooltip("Back")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                        ),
                    ),
                ),
            )
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        column()
                            .pb_4()
                            .flex()
                            .flex_col()
                            .gap(u(SECTION_GAP))
                            .children(self.voice.clone())
                            .child(window_section),
                    )
                    .test_support(),
            )
            .test_support()
    }
}

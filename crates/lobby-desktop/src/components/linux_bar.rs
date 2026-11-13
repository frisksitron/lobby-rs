//! The title bar as a Linux desktop draws one, in place of the kit's, whose Windows-style buttons
//! cannot be turned off. The window's buttons are round and sit where the desktop's layout puts
//! them. The bar moves the window, toggles maximize on a double click and opens the desktop's
//! window menu on a right click, as the kit's does; every interactive child occludes.

use std::{rc::Rc, sync::OnceLock};

use gpui_kit::{
    AnyElement, App, Background, ClickEvent, Decorations, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement, Refineable as _, RenderOnce, Rgba,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Subscription, TestSupportExt as _,
    Window, WindowButton, WindowButtonLayout, assets::IconName, base::InteractiveElementExt as _,
    div, linear_color_stop, linear_gradient, prelude::FluentBuilder as _,
};

use crate::kit::{ActiveTheme as _, window_corner_radii};
use crate::scale::u;
use crate::ui;

const CONTROL: f32 = 24.;
const CONTROL_ICON: f32 = 16.;
const CONTROL_GAP: f32 = 6.;
const CONTROL_EDGE: f32 = 8.;
/// What the bar keeps free left of its content when no button is there, as the kit's bar does.
const CONTENT_INSET: f32 = 12.;

const REST: f32 = 0.09;
const HOVER: f32 = 0.18;
const PRESSED: f32 = 0.26;
/// How much of itself the bar keeps while its window is not the active one.
const BACKDROP: f32 = 0.55;

type CloseHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(gpui_kit::IntoElement)]
pub struct LinuxBar {
    style: StyleRefinement,
    children: Vec<AnyElement>,
    on_close_window: Option<CloseHandler>,
}

impl LinuxBar {
    pub fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            children: Vec::new(),
            on_close_window: None,
        }
    }

    pub fn on_close_window(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close_window = Some(Rc::new(f));
        self
    }
}

impl Styled for LinuxBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for LinuxBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// Between the bar's own fill and the window's, lighter at the top, as the kit's bar is.
fn fill(title_bar: Hsla, background: Hsla) -> Background {
    let (bar, back) = (title_bar.to_rgb(), background.to_rgb());
    let mixed = Hsla::from(Rgba {
        r: bar.r * 0.55 + back.r * 0.45,
        g: bar.g * 0.55 + back.g * 0.45,
        b: bar.b * 0.55 + back.b * 0.45,
        a: bar.a * 0.55 + back.a * 0.45,
    });
    linear_gradient(
        180.,
        linear_color_stop(mixed, 0.),
        linear_color_stop(title_bar, 1.),
    )
}

struct Drag {
    armed: bool,
}

/// The buttons the desktop's layout names. The portal's `button-layout` is GNOME's own setting:
/// beside KDE's portal a GTK one is often installed, and it answers with the schema's default,
/// `appmenu:close`, which would leave a Plasma window with no minimize and no maximize. Only a
/// GNOME desktop is asked; every other draws GPUI's default layout.
fn button_layout(cx: &App) -> WindowButtonLayout {
    static GNOME: OnceLock<bool> = OnceLock::new();
    let gnome = *GNOME.get_or_init(|| {
        std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| {
            desktops
                .split(':')
                .any(|desktop| desktop.eq_ignore_ascii_case("GNOME"))
        })
    });
    match cx.button_layout() {
        Some(layout) if gnome => layout,
        _ => WindowButtonLayout::linux_default(),
    }
}

/// Keeps the window redrawn when the desktop's button layout changes under a running Lobby.
struct LayoutWatch(#[allow(dead_code)] Subscription);

impl RenderOnce for LinuxBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Nothing to close, minimize or maximize in a fullscreen window: the desktop's way out of
        // it is its own, as in any GNOME or KDE app.
        let client = matches!(window.window_decorations(), Decorations::Client { .. });
        let controls = client && !window.is_fullscreen();
        let active = window.is_window_active();
        let drag = window.use_state(cx, |_, _| Drag { armed: false });
        window.use_state(cx, |window, _| {
            LayoutWatch(window.observe_button_layout_changed(|window, _| window.refresh()))
        });
        let radii = window_corner_radii(window, cx);
        let theme = cx.theme();
        let (fill, border) = (
            fill(theme.title_bar, theme.background),
            theme.title_bar_border,
        );

        let (left, right) = if controls {
            let layout = button_layout(cx);
            let offered = window.window_controls();
            let side = |buttons: [Option<WindowButton>; 3]| {
                buttons
                    .into_iter()
                    .flatten()
                    .filter(|button| match button {
                        WindowButton::Minimize => offered.minimize,
                        WindowButton::Maximize => offered.maximize,
                        WindowButton::Close => true,
                    })
                    .map(|button| Control::new(button, self.on_close_window.clone()))
                    .collect::<Vec<_>>()
            };
            (side(layout.left), side(layout.right))
        } else {
            (Vec::new(), Vec::new())
        };
        let menu = client && window.window_controls().window_menu;
        let has_left = !left.is_empty();
        let has_right = !right.is_empty();
        let strip = |controls: Vec<Control>| {
            div()
                .flex()
                .items_center()
                .flex_shrink_0()
                .gap(u(CONTROL_GAP))
                .children(controls)
        };

        let mut bar = div()
            .id("title-bar")
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(border)
            .bg(fill)
            .rounded_tl(radii.top_left)
            .rounded_tr(radii.top_right)
            .when(has_left, |bar| bar.pl(u(CONTROL_EDGE)))
            .when(!has_left, |bar| bar.pl(u(CONTENT_INSET)))
            .when(has_right, |bar| bar.pr(u(CONTROL_EDGE)));
        bar.style().refine(&self.style);
        bar.on_double_click(|_, window, _| window.zoom_window())
            .test_support()
            .on_mouse_down_out(window.listener_for(&drag, |drag, _, _, _| drag.armed = false))
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&drag, |drag, _, _, _| drag.armed = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&drag, |drag, _, _, _| drag.armed = false),
            )
            .on_mouse_move(window.listener_for(&drag, |drag, _, window, _| {
                if std::mem::take(&mut drag.armed) {
                    window.start_window_move();
                }
            }))
            .when(menu, |bar| {
                bar.on_mouse_down(MouseButton::Right, |event, window, _| {
                    window.show_window_menu(event.position)
                })
            })
            .when(has_left, |bar| bar.child(strip(left).mr(u(CONTROL_GAP))))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(!active, |content| content.opacity(BACKDROP))
                    .children(self.children),
            )
            .when(has_right, |bar| bar.child(strip(right).ml(u(CONTROL_GAP))))
    }
}

#[derive(IntoElement)]
struct Control {
    button: WindowButton,
    on_close_window: Option<CloseHandler>,
}

impl Control {
    fn new(button: WindowButton, on_close_window: Option<CloseHandler>) -> Self {
        Self {
            button,
            on_close_window,
        }
    }
}

impl RenderOnce for Control {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (id, icon) = match self.button {
            WindowButton::Minimize => ("minimize", IconName::WindowMinimize),
            WindowButton::Maximize if window.is_maximized() => ("restore", IconName::WindowRestore),
            WindowButton::Maximize => ("maximize", IconName::WindowMaximize),
            WindowButton::Close => ("close", IconName::WindowClose),
        };
        let ink = cx.theme().foreground;
        let glyph = if window.is_window_active() {
            ink
        } else {
            ink.opacity(BACKDROP)
        };
        let (button, on_close_window) = (self.button, self.on_close_window);
        div()
            .id(id)
            .test_support()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .size(u(CONTROL))
            .rounded_full()
            .bg(ink.opacity(REST))
            .hover(|style| style.bg(ink.opacity(HOVER)))
            .active(|style| style.bg(ink.opacity(PRESSED)))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match button {
                    WindowButton::Minimize => window.minimize_window(),
                    WindowButton::Maximize => window.zoom_window(),
                    WindowButton::Close => match &on_close_window {
                        Some(f) => f(&ClickEvent::default(), window, cx),
                        None => window.remove_window(),
                    },
                }
            })
            .child(ui::Icon::new(icon).size(u(CONTROL_ICON)).text_color(glyph))
    }
}

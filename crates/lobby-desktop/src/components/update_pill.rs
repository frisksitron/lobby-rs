//! The title bar's update control. Downloading, the pill is its own progress bar; downloaded, it
//! becomes the primary Restart to update; failed, it offers another try. There is no close
//! button: it goes away once the app runs the update.

use std::rc::Rc;

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    component::{ActiveTheme as _, Icon, button::ButtonVariants as _, tooltip::Tooltip},
    div,
    prelude::FluentBuilder as _,
    relative,
};
use lobby_client::{copy, update::UpdateStatus};

use super::{BAR_CONTROL_H, BarButton, ICON_SM, small_word_button};
use crate::scale::u;

/// Room for "100%", so the pill keeps its width as the number grows.
const PERCENT_W: f32 = 30.;

#[derive(IntoElement)]
pub struct UpdatePill {
    status: UpdateStatus,
    wide: bool,
    on_restart: Rc<dyn Fn(&mut App)>,
    on_retry: Rc<dyn Fn(&mut App)>,
}

impl UpdatePill {
    pub fn new(
        status: UpdateStatus,
        wide: bool,
        on_restart: Rc<dyn Fn(&mut App)>,
        on_retry: Rc<dyn Fn(&mut App)>,
    ) -> Self {
        Self {
            status,
            wide,
            on_restart,
            on_retry,
        }
    }
}

impl RenderOnce for UpdatePill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        match self.status {
            UpdateStatus::Current(_) => div().into_any_element(),
            UpdateStatus::Downloading {
                version,
                received,
                total,
            } => {
                let done = if total == 0 {
                    0.
                } else {
                    received as f32 / total as f32
                };
                let tip: SharedString =
                    copy::update_downloading(&version.to_string(), received, total).into();
                div()
                    .id("update-progress")
                    .debug_selector(|| "update-progress".into())
                    .occlude()
                    .relative()
                    .overflow_hidden()
                    .h(u(BAR_CONTROL_H))
                    .px_2()
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(relative(done.clamp(0., 1.)))
                            .bg(theme.foreground.opacity(0.08)),
                    )
                    .child(Icon::new(IconName::ArrowDown).size(u(ICON_SM)))
                    .when(self.wide, |el| el.child(copy::UPDATING_LABEL))
                    .child(
                        div()
                            .w(u(PERCENT_W))
                            .text_right()
                            .text_color(theme.foreground)
                            .child(format!("{:.0}%", (done * 100.).floor())),
                    )
                    .test_support()
                    .into_any_element()
            }
            UpdateStatus::Ready { version, .. } => {
                let on_restart = self.on_restart;
                let label = if self.wide {
                    copy::UPDATE_RESTART
                } else {
                    copy::UPDATE_RESTART_SHORT
                };
                let button = small_word_button("update-restart")
                    .debug_selector(|| "update-restart".into())
                    .primary()
                    .label(label)
                    .tooltip(copy::update_ready(&version.to_string()))
                    .on_click(move |_, _, cx| on_restart(cx));
                BarButton::slot(button, None).into_any_element()
            }
            UpdateStatus::Failed { version, .. } => {
                let on_retry = self.on_retry;
                let button = BarButton::button("update-retry")
                    .debug_selector(|| "update-retry".into())
                    .icon(Icon::new(IconName::CircleAlert))
                    .when(self.wide, |b| b.label(copy::UPDATE_FAILED))
                    .tooltip(copy::update_failed(&version.to_string()))
                    .on_click(move |_, _, cx| on_retry(cx));
                BarButton::slot(button, None).into_any_element()
            }
        }
    }
}

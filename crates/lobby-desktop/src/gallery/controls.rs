use gpui_kit::{
    Axis, Context, InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _,
    Window,
    assets::IconName,
    base::{Popover, Toolbar, v_flex},
    div,
    prelude::FluentBuilder as _,
    px, relative,
};

use super::{
    Gallery, Section,
    stories::{caption, wrap},
};
use crate::components::{BarButton, SURFACE_INNER_RADIUS, primary_button};
use crate::kit::ActiveTheme as _;
use crate::scale::{device_in, u};
use crate::ui::{self, Icon};

fn page(id: &'static str) -> ui::Button {
    ui::Button::new(id).size(ui::Size::Page)
}

pub(super) fn controls(
    g: &mut Gallery,
    window: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let variants = [
        ("default", page("k-default").label("Default")),
        (
            "primary",
            page("k-primary")
                .variant(ui::Variant::Primary)
                .label("Join Voice"),
        ),
        (
            "secondary",
            page("k-secondary")
                .variant(ui::Variant::Secondary)
                .label("Cancel"),
        ),
        (
            "outline",
            page("k-outline")
                .variant(ui::Variant::Outline)
                .label("Add Lobby"),
        ),
        (
            "ghost",
            page("k-ghost")
                .variant(ui::Variant::Ghost)
                .label("Clear all"),
        ),
        (
            "danger",
            page("k-danger").variant(ui::Variant::Danger).label("Leave"),
        ),
        ("icon", page("k-icon").icon(IconName::Settings)),
        (
            "loading",
            page("k-loading")
                .variant(ui::Variant::Primary)
                .loading(true)
                .label("Sending"),
        ),
        (
            "disabled",
            page("k-disabled")
                .variant(ui::Variant::Primary)
                .disabled(true)
                .label("Send"),
        ),
    ];
    let states = [
        (
            "ghost selected",
            page("k-ghost-selected")
                .icon(IconName::Users)
                .label("Members")
                .selected(true),
        ),
        (
            "icon selected",
            page("k-icon-selected")
                .icon(IconName::Settings)
                .selected(true),
        ),
        (
            "outline selected",
            page("k-outline-selected")
                .variant(ui::Variant::Outline)
                .label("Add Lobby")
                .selected(true),
        ),
        (
            "ghost loading",
            page("k-ghost-loading").loading(true).label("Checking"),
        ),
        (
            "outline loading, disabled",
            page("k-outline-loading")
                .variant(ui::Variant::Outline)
                .loading(true)
                .disabled(true)
                .label("Check now"),
        ),
        (
            "primary loading with icon",
            page("k-primary-loading")
                .variant(ui::Variant::Primary)
                .icon(IconName::Download)
                .loading(true)
                .label("Save"),
        ),
        (
            "ghost disabled",
            page("k-ghost-disabled").disabled(true).icon(IconName::Bell),
        ),
        (
            "outline disabled",
            page("k-outline-disabled")
                .variant(ui::Variant::Outline)
                .disabled(true)
                .label("Open"),
        ),
        (
            "danger disabled",
            page("k-danger-disabled")
                .variant(ui::Variant::Danger)
                .disabled(true)
                .label("Leave"),
        ),
    ];
    let theme = cx.theme();
    let (danger, danger_ink) = (theme.danger, theme.danger_foreground);
    let (danger_hover, muted, foreground) =
        (theme.danger_hover, theme.muted_foreground, theme.foreground);
    let coral = ui::Custom::new(danger, danger_ink)
        .hover(danger_hover)
        .active(danger);
    let wash = ui::Custom::new(gpui_kit::transparent_black(), foreground)
        .hover(foreground.opacity(0.06))
        .active(foreground.opacity(0.09));
    let customs = [
        (
            "rest",
            page("k-custom")
                .variant(ui::Variant::Custom(coral))
                .icon(IconName::MicOff),
        ),
        (
            "selected",
            page("k-custom-selected")
                .variant(ui::Variant::Custom(coral))
                .icon(IconName::MicOff)
                .selected(true),
        ),
        (
            "disabled",
            page("k-custom-disabled")
                .variant(ui::Variant::Custom(coral))
                .icon(IconName::MicOff)
                .disabled(true),
        ),
        (
            "wash",
            page("k-wash")
                .variant(ui::Variant::Custom(wash))
                .label("A Member's row"),
        ),
        (
            "wash open",
            page("k-wash-open")
                .variant(ui::Variant::Custom(wash))
                .label("A Member's row")
                .selected(true),
        ),
    ];
    let lobby_sizes = [
        (
            "bar icon",
            ui::Button::new("k-bar-icon")
                .size(ui::Size::Bar)
                .icon(IconName::Settings)
                .tooltip("Settings")
                .into_any_element(),
        ),
        (
            "bar",
            ui::Button::new("k-bar")
                .size(ui::Size::Bar)
                .icon(IconName::Users)
                .label("Members")
                .into_any_element(),
        ),
        (
            "word",
            ui::Button::new("k-word")
                .variant(ui::Variant::Secondary)
                .size(ui::Size::Word)
                .label("Vouch")
                .into_any_element(),
        ),
        (
            "inline",
            ui::Button::new("k-inline")
                .size(ui::Size::Inline)
                .icon(IconName::Type)
                .into_any_element(),
        ),
        (
            "page",
            ui::Button::new("k-page")
                .size(ui::Size::Page)
                .label("Cancel")
                .into_any_element(),
        ),
        (
            "page danger",
            ui::Button::new("k-page-danger")
                .variant(ui::Variant::Danger)
                .size(ui::Size::Page)
                .label("Leave")
                .into_any_element(),
        ),
        (
            "step",
            ui::Button::new("k-step")
                .variant(ui::Variant::Primary)
                .size(ui::Size::Step)
                .rounded(device_in(SURFACE_INNER_RADIUS, cx))
                .label("Send code")
                .into_any_element(),
        ),
        (
            "step busy",
            primary_button("k-step-busy", "Send code", "Sending code", true, cx).into_any_element(),
        ),
        (
            "settings",
            BarButton::settings(false, |_, _| {}).into_any_element(),
        ),
        (
            "settings open",
            BarButton::settings(true, |_, _| {}).into_any_element(),
        ),
    ];
    let volume = g.held.volumes[0].clone();
    let value = volume.read(cx).value().start();
    let dnd = |id: &'static str, on: bool, disabled: bool| {
        div()
            .flex()
            .items_start()
            .gap_2()
            .when(disabled, |d| d.text_color(muted))
            .child(
                ui::Switch::new(id)
                    .checked(on)
                    .disabled(disabled)
                    .accessibility_label("Do Not Disturb"),
            )
            .child(div().text_sm().line_height(u(20.)).child("Do Not Disturb"))
    };
    let format =
        |id: &'static str, icon: IconName| ui::Button::new(id).size(ui::Size::Bar).icon(icon);
    vec![
        Section::new(
            "button-variants",
            "Button variants",
            wrap(variants.map(|(n, b)| caption(n, b, cx))),
        ),
        Section::new(
            "button-states",
            "Button states",
            wrap(states.map(|(n, b)| caption(n, b, cx))),
        ),
        Section::new(
            "button-custom",
            "Custom variant",
            wrap(customs.map(|(n, b)| caption(n, b, cx))),
        ),
        Section::new(
            "lobby-sizes",
            "Lobby's control sizes",
            wrap(lobby_sizes.map(|(n, b)| caption(n, b, cx))),
        ),
        Section::new(
            "badge",
            "Badge (a dot on a bar button)",
            wrap([caption(
                "dot",
                BarButton::slot(
                    ui::Button::new("kb-dot")
                        .size(ui::Size::Bar)
                        .icon(IconName::Users),
                    Some(danger),
                ),
                cx,
            )]),
        ),
        Section::new(
            "tooltip",
            "Tooltip (hover)",
            wrap([caption(
                "hover me",
                page("k-tip")
                    .variant(ui::Variant::Outline)
                    .label("Mute")
                    .tooltip("Mute your microphone (Ctrl+Shift+M)"),
                cx,
            )]),
        ),
        Section::new(
            "popover",
            "Popover (click)",
            wrap([caption(
                "click me",
                Popover::new("k-popover")
                    .offset(u(4.).to_pixels(window.rem_size()))
                    .trigger(
                        page("k-popover-trigger")
                            .variant(ui::Variant::Outline)
                            .label("Connected"),
                    )
                    .content(|_, _, cx| {
                        ui::popover_surface(v_flex().id("content").occlude(), cx)
                            .p_3()
                            .child(
                                div()
                                    .w(px(220.))
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child(
                                        "A popover holds a few lines and the one action that helps.",
                                    ),
                            )
                    }),
                cx,
            )]),
        ),
        Section::new(
            "switch",
            "Switch",
            wrap([
                caption("off", dnd("k-switch-off", false, false), cx),
                caption("on", dnd("k-switch-on", true, false), cx),
                caption("disabled", dnd("k-switch-disabled", false, true), cx),
                caption(
                    "small",
                    ui::Switch::new("k-switch-small")
                        .small()
                        .checked(true)
                        .accessibility_label("Let others draw"),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "skeleton",
            "Skeleton",
            div()
                .w(px(320.))
                .flex()
                .gap_2()
                .child(ui::Skeleton::new().size(px(30.)).rounded_full())
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(ui::Skeleton::new().h(px(10.)).w(px(96.)))
                        .child(ui::Skeleton::new().h(px(12.)).w(relative(0.8))),
                ),
        ),
        Section::new(
            "separator",
            "Separator",
            div()
                .w(px(420.))
                .flex()
                .flex_col()
                .gap_3()
                .child(ui::Separator::horizontal())
                .child(ui::Separator::horizontal().label("Today"))
                .child(
                    div()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child("Bold")
                        .child(ui::Separator::vertical())
                        .child("Code"),
                ),
        ),
        Section::new(
            "empty",
            "Empty",
            div().w(px(420.)).child(
                ui::Empty::new().header(
                    ui::EmptyHeader::new()
                        .media(ui::EmptyMedia::new().child(Icon::new(IconName::Inbox)))
                        .title(ui::EmptyTitle::new().child("Nothing here yet"))
                        .description(
                            ui::EmptyDescription::new().child("What arrives shows up here."),
                        ),
                ),
            ),
        ),
        Section::new(
            "toolbar",
            "Toolbar (arrow keys move along it)",
            Toolbar::new("k-toolbar")
                .axis(Axis::Horizontal)
                .flex()
                .items_center()
                .gap_1()
                .child(format("k-tb-bold", IconName::Bold))
                .child(format("k-tb-italic", IconName::Italic))
                .child(format("k-tb-code", IconName::Code)),
        ),
        Section::new(
            "slider",
            "Slider (a Member's volume in their card)",
            div()
                .w(px(280.))
                .flex()
                .flex_col()
                .gap_2()
                .child(ui::Slider::new(&volume).accessibility_label("Volume"))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{value:.0} %")),
                ),
        ),
    ]
}

use gpui_kit::{
    Context, ParentElement as _, Styled as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _, Icon, Sizable as _,
        badge::Badge,
        button::{Button, ButtonVariants as _},
        empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle},
        popover::Popover,
        separator::Separator,
        skeleton::Skeleton,
        slider::Slider,
        switch::Switch,
        toolbar::Toolbar,
    },
    div, px, relative,
};

use super::{
    Gallery, Section,
    stories::{caption, wrap},
};

pub(super) fn kit(g: &mut Gallery, _: &mut Window, cx: &mut Context<Gallery>) -> Vec<Section> {
    let variants = [
        ("default", Button::new("k-default").label("Default")),
        (
            "primary",
            Button::new("k-primary").primary().label("Join Voice"),
        ),
        (
            "secondary",
            Button::new("k-secondary").secondary().label("Cancel"),
        ),
        (
            "outline",
            Button::new("k-outline").outline().label("Add Lobby"),
        ),
        ("ghost", Button::new("k-ghost").ghost().label("Clear all")),
        ("danger", Button::new("k-danger").danger().label("Leave")),
        (
            "link",
            Button::new("k-link").link().label("Send a new code"),
        ),
        (
            "icon",
            Button::new("k-icon")
                .ghost()
                .icon(Icon::new(IconName::Settings)),
        ),
        (
            "loading",
            Button::new("k-loading")
                .primary()
                .loading(true)
                .label("Sending"),
        ),
        (
            "disabled",
            Button::new("k-disabled")
                .primary()
                .disabled(true)
                .label("Send"),
        ),
    ];
    let sizes = [
        (
            "xsmall",
            Button::new("k-xs").outline().xsmall().label("Add Lobby"),
        ),
        (
            "small",
            Button::new("k-sm").outline().small().label("Add Lobby"),
        ),
        ("medium", Button::new("k-md").outline().label("Add Lobby")),
        (
            "large",
            Button::new("k-lg").outline().large().label("Add Lobby"),
        ),
    ];
    let theme = cx.theme();
    let (danger, muted) = (theme.danger, theme.muted_foreground);
    let volume = g.held.volumes[0].clone();
    let value = volume.read(cx).value().start();
    vec![
        Section::new(
            "button-variants",
            "Button variants",
            wrap(variants.map(|(n, b)| caption(n, b, cx))),
        ),
        Section::new(
            "button-sizes",
            "Button sizes",
            wrap(sizes.map(|(n, b)| caption(n, b, cx))),
        ),
        Section::new(
            "badge",
            "Badge",
            wrap([
                caption(
                    "dot",
                    Badge::new().dot().child(
                        Button::new("kb-dot")
                            .ghost()
                            .icon(Icon::new(IconName::Users)),
                    ),
                    cx,
                ),
                caption(
                    "count",
                    Badge::new()
                        .count(3)
                        .color(danger)
                        .child(Button::new("kb-3").ghost().icon(Icon::new(IconName::Bell))),
                    cx,
                ),
                caption(
                    "9+",
                    Badge::new()
                        .count(42)
                        .max(9)
                        .child(Button::new("kb-42").ghost().icon(Icon::new(IconName::Bell))),
                    cx,
                ),
            ]),
        ),
        Section::new(
            "tooltip",
            "Tooltip (hover)",
            wrap([caption(
                "hover me",
                Button::new("k-tip")
                    .outline()
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
                    .trigger(
                        Button::new("k-popover-trigger")
                            .outline()
                            .label("Connected"),
                    )
                    .content(|_, _, cx| {
                        div()
                            .w(px(220.))
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child("A popover holds a few lines and the one action that helps.")
                    }),
                cx,
            )]),
        ),
        Section::new(
            "switch",
            "Switch",
            wrap([
                caption(
                    "off",
                    Switch::new("k-switch-off").label("Do Not Disturb"),
                    cx,
                ),
                caption(
                    "on",
                    Switch::new("k-switch-on")
                        .checked(true)
                        .label("Do Not Disturb"),
                    cx,
                ),
                caption(
                    "disabled",
                    Switch::new("k-switch-disabled")
                        .disabled(true)
                        .label("Do Not Disturb"),
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
                .child(Skeleton::new().size(px(30.)).rounded_full())
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(Skeleton::new().h(px(10.)).w(px(96.)))
                        .child(Skeleton::new().h(px(12.)).w(relative(0.8))),
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
                .child(Separator::horizontal())
                .child(Separator::horizontal().label("Today"))
                .child(
                    div()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child("Bold")
                        .child(Separator::vertical())
                        .child("Code"),
                ),
        ),
        Section::new(
            "empty",
            "Empty",
            div().w(px(420.)).child(
                Empty::new().header(
                    EmptyHeader::new()
                        .media(EmptyMedia::new().child(Icon::new(IconName::Inbox)))
                        .title(EmptyTitle::new().child("Nothing here yet"))
                        .description(EmptyDescription::new().child("What arrives shows up here.")),
                ),
            ),
        ),
        Section::new(
            "toolbar",
            "Toolbar",
            Toolbar::new("k-toolbar")
                .child(
                    Button::new("k-tb-bold")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Bold)),
                )
                .child(
                    Button::new("k-tb-italic")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Italic)),
                )
                .child(
                    Button::new("k-tb-code")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Code)),
                ),
        ),
        Section::new(
            "slider",
            "Slider (a Member's volume in their card)",
            div()
                .w(px(280.))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    Slider::new(&volume)
                        .horizontal()
                        .accessibility_label("Volume"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{value:.0} %")),
                ),
        ),
    ]
}

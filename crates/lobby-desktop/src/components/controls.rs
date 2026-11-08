//! Fields, selects and buttons at the heights of the surface they stand on (see
//! `components/tokens.rs`), so a page never sizes a control by hand. The onboarding screens'
//! pair is [`FIELD_H`] tall; a page's and a dialog's is [`PAGE_CONTROL_H`].

use gpui_kit::{
    App, ElementId, Entity, FontWeight, ParentElement as _, Styled as _, Window,
    assets::IconName,
    component::{
        Icon, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::{AlertDialog, DialogFooter},
        input::{Input, InputState},
        searchable_list::{SearchableListDelegate, SearchableListItem},
        select::{Select, SelectState},
    },
};

use super::{FIELD_H, ICON_MD, PAGE_CONTROL_H, PAGE_CONTROL_RADIUS, SURFACE_INNER_RADIUS};
use crate::scale::{device_in, u};

const STEP_PAD_X: f32 = 16.;
const PAGE_PAD_X: f32 = 14.;

/// The onboarding step's one field: the kit's large size is [`FIELD_H`] tall.
pub fn step_input(state: &Entity<InputState>, id: &'static str) -> Input {
    Input::new(state)
        .id(id)
        .large()
        .rounded(u(SURFACE_INNER_RADIUS))
}

/// A step's own button at the field's height, with the column's inner corners.
pub fn step_button(id: &'static str, cx: &App) -> Button {
    Button::new(id)
        .h(u(FIELD_H))
        .px(u(STEP_PAD_X))
        .rounded(device_in(SURFACE_INNER_RADIUS, cx))
        .font_weight(FontWeight::SEMIBOLD)
}

/// A page's or dialog's button, at the page control height.
pub fn page_button(id: impl Into<ElementId>, cx: &App) -> Button {
    Button::new(id)
        .h(u(PAGE_CONTROL_H))
        .px(u(PAGE_PAD_X))
        .rounded(device_in(PAGE_CONTROL_RADIUS, cx))
        .font_weight(FontWeight::SEMIBOLD)
}

/// A page's square icon button, such as Back or a stepper's minus.
pub fn page_icon_button(id: impl Into<ElementId>, icon: IconName, cx: &App) -> Button {
    page_button(id, cx)
        .w(u(PAGE_CONTROL_H))
        .px(u(0.))
        .icon(Icon::new(icon).size(u(ICON_MD)))
}

/// An alert that asks before something that cannot be undone: Cancel, then the action in the danger
/// colour, both at the page control height. The kit's own alert footer takes no size, so this one
/// brings its own buttons. A press runs its callback and then closes the dialog; Enter and Esc
/// run the same callbacks.
pub fn confirm_alert(
    alert: AlertDialog,
    cancel_label: &'static str,
    ok_label: &'static str,
    on_cancel: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_ok: impl Fn(&mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> AlertDialog {
    let (cancel_key, ok_key) = (on_cancel.clone(), on_ok.clone());
    let footer = DialogFooter::new()
        .child(
            page_button("confirm-cancel", cx)
                .ghost()
                .label(cancel_label)
                .on_click(move |_, window, cx| {
                    on_cancel(window, cx);
                    window.close_dialog(cx);
                }),
        )
        .child(
            page_button("confirm-ok", cx)
                .danger()
                .label(ok_label)
                .on_click(move |_, window, cx| {
                    on_ok(window, cx);
                    window.close_dialog(cx);
                }),
        );
    alert
        .confirm()
        .footer(footer)
        .on_cancel(move |_, window, cx| {
            cancel_key(window, cx);
            true
        })
        .on_ok(move |_, window, cx| {
            ok_key(window, cx);
            true
        })
}

/// A page's select at the page control height, at its row's full width unless the caller sets one.
pub fn page_select<D>(state: &Entity<SelectState<D>>, label: &'static str) -> Select<D>
where
    D: SearchableListDelegate + 'static,
    <D::Item as SearchableListItem>::Value: PartialEq + Clone,
{
    Select::new(state)
        .h(u(PAGE_CONTROL_H))
        .rounded(u(PAGE_CONTROL_RADIUS))
        .accessibility_label(label)
}

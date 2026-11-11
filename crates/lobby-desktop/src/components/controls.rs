//! Fields, selects and dialogs at the heights of the surface they stand on (see `ui::Size`), so a
//! page never sizes a control by hand. The onboarding screens' pair is
//! [`FIELD_H`](crate::ui::FIELD_H) tall; a page's and a dialog's is [`PAGE_CONTROL_H`].

use gpui_kit::{App, Entity, ParentElement as _, Styled as _, Window};

use super::{PAGE_CONTROL_H, PAGE_CONTROL_RADIUS, SURFACE_INNER_RADIUS};
use crate::kit::{
    Sizable as _, WindowExt as _,
    dialog::{AlertDialog, DialogFooter},
    input::{Input, InputState},
    searchable_list::{SearchableListDelegate, SearchableListItem},
    select::{Select, SelectState},
};
use crate::scale::u;
use crate::ui;

/// The onboarding step's one field: the kit's large size is [`FIELD_H`](crate::ui::FIELD_H) tall.
pub fn step_input(state: &Entity<InputState>, id: &'static str) -> Input {
    Input::new(state)
        .id(id)
        .large()
        .rounded(u(SURFACE_INNER_RADIUS))
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
) -> AlertDialog {
    let (cancel_key, ok_key) = (on_cancel.clone(), on_ok.clone());
    let footer = DialogFooter::new()
        .child(
            ui::Button::new("confirm-cancel")
                .size(ui::Size::Page)
                .label(cancel_label)
                .on_click(move |_, window, cx| {
                    on_cancel(window, cx);
                    window.close_dialog(cx);
                }),
        )
        .child(
            ui::Button::new("confirm-ok")
                .variant(ui::Variant::Danger)
                .size(ui::Size::Page)
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

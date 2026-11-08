//! One line of text that ends in an ellipsis when it doesn't fit, with the whole text as a tooltip
//! only while it is cut off. Style the returned element: both copies inherit its text style.

use std::{cell::Cell, rc::Rc};

use gpui_kit::{
    AppContext as _, Div, ElementId, EmptyView, InteractiveElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, component::tooltip::Tooltip, div,
    px,
};

pub fn truncated_text(id: impl Into<ElementId>, text: SharedString) -> Div {
    let cut = Rc::new(Cell::new(false));
    let seen = cut.clone();
    let whole = text.clone();
    div()
        .relative()
        .min_w_0()
        // An unconstrained copy, laid out but never seen, measures the text's natural width.
        .on_children_prepainted(move |bounds, _, _| {
            if let [shown, natural] = bounds[..] {
                seen.set(natural.size.width > shown.size.width + px(0.5)); // px: laid-out widths
            }
        })
        .child(
            div()
                .id(id)
                .min_w_0()
                .truncate()
                .child(text.clone())
                .tooltip(move |window, cx| {
                    if cut.get() {
                        Tooltip::new(whole.clone()).build(window, cx)
                    } else {
                        cx.new(|_| EmptyView).into()
                    }
                }),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .whitespace_nowrap()
                .opacity(0.)
                .child(text),
        )
}

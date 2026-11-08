//! A small button that carries a word, such as Vouch, Send again or Jump to latest: the weight and
//! the room around its words that Join Voice and Leave have, at the title bar's control height.

use gpui_kit::{
    ElementId, FontWeight, Styled as _,
    component::{Sizable as _, button::Button},
};

use super::BAR_CONTROL_H;
use crate::scale::u;

const PAD_X: f32 = 12.;

pub fn small_word_button(id: impl Into<ElementId>) -> Button {
    Button::new(id)
        .small()
        .h(u(BAR_CONTROL_H))
        .px(u(PAD_X))
        .font_weight(FontWeight::SEMIBOLD)
}

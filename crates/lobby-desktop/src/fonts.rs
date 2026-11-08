//! Figtree, embedded: the family of every piece of text, in the four weights the UI uses, so a
//! semibold label is the font's own SemiBold and never Regular made heavier, and every machine
//! draws the same letters.

use std::borrow::Cow;

use gpui_kit::App;

/// The family name every face has.
pub const FAMILY: &str = "Figtree";

/// Regular, Medium, SemiBold and Bold, in that order.
pub const FACES: [&[u8]; 4] = [
    include_bytes!("../fonts/Figtree-Regular.ttf"),
    include_bytes!("../fonts/Figtree-Medium.ttf"),
    include_bytes!("../fonts/Figtree-SemiBold.ttf"),
    include_bytes!("../fonts/Figtree-Bold.ttf"),
];

/// Gives the text system the four faces. Once per app, by its composition root; the render tests
/// build a text system with the faces they want instead.
pub fn install(cx: &mut App) {
    cx.text_system()
        .add_fonts(FACES.iter().map(|face| Cow::Borrowed(*face)).collect())
        .expect("the bundled fonts load");
}

//! An icon from the kit's icon set, drawn in the text colour around it at the text's size unless
//! it is given its own.

use gpui_kit::{
    App, IntoElement, Refineable as _, RenderOnce, StyleRefinement, Styled, Transformation, Window,
    assets::IconName, svg,
};

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    style: StyleRefinement,
    transformation: Option<Transformation>,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            style: StyleRefinement::default(),
            transformation: None,
        }
    }

    /// Drawn turned or scaled about its centre, as a spinner or a swelling glyph is.
    pub fn transform(mut self, transformation: Transformation) -> Self {
        self.transformation = Some(transformation);
        self
    }

    fn has_size(&self) -> bool {
        self.style.size.width.is_some() || self.style.size.height.is_some()
    }
}

impl From<IconName> for Icon {
    fn from(name: IconName) -> Self {
        Self::new(name)
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let text = window.text_style();
        let sized = self.has_size();
        let mut el = svg()
            .path(self.name.path())
            .flex_shrink_0()
            .text_color(text.color);
        if !sized {
            el = el.size(text.font_size.to_pixels(window.rem_size()));
        }
        el.style().refine(&self.style);
        match self.transformation {
            Some(transformation) => el.with_transformation(transformation),
            None => el,
        }
    }
}

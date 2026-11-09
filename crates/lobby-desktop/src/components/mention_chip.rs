//! A mention in a Message, drawn as a chip. `lobby_client::message_text` writes it as a link whose
//! URL has the [`MENTION_SCHEME`]; this plugin takes that link before the Markdown view makes a
//! link of it, so a chip is never clickable and a Member cannot write one.

use gpui_kit::{
    App, BorderStyle, Bounds, ContentMask, Edges, Entity, FontWeight, Hsla, ParentElement as _,
    Pixels, Styled as _, Window,
    component::{
        ActiveTheme as _,
        input::TextareaState,
        text::{
            InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
            markdown_ast::Node,
        },
    },
    div, point, px, quad, size,
};
use lobby_client::{
    compose::mention_spans,
    message_text::{MENTION_SCHEME, MentionNames},
};

use crate::scale::u;

const NAME: &str = "lobby-mention";
/// The chip's air to the sides of its words.
const PAD_X: f32 = 4.;
/// How much shorter the chip is than the line, so chips on two lines do not touch.
const LINE_INSET: f32 = 3.;
const FILL_OPACITY: f32 = 0.16;
/// The chip's corner radius, also the composer's.
const RADIUS: f32 = PAD_X;

/// The chip's fill, behind its words.
pub(crate) fn fill(cx: &App) -> Hsla {
    cx.theme().primary.opacity(FILL_OPACITY)
}

/// A chip's fill behind each mention in the composer's field, painted over its text. Call it in
/// the paint of a canvas laid over the field and after it, so the field's layout is this frame's.
/// The text cannot change colour, so this is only the fill. `names` are the Members now; a
/// mention is what [`mention_spans`] says, the same as in the sent Message.
pub(crate) fn paint_in_field(
    field: &Bounds<Pixels>,
    input: &Entity<TextareaState>,
    names: &MentionNames,
    window: &mut Window,
    cx: &mut App,
) {
    let state = input.read(cx);
    let spans = mention_spans(&state.value(), names);
    if spans.is_empty() {
        return;
    }
    let color = fill(cx);
    let rem = window.rem_size();
    let inset = u(LINE_INSET).to_pixels(rem);
    let pad = u(PAD_X).to_pixels(rem);
    let radius = u(RADIUS).to_pixels(rem);
    window.with_content_mask(Some(ContentMask { bounds: *field }), |window| {
        for span in spans {
            // A span that wraps or is off screen has no one row to fill.
            let (Some(row), Some(all)) = (
                state.range_to_bounds(&(span.start..span.start)),
                state.range_to_bounds(&span),
            ) else {
                continue;
            };
            if (all.size.height - row.size.height).abs() > px(0.5) || all.size.width <= px(0.) {
                continue;
            }
            let chip = Bounds::new(
                point(all.origin.x - pad, all.origin.y + inset / 2.),
                size(all.size.width + pad * 2., all.size.height - inset),
            );
            window.paint_quad(quad(
                chip,
                radius,
                color,
                Edges::default(),
                color,
                BorderStyle::default(),
            ));
        }
    });
}

pub struct MentionChip;

impl MarkdownPlugin for MentionChip {
    fn name(&self) -> &str {
        NAME
    }

    fn parse(&self, node: &Node, _: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Link(link) = node else { return None };
        link.url.strip_prefix(MENTION_SCHEME)?;
        let words: String = link
            .children
            .iter()
            .filter_map(|c| match c {
                Node::Text(t) => Some(t.value.as_str()),
                _ => None,
            })
            .collect();
        Some(MarkdownNode::new(NAME, ()).text(words))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let lavender = cx.theme().primary;
        let height = context.line_height() - u(LINE_INSET).to_pixels(context.rem_size());
        Some(InlineElement::new(
            div()
                .flex()
                .items_center()
                .h(height)
                .px(u(PAD_X))
                .rounded(u(RADIUS))
                .bg(fill(cx))
                .text_color(lavender)
                .text_size(context.font_size())
                .line_height(context.line_height())
                .font_weight(FontWeight::SEMIBOLD)
                .child(node.as_text().to_owned()),
        ))
    }
}

//! A mention in a Message, drawn as a chip. `lobby_client::message_text` writes it as a link whose
//! URL has the [`MENTION_SCHEME`]; this plugin takes that link before the Markdown view makes a
//! link of it, so a chip is never clickable and a Member cannot write one.

use gpui_kit::{
    App, FontWeight, ParentElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _,
        text::{
            InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
            markdown_ast::Node,
        },
    },
    div,
};
use lobby_client::message_text::MENTION_SCHEME;

use crate::scale::u;

const NAME: &str = "lobby-mention";
/// The chip's air to the sides of its words.
const PAD_X: f32 = 4.;
/// How much shorter the chip is than the line, so chips on two lines do not touch.
const LINE_INSET: f32 = 3.;
const FILL_OPACITY: f32 = 0.16;

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
                .rounded(u(PAD_X))
                .bg(lavender.opacity(FILL_OPACITY))
                .text_color(lavender)
                .text_size(context.font_size())
                .line_height(context.line_height())
                .font_weight(FontWeight::SEMIBOLD)
                .child(node.as_text().to_owned()),
        ))
    }
}

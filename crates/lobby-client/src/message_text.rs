//! A Message's Markdown for display. The renderer (gpui-component's Markdown view) reads a wider
//! dialect than Messages are written in: it draws raw HTML, loads images and joins the lines of a
//! paragraph. So the content is parsed in the chat dialect and written back as Markdown that the
//! renderer cannot misread: every character of text escaped, a line break a hard break, links only
//! to absolute http, https and mailto URLs, and what chat has no construct for (raw HTML, images,
//! tables, task boxes, math, footnotes) as its text.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap, HashSet},
    ops::Range,
};

use lobby_protocol::{EVERYONE, Username, content};
use markdown::mdast::{List, Node};

use crate::{compose::longest_run, links::is_external_link};

/// The scheme of a mention in the Markdown for the renderer: `[\@Alice](<lobby-mention:alice>)`.
/// A link a Member types never keeps it (`is_external_link`), so only this module writes one.
pub const MENTION_SCHEME: &str = "lobby-mention:";

/// The usernames a Message can mention: the current Members', lowercase, and `everyone`. `@name`
/// of a Member who left stays text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionNames(HashSet<String>);

impl MentionNames {
    pub fn of<'a>(names: impl IntoIterator<Item = &'a Username>) -> Self {
        let mut names: HashSet<String> = names
            .into_iter()
            .map(|n| n.as_str().to_lowercase())
            .collect();
        names.insert(EVERYONE.to_owned());
        Self(names)
    }
}

/// An `@name` in some text that [`MentionNames`] holds: the bytes from the `@` to the end of the
/// name, and the name as typed.
pub(crate) struct Mention<'a> {
    pub range: Range<usize>,
    pub name: &'a str,
}

/// The mentions in `value`, in order: the `@` has no name character before it, and the whole run
/// of name characters after it is a name in `names` (any case).
pub(crate) fn mentions_in<'a>(
    value: &'a str,
    names: &'a MentionNames,
) -> impl Iterator<Item = Mention<'a>> {
    let mut after = 0;
    std::iter::from_fn(move || {
        while let Some(at) = value[after..].find('@').map(|i| i + after) {
            after = at + 1;
            let boundary = value[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !is_name_char(c));
            let name_len = value[after..]
                .find(|c| !is_name_char(c))
                .unwrap_or(value.len() - after);
            let name = &value[after..after + name_len];
            if boundary && names.0.contains(&name.to_lowercase()) {
                after += name_len;
                return Some(Mention {
                    range: at..after,
                    name,
                });
            }
        }
        None
    })
}

/// A Message's content, parsed once: Markdown for the renderer and the plain text it reads as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageText {
    pub markdown: String,
    pub plain: String,
    mentions: BTreeSet<String>,
}

impl MessageText {
    /// Content with nothing to read once parsed ("-", "1.", "> ") shows as typed.
    pub fn of(text: &str) -> MessageText {
        MessageText::of_in(text, &MentionNames::default())
    }

    /// Like [`MessageText::of`], and `@name` is a mention when `name` is in `names`, no letter,
    /// digit, `_` or `-` comes before the `@`, and the whole run of those after it is the name
    /// (any case). Code, code blocks and links hold none.
    pub fn of_in(text: &str, names: &MentionNames) -> MessageText {
        let root = content::parse(text).ok();
        let plain = root.as_ref().map_or_else(|| text.to_owned(), plain_of);
        let Some(root) = root.filter(|_| !plain.trim().is_empty()) else {
            let plain = if plain.trim().is_empty() {
                text.trim().to_owned()
            } else {
                plain
            };
            return MessageText {
                markdown: literal(text),
                plain,
                mentions: BTreeSet::new(),
            };
        };
        let mut defs = HashMap::new();
        collect_definitions(&root, &mut defs);
        let writer = Writer {
            defs,
            names,
            mentioned: RefCell::default(),
            in_link: Cell::new(false),
        };
        let markdown = writer.blocks(children(&root), "\n\n");
        MessageText {
            markdown,
            plain,
            mentions: writer.mentioned.into_inner(),
        }
    }

    /// Whether the Message mentions the Member with this username, by name or as `@everyone`.
    pub fn mentions(&self, username: &Username) -> bool {
        self.mentions.contains(&username.as_str().to_lowercase())
            || self.mentions.contains(EVERYONE)
    }
}

fn children(node: &Node) -> &[Node] {
    node.children().map_or(&[], Vec::as_slice)
}

fn literal(text: &str) -> String {
    text.trim()
        .split("\n\n")
        .map(|paragraph| {
            paragraph
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| escape(l.trim_end()))
                .collect::<Vec<_>>()
                .join("\\\n")
        })
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn collect_definitions(node: &Node, defs: &mut HashMap<String, String>) {
    if let Node::Definition(d) = node {
        defs.entry(d.identifier.clone())
            .or_insert_with(|| d.url.clone());
    }
    for child in children(node) {
        collect_definitions(child, defs);
    }
}

struct Writer<'a> {
    defs: HashMap<String, String>,
    names: &'a MentionNames,
    /// Lowercase names of the mentions written so far.
    mentioned: RefCell<BTreeSet<String>>,
    /// Inside a link's words, where a mention would nest a link in a link.
    in_link: Cell<bool>,
}

impl Writer<'_> {
    fn blocks(&self, nodes: &[Node], sep: &str) -> String {
        nodes
            .iter()
            .map(|n| self.block(n))
            .filter(|b| !b.trim().is_empty())
            .collect::<Vec<_>>()
            .join(sep)
    }

    fn block(&self, node: &Node) -> String {
        match node {
            Node::Paragraph(p) => self.inlines(&p.children),
            Node::Heading(h) => {
                let text = self.inlines(&h.children).replace("\\\n", " ");
                format!("{} {text}", "#".repeat(usize::from(h.depth)))
            }
            Node::ThematicBreak(_) => "***".to_owned(),
            Node::Blockquote(b) => self
                .blocks(&b.children, "\n\n")
                .lines()
                .map(|l| {
                    if l.is_empty() {
                        ">".to_owned()
                    } else {
                        format!("> {l}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Node::List(list) => self.list(list),
            Node::Code(code) => {
                let fence = "`".repeat(longest_run(&code.value, '`').max(2) + 1);
                format!("{fence}\n{}\n{fence}", code.value)
            }
            Node::Table(table) => table
                .children
                .iter()
                .map(|row| {
                    children(row)
                        .iter()
                        .map(|cell| self.inlines(children(cell)))
                        .collect::<Vec<_>>()
                        .join(" \\| ")
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
            Node::Definition(_) => String::new(),
            Node::Html(h) => literal(&h.value),
            Node::Math(m) => literal(&m.value),
            Node::Toml(t) => literal(&t.value),
            Node::Yaml(y) => literal(&y.value),
            Node::Root(_) | Node::FootnoteDefinition(_) | Node::ListItem(_) => {
                self.blocks(children(node), "\n\n")
            }
            inline => self.inlines(std::slice::from_ref(inline)),
        }
    }

    fn list(&self, list: &List) -> String {
        // The renderer numbers every list from 1: one that starts elsewhere keeps its numbers
        // as text.
        if let Some(start) = list.start.filter(|&s| list.ordered && s != 1) {
            return list
                .children
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let n = u64::from(start) + i as u64;
                    format!("{n}\\. {}", self.blocks(children(item), "\n\n"))
                })
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        let sep = if list.spread { "\n\n" } else { "\n" };
        let mut lines = Vec::new();
        for (i, item) in list.children.iter().enumerate() {
            let marker = if list.ordered {
                format!("{}. ", i + 1)
            } else {
                "- ".to_owned()
            };
            let (checked, spread) = match item {
                Node::ListItem(li) => (li.checked, li.spread),
                _ => (None, false),
            };
            let box_ = match checked {
                Some(true) => "☑ ",
                Some(false) => "☐ ",
                None => "",
            };
            let body = self.blocks(children(item), if spread { "\n\n" } else { "\n" });
            let pad = " ".repeat(marker.len());
            let mut item_lines = vec![];
            for (j, line) in body.lines().enumerate() {
                item_lines.push(match j {
                    0 => format!("{marker}{box_}{line}"),
                    _ if line.is_empty() => String::new(),
                    _ => format!("{pad}{line}"),
                });
            }
            if item_lines.is_empty() {
                item_lines.push(format!("{marker}{box_}").trim_end().to_owned());
            }
            lines.push(item_lines.join("\n"));
        }
        lines.join(sep)
    }

    fn inlines(&self, nodes: &[Node]) -> String {
        let mut out = String::new();
        for node in nodes {
            self.inline(node, &mut out);
        }
        trim_breaks(out)
    }

    fn inline(&self, node: &Node, out: &mut String) {
        match node {
            Node::Text(t) => self.words(&t.value, out),
            Node::Break(_) => out.push_str("\\\n"),
            Node::InlineCode(c) => code_span(&c.value, out),
            Node::Emphasis(e) => self.emphasis(&e.children, "*", out),
            Node::Strong(s) => self.emphasis(&s.children, "**", out),
            Node::Delete(d) => self.emphasis(&d.children, "~~", out),
            Node::Link(l) => self.link(&l.url, &l.children, out),
            Node::LinkReference(r) => {
                let url = self.defs.get(&r.identifier).map_or("", String::as_str);
                self.link(url, &r.children, out);
            }
            Node::Image(i) => {
                let words = if i.alt.trim().is_empty() {
                    &i.url
                } else {
                    &i.alt
                };
                let mut label = String::new();
                text(words, &mut label);
                link_to(&i.url, &label, out);
            }
            Node::ImageReference(i) => text(&i.alt, out),
            Node::Html(h) => text(&h.value, out),
            Node::InlineMath(m) => text(&m.value, out),
            Node::FootnoteReference(f) => {
                text(
                    &format!("[^{}]", f.label.as_ref().unwrap_or(&f.identifier)),
                    out,
                );
            }
            other => {
                for child in children(other) {
                    self.inline(child, out);
                }
            }
        }
    }

    /// The markers around the words, with edge spaces moved outside (`** x**` is not bold).
    fn emphasis(&self, nodes: &[Node], marker: &str, out: &mut String) {
        let mut inner = String::new();
        for node in nodes {
            self.inline(node, &mut inner);
        }
        let trimmed = inner.trim();
        if trimmed.is_empty() {
            out.push_str(&inner);
            return;
        }
        if inner.starts_with(' ') {
            out.push(' ');
        }
        out.push_str(marker);
        out.push_str(trimmed);
        out.push_str(marker);
        if inner.ends_with(' ') {
            out.push(' ');
        }
    }

    /// Text with each mention in it written as a link the renderer draws as a chip.
    fn words(&self, value: &str, out: &mut String) {
        let mut plain_from = 0;
        if !self.in_link.get() {
            for Mention { range, name } in mentions_in(value, self.names) {
                let lower = name.to_lowercase();
                text(&value[plain_from..range.start], out);
                out.push_str(&format!(
                    "[{}](<{MENTION_SCHEME}{lower}>)",
                    escape(&format!("@{name}"))
                ));
                self.mentioned.borrow_mut().insert(lower);
                plain_from = range.end;
            }
        }
        text(&value[plain_from..], out);
    }

    fn link(&self, url: &str, nodes: &[Node], out: &mut String) {
        let mut words = String::new();
        let was_in_link = self.in_link.replace(true);
        for node in nodes {
            self.inline(node, &mut words);
        }
        self.in_link.set(was_in_link);
        if words.trim().is_empty() {
            words.clear();
            text(url, &mut words);
        }
        link_to(url, &words, out);
    }
}

pub(crate) fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

fn link_to(url: &str, words: &str, out: &mut String) {
    if !is_external_link(url) {
        out.push_str(words);
        return;
    }
    let mut href = String::new();
    for c in url.trim().chars() {
        match c {
            // What would end or escape inside `<...>`.
            '<' => href.push_str("%3C"),
            '>' => href.push_str("%3E"),
            ' ' => href.push_str("%20"),
            '\\' => href.push_str("%5C"),
            '\n' | '\r' => {}
            c => href.push(c),
        }
    }
    out.push_str(&format!("[{words}](<{href}>)"));
}

fn text(value: &str, out: &mut String) {
    for (i, line) in value.split('\n').enumerate() {
        if i > 0 {
            out.push_str("\\\n");
        }
        out.push_str(&escape(line.strip_suffix('\r').unwrap_or(line)));
    }
}

/// Every ASCII punctuation character escaped: CommonMark allows it for all of them, and it keeps
/// text text (no emphasis, lists, headings, HTML, entities or autolinks out of what was typed).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn code_span(code: &str, out: &mut String) {
    let code = code.replace('\n', " ");
    if code.is_empty() {
        return;
    }
    let ticks = "`".repeat(longest_run(&code, '`') + 1);
    let pad = if code.starts_with(['`', ' ']) || code.ends_with(['`', ' ']) {
        " "
    } else {
        ""
    };
    out.push_str(&format!("{ticks}{pad}{code}{pad}{ticks}"));
}

/// A hard break at the end of a paragraph would show as a backslash.
fn trim_breaks(mut s: String) -> String {
    loop {
        let trimmed = s.trim_end_matches([' ', '\n']);
        match trimmed.strip_suffix('\\') {
            // An escaped backslash ("\\") is text, not a break.
            Some(t) if !t.ends_with('\\') => s = t.to_owned(),
            _ => return trimmed.to_owned(),
        }
    }
}

fn plain_of(root: &Node) -> String {
    let mut out = String::new();
    plain(root, &mut out);
    out.trim_end_matches('\n').to_owned()
}

fn plain(node: &Node, out: &mut String) {
    match node {
        Node::Text(t) => out.push_str(&t.value),
        Node::InlineCode(c) => out.push_str(&c.value),
        Node::Code(c) => out.push_str(&c.value),
        Node::Html(h) => out.push_str(&h.value),
        Node::Math(m) => out.push_str(&m.value),
        Node::InlineMath(m) => out.push_str(&m.value),
        Node::Break(_) => out.push('\n'),
        Node::Image(i) => out.push_str(&i.alt),
        Node::ImageReference(i) => out.push_str(&i.alt),
        Node::FootnoteReference(f) => {
            out.push_str(&format!("[^{}]", f.label.as_ref().unwrap_or(&f.identifier)));
        }
        Node::Definition(_) | Node::Toml(_) | Node::Yaml(_) | Node::ThematicBreak(_) => {}
        Node::TableRow(row) => {
            for (i, cell) in row.children.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                plain(cell, out);
            }
        }
        other => {
            if let Node::ListItem(li) = other {
                match li.checked {
                    Some(true) => out.push_str("☑ "),
                    Some(false) => out.push_str("☐ "),
                    None => {}
                }
            }
            for child in children(other) {
                plain(child, out);
            }
        }
    }
    let ends_block = matches!(
        node,
        Node::Paragraph(_) | Node::Heading(_) | Node::Code(_) | Node::TableRow(_) | Node::Math(_)
    );
    if ends_block && !out.ends_with('\n') {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_rendered(content: &str) -> Node {
        markdown::to_mdast(
            &MessageText::of(content).markdown,
            &markdown::ParseOptions::gfm(),
        )
        .unwrap()
    }

    fn any(node: &Node, f: &dyn Fn(&Node) -> bool) -> bool {
        f(node) || children(node).iter().any(|c| any(c, f))
    }

    #[test]
    fn markdown_reaches_the_renderer_and_plain_text_safely() {
        for content in [
            "<b>x</b>",
            "<img src=\"http://e/x.png\">",
            "<a href=\"javascript:alert(1)\">x</a>",
            "![x](http://e/x.png)",
            "![x][r]\n\n[r]: http://e/x.png",
            "two\nlines",
            "> quoted\n> lines",
            "- item\n  continued",
        ] {
            let rendered = as_rendered(content);
            assert!(
                !any(&rendered, &|n| matches!(
                    n,
                    Node::Html(_) | Node::Image(_) | Node::ImageReference(_)
                )),
                "{content:?}: {rendered:?}"
            );
            assert!(
                !any(
                    &rendered,
                    &|n| matches!(n, Node::Text(t) if t.value.contains('\n'))
                ),
                "{content:?} keeps a soft break: {rendered:?}"
            );
            assert!(
                !any(
                    &rendered,
                    &|n| matches!(n, Node::Link(l) if !is_external_link(&l.url))
                ),
                "{content:?}: {rendered:?}"
            );
        }
        assert!(any(&as_rendered("two\nlines"), &|n| matches!(
            n,
            Node::Break(_)
        )));

        let deep = ">".repeat(1000) + " x";
        let text = MessageText::of(&deep);
        assert_eq!((text.markdown, text.plain), (escape(&deep), deep));

        for (content, plain) in [
            ("**hi** see `x`\n\n- one\n- two", "hi see x\none\ntwo"),
            ("two\nlines", "two\nlines"),
            ("[link](https://e.x)", "link"),
            ("<b>raw</b>", "<b>raw</b>"),
            ("![cat](http://e/x.png)", "cat"),
            ("```\ncode\n```\nafter", "code\nafter"),
            ("-", "-"),
            ("  padded \n\n", "padded"),
        ] {
            assert_eq!(MessageText::of(content).plain, plain, "{content:?}");
        }
    }

    #[test]
    fn a_mention_is_a_current_member_at_a_word_boundary_outside_code_and_links() {
        let names = MentionNames::of(&[
            Username::parse("Alice").unwrap(),
            Username::parse("bob-b").unwrap(),
        ]);
        let alice = Username::parse("alice").unwrap();
        let chip = |content: &str| {
            MessageText::of_in(content, &names)
                .markdown
                .matches(MENTION_SCHEME)
                .count()
        };
        for (content, chips) in [
            ("hey @alice", 1),
            ("hey @ALICE, @bob-b!", 2),
            ("**@alice**\nand @alice's", 2),
            ("@alice", 1),
            ("mail me@alice.com", 0),
            ("@alicex", 0),
            ("@carol", 0),
            ("@bob", 0),
            ("`@alice`", 0),
            ("```\n@alice\n```", 0),
            ("[@alice](https://e.x/@alice)", 0),
            ("[x](lobby-mention:alice)", 0),
            ("a_@alice", 0),
            ("@everyone and @EveryOne", 2),
            ("@everyones", 0),
            ("`@everyone`", 0),
            ("[@everyone](https://e.x)", 0),
        ] {
            assert_eq!(chip(content), chips, "{content:?}");
            let text = MessageText::of_in(content, &names);
            assert_eq!(
                text.mentions(&alice),
                text.markdown.contains("mention:alice")
                    || text.markdown.contains("mention:everyone")
            );
            assert_eq!(text.plain, MessageText::of(content).plain, "{content:?}");
        }
        assert!(!MessageText::of("@alice @everyone").mentions(&alice));
        assert!(MessageText::of_in("@everyone", &names).mentions(&Username::parse("zed").unwrap()));
        assert!(!MessageText::of_in("@alice", &names).mentions(&Username::parse("zed").unwrap()));
    }
}

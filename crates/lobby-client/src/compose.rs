//! The Markdown-shortcut composer, pure: what Enter does on a line (send, continue a list, stay
//! in a code fence), what the toolbar and its shortcuts write around the selection, and a URL
//! pasted over a selection. The draft goes out as typed; [`crate::message_text`] renders it.
//!
//! Offsets are UTF-8 byte offsets, as `TextareaState` uses them.

use std::ops::Range;

use lobby_protocol::{EVERYONE, Username, content};
use markdown::mdast::Node;

use crate::{
    links::is_external_link,
    message_text::{MentionNames, is_name_char, mentions_in},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub selection: Range<usize>,
}

impl Edit {
    fn caret(text: String, at: usize) -> Edit {
        Edit {
            text,
            selection: at..at,
        }
    }
}

/// The most Members the mention menu offers at once.
pub const MENTION_MENU_MAX: usize = 5;

/// An `@name` being typed: the caret is in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionQuery {
    /// From the `@` to the end of the name.
    pub range: Range<usize>,
    /// The name up to the caret.
    pub typed: String,
}

/// The `@name` the caret is in, if the `@` starts a word (the same rule that makes a Message's
/// `@name` a mention).
pub fn mention_query(text: &str, caret: usize) -> Option<MentionQuery> {
    let before = text.get(..caret)?;
    let name_start = before.trim_end_matches(is_name_char).len();
    let at = name_start
        .checked_sub(1)
        .filter(|&at| before.as_bytes()[at] == b'@')?;
    if before[..at].chars().next_back().is_some_and(is_name_char) {
        return None;
    }
    let after = &text[caret..];
    let end = caret + after.len() - after.trim_start_matches(is_name_char).len();
    Some(MentionQuery {
        range: at..end,
        typed: before[name_start..].to_owned(),
    })
}

/// Who an `@name` can reach: one Member, or all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionTarget {
    Member(Username),
    Everyone,
}

impl MentionTarget {
    pub fn name(&self) -> &str {
        match self {
            MentionTarget::Member(username) => username.as_str(),
            MentionTarget::Everyone => EVERYONE,
        }
    }
}

/// The Members whose username starts with what was typed (any case), but not `me`, by name, and
/// `@everyone` last when it starts with what was typed too.
pub fn mention_matches<'a>(
    typed: &str,
    names: impl IntoIterator<Item = &'a Username>,
    me: Option<&Username>,
) -> Vec<MentionTarget> {
    let typed = typed.to_lowercase();
    let everyone = EVERYONE.starts_with(&typed);
    let mut found: Vec<&Username> = names
        .into_iter()
        .filter(|n| Some(*n) != me && n.as_str().to_lowercase().starts_with(&typed))
        .collect();
    found.sort_by_key(|n| n.as_str().to_lowercase());
    found.truncate(MENTION_MENU_MAX - usize::from(everyone));
    found
        .into_iter()
        .cloned()
        .map(MentionTarget::Member)
        .chain(everyone.then_some(MentionTarget::Everyone))
        .collect()
}

/// The query's `@name` replaced by `@name` of the target and a space, the caret after it.
pub fn complete_mention(text: &str, query: &MentionQuery, target: &MentionTarget) -> Edit {
    let rest = &text[query.range.end..];
    let space = if rest.starts_with(' ') { "" } else { " " };
    let head = format!("{}@{}{space}", &text[..query.range.start], target.name());
    let caret = head.len() + usize::from(space.is_empty());
    Edit::caret(format!("{head}{rest}"), caret)
}

/// The byte ranges of the mentions in a draft, from each `@` to the end of its name: the ones a
/// sent Message would show as chips. A draft that does not parse has none.
pub fn mention_spans(text: &str, names: &MentionNames) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    if text.contains('@')
        && let Ok(root) = content::parse(text)
    {
        collect_spans(&root, text, names, &mut spans);
    }
    spans
}

fn collect_spans(node: &Node, source: &str, names: &MentionNames, out: &mut Vec<Range<usize>>) {
    match node {
        Node::Link(_) | Node::LinkReference(_) => {}
        Node::Text(t) => {
            let found: Vec<_> = mentions_in(&t.value, names).collect();
            let Some(pos) = t.position.as_ref().filter(|_| !found.is_empty()) else {
                return;
            };
            let words = source.get(pos.start.offset..pos.end.offset).unwrap_or("");
            let Some(map) = source_offsets(&t.value, words) else {
                return;
            };
            for m in found {
                out.push(
                    pos.start.offset + map[m.range.start].1..pos.start.offset + map[m.range.end].0,
                );
            }
        }
        other => {
            for child in other.children().into_iter().flatten() {
                collect_spans(child, source, names, out);
            }
        }
    }
}

/// Where each char of a text node's value sits in its source, as (before its escape, after it).
/// The value drops backslash escapes and, around a line break, spaces and quote markers. `None`
/// when the source says more than that (an entity), so the node gets no chip.
fn source_offsets(value: &str, source: &str) -> Option<Vec<(usize, usize)>> {
    let mut map = vec![(0, 0); value.len() + 1];
    let mut at = 0;
    for (i, c) in value.char_indices() {
        if c == '\n' {
            at += source[at..].len() - source[at..].trim_start_matches([' ', '\t', '\r']).len();
        }
        let before = at;
        if c.is_ascii_punctuation()
            && let Some(escaped) = source[at..].strip_prefix('\\')
            && escaped.starts_with(c)
        {
            at += 1;
        }
        map[i] = (before, at);
        if !source[at..].starts_with(c) {
            return None;
        }
        at += c.len_utf8();
        if c == '\n' {
            // The next line's indent and quote markers.
            at += source[at..].len() - source[at..].trim_start_matches([' ', '\t', '>']).len();
        }
    }
    map[value.len()] = (at, at);
    Some(map)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enter {
    Send,
    Edit(Edit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Bold,
    Italic,
    Strike,
    Code,
    CodeBlock,
    BulletList,
    NumberedList,
    Quote,
    Link,
}

impl Format {
    pub const ALL: [Format; 9] = [
        Format::Bold,
        Format::Italic,
        Format::Strike,
        Format::Code,
        Format::Link,
        Format::BulletList,
        Format::NumberedList,
        Format::Quote,
        Format::CodeBlock,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Bold => "Bold",
            Format::Italic => "Italic",
            Format::Strike => "Strikethrough",
            Format::Code => "Code",
            Format::CodeBlock => "Code block",
            Format::BulletList => "Bulleted list",
            Format::NumberedList => "Numbered list",
            Format::Quote => "Quote",
            Format::Link => "Link",
        }
    }

    pub fn keystroke(self) -> &'static str {
        match self {
            Format::Bold => "ctrl-b",
            Format::Italic => "ctrl-i",
            Format::Strike => "ctrl-shift-x",
            Format::Code => "ctrl-e",
            Format::CodeBlock => "ctrl-shift-c",
            Format::BulletList => "ctrl-shift-8",
            Format::NumberedList => "ctrl-shift-7",
            Format::Quote => "ctrl-shift-9",
            Format::Link => "ctrl-k",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Format::Bold => "Ctrl+B",
            Format::Italic => "Ctrl+I",
            Format::Strike => "Ctrl+Shift+X",
            Format::Code => "Ctrl+E",
            Format::CodeBlock => "Ctrl+Shift+C",
            Format::BulletList => "Ctrl+Shift+8",
            Format::NumberedList => "Ctrl+Shift+7",
            Format::Quote => "Ctrl+Shift+9",
            Format::Link => "Ctrl+K",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Marker {
    indent: usize,
    content: usize,
    next: String,
}

fn marker(line: &str) -> Option<Marker> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let pad = " ".repeat(indent);
    for bullet in ["- ", "* ", "+ "] {
        if let Some(after) = rest.strip_prefix(bullet) {
            // A task item continues as an open task.
            for task in ["[ ] ", "[x] ", "[X] "] {
                if after.starts_with(task) {
                    return Some(Marker {
                        indent,
                        content: indent + bullet.len() + task.len(),
                        next: format!("{pad}{bullet}[ ] "),
                    });
                }
            }
            return Some(Marker {
                indent,
                content: indent + bullet.len(),
                next: format!("{pad}{bullet}"),
            });
        }
    }
    if rest.starts_with("> ") {
        return Some(Marker {
            indent,
            content: indent + 2,
            next: format!("{pad}> "),
        });
    }
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if (1..=9).contains(&digits) {
        let after = &rest[digits..];
        for delim in [". ", ") "] {
            if after.starts_with(delim) {
                let n: u64 = rest[..digits].parse().ok()?;
                return Some(Marker {
                    indent,
                    content: indent + digits + delim.len(),
                    next: format!("{pad}{}{delim}", n + 1),
                });
            }
        }
    }
    None
}

fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

pub fn on_enter(text: &str, caret: usize) -> Enter {
    let caret = floor_boundary(text, caret.min(text.len()));
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[caret..].find('\n').map_or(text.len(), |i| caret + i);
    let fences_before = text[..line_start].lines().filter(|l| is_fence(l)).count();
    let line = &text[line_start..line_end];
    let opening = fences_before % 2 == 0 && is_fence(&text[line_start..caret]);
    if fences_before % 2 == 1 && !is_fence(line) || opening {
        return Enter::Edit(insert(text, caret..caret, "\n"));
    }
    let Some(m) = marker(line) else {
        return Enter::Send;
    };
    if line[m.content..].trim().is_empty() {
        // An empty item ends the list: the marker goes, the line stays.
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..line_start]);
        out.push_str(&text[line_end..]);
        return Enter::Edit(Edit::caret(out, line_start));
    }
    if caret < line_start + m.content {
        return Enter::Edit(insert(text, caret..caret, "\n"));
    }
    Enter::Edit(insert(text, caret..caret, &format!("\n{}", m.next)))
}

fn insert(text: &str, range: Range<usize>, what: &str) -> Edit {
    let mut out = String::with_capacity(text.len() + what.len());
    out.push_str(&text[..range.start]);
    out.push_str(what);
    out.push_str(&text[range.end..]);
    Edit::caret(out, range.start + what.len())
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn clamp(text: &str, selection: Range<usize>) -> Range<usize> {
    let a = floor_boundary(text, selection.start.min(text.len()));
    let b = floor_boundary(text, selection.end.min(text.len()));
    a.min(b)..a.max(b)
}

pub fn apply(format: Format, text: &str, selection: Range<usize>) -> Edit {
    let sel = clamp(text, selection);
    match format {
        Format::Bold => wrap(text, sel, "**"),
        Format::Italic => wrap(text, sel, "*"),
        Format::Strike => wrap(text, sel, "~~"),
        Format::Code => wrap(text, sel, "`"),
        Format::CodeBlock => code_block(text, sel),
        Format::BulletList => prefix_lines(text, sel, LinePrefix::Bullet),
        Format::NumberedList => prefix_lines(text, sel, LinePrefix::Numbered),
        Format::Quote => prefix_lines(text, sel, LinePrefix::Quote),
        Format::Link => link(text, sel),
    }
}

fn wrap(text: &str, sel: Range<usize>, m: &str) -> Edit {
    let n = m.len();
    let (s, e) = (sel.start, sel.end);
    if s >= n
        && text.get(s - n..s) == Some(m)
        && text.get(e..e + n) == Some(m)
        && !(m == "*" && text.get(s.saturating_sub(2)..s) == Some("**"))
    {
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..s - n]);
        out.push_str(&text[s..e]);
        out.push_str(&text[e + n..]);
        return Edit {
            text: out,
            selection: s - n..e - n,
        };
    }
    let inner = &text[s..e];
    if inner.len() >= 2 * n && inner.starts_with(m) && inner.ends_with(m) {
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..s]);
        out.push_str(&inner[n..inner.len() - n]);
        out.push_str(&text[e..]);
        return Edit {
            text: out,
            selection: s..e - 2 * n,
        };
    }
    // Spaces at the selection's edges stay outside the markers (`** x**` is not bold).
    let lead = inner.len() - inner.trim_start().len();
    let trail = inner.len() - inner.trim_end().len();
    let (s, e) = if s + lead < e - trail {
        (s + lead, e - trail)
    } else {
        (s, e)
    };
    let mut out = String::with_capacity(text.len() + 2 * n);
    out.push_str(&text[..s]);
    out.push_str(m);
    out.push_str(&text[s..e]);
    out.push_str(m);
    out.push_str(&text[e..]);
    Edit {
        text: out,
        selection: s + n..e + n,
    }
}

fn code_block(text: &str, sel: Range<usize>) -> Edit {
    let (s, e) = (sel.start, sel.end);
    let before = if s == 0 || text[..s].ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let after = if e == text.len() || text[e..].starts_with('\n') {
        ""
    } else {
        "\n"
    };
    let inner = &text[s..e];
    let fence = "`".repeat(longest_run(inner, '`').max(2) + 1);
    let mut out = String::with_capacity(text.len() + 16);
    out.push_str(&text[..s]);
    out.push_str(before);
    out.push_str(&fence);
    out.push('\n');
    let body = out.len();
    out.push_str(inner);
    let body_end = out.len();
    out.push('\n');
    out.push_str(&fence);
    out.push_str(after);
    out.push_str(&text[e..]);
    Edit {
        text: out,
        selection: body..body_end,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinePrefix {
    Bullet,
    Numbered,
    Quote,
}

impl LinePrefix {
    fn has(self, line: &str) -> Option<usize> {
        let m = marker(line)?;
        let rest = line[m.indent..].trim_start();
        let ok = match self {
            LinePrefix::Bullet => {
                rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ")
            }
            LinePrefix::Quote => rest.starts_with("> "),
            LinePrefix::Numbered => rest.starts_with(|c: char| c.is_ascii_digit()),
        };
        ok.then_some(m.content)
    }

    fn marker(self, n: usize) -> String {
        match self {
            LinePrefix::Bullet => "- ".to_owned(),
            LinePrefix::Numbered => format!("{n}. "),
            LinePrefix::Quote => "> ".to_owned(),
        }
    }
}

fn prefix_lines(text: &str, sel: Range<usize>, kind: LinePrefix) -> Edit {
    let start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[sel.end..]
        .find('\n')
        .map_or(text.len(), |i| sel.end + i);
    let block = &text[start..end];
    let lines: Vec<&str> = block.split('\n').collect();
    let all_have = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .all(|l| kind.has(l).is_some())
        && lines.iter().any(|l| !l.trim().is_empty());
    let mut n = 0;
    let rewritten: Vec<String> = lines
        .iter()
        .map(|line| {
            if all_have {
                match kind.has(line) {
                    Some(content) => line[content..].to_owned(),
                    None => (*line).to_owned(),
                }
            } else if line.trim().is_empty() && lines.len() > 1 {
                (*line).to_owned()
            } else {
                n += 1;
                // Another kind of marker is replaced, not stacked.
                let body = marker(line).map_or(*line, |m| &line[m.content..]);
                format!("{}{body}", kind.marker(n))
            }
        })
        .collect();
    let replaced = rewritten.join("\n");
    let mut out = String::with_capacity(text.len() + 8);
    out.push_str(&text[..start]);
    out.push_str(&replaced);
    let caret = out.len();
    out.push_str(&text[end..]);
    Edit::caret(out, caret)
}

fn link(text: &str, sel: Range<usize>) -> Edit {
    let (s, e) = (sel.start, sel.end);
    let inner = &text[s..e];
    let mut out = String::with_capacity(text.len() + 16);
    out.push_str(&text[..s]);
    let selection = if s == e {
        // "[text](https://)" with "text" selected: type the words, then Tab… or just the URL.
        out.push('[');
        let a = out.len();
        out.push_str("text");
        let b = out.len();
        out.push_str("](https://)");
        a..b
    } else if is_external_link(inner.trim()) {
        // A URL selected: its words go between the brackets.
        out.push('[');
        let at = out.len();
        out.push_str("](");
        out.push_str(inner.trim());
        out.push(')');
        at..at
    } else {
        out.push('[');
        out.push_str(inner);
        out.push_str("](");
        let a = out.len();
        out.push_str("https://");
        let b = out.len();
        out.push(')');
        a..b
    };
    out.push_str(&text[e..]);
    Edit {
        text: out,
        selection,
    }
}

/// The constructs the caret or selection sits inside, for the bar's buttons to show as on: an
/// inline mark when the selection lies within its markers (`**bo|ld**`), a block when it lies
/// in the block (a list item, a quote, a fence).
pub fn active_formats(text: &str, selection: Range<usize>) -> Vec<Format> {
    let sel = clamp(text, selection);
    let Ok(root) = content::parse(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    active_in(&root, &sel, &mut out);
    out
}

fn active_in(node: &Node, sel: &Range<usize>, out: &mut Vec<Format>) {
    if let Some(pos) = node.position() {
        let (start, end) = (pos.start.offset, pos.end.offset);
        let inside = start < sel.start && sel.end < end;
        let within = start <= sel.start && sel.end <= end;
        if !within {
            return;
        }
        let format = match node {
            Node::Strong(_) if inside => Some(Format::Bold),
            Node::Emphasis(_) if inside => Some(Format::Italic),
            Node::Delete(_) if inside => Some(Format::Strike),
            Node::InlineCode(_) if inside => Some(Format::Code),
            Node::Link(_) if inside => Some(Format::Link),
            Node::Code(_) => Some(Format::CodeBlock),
            Node::Blockquote(_) => Some(Format::Quote),
            Node::List(l) if l.ordered => Some(Format::NumberedList),
            Node::List(_) => Some(Format::BulletList),
            _ => None,
        };
        if let Some(f) = format
            && !out.contains(&f)
        {
            out.push(f);
        }
    }
    for child in node.children().into_iter().flatten() {
        active_in(child, sel, out);
    }
}

/// A URL pasted over selected words links them: `[words](url)`. `None`: paste as usual.
pub fn paste_link(text: &str, selection: Range<usize>, pasted: &str) -> Option<Edit> {
    let sel = clamp(text, selection);
    let url = pasted.trim();
    if sel.is_empty() || url.contains(char::is_whitespace) || !is_external_link(url) {
        return None;
    }
    let words = &text[sel.clone()];
    if words.contains('\n') || is_external_link(words.trim()) {
        return None;
    }
    let link = format!("[{words}]({url})");
    Some(insert(text, sel, &link))
}

pub(crate) fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        if ch == c {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_text::{MENTION_SCHEME, MessageText};

    #[test]
    fn enter_sends_continues_lists_and_stays_in_fences() {
        assert_eq!(on_enter("hello", 5), Enter::Send);
        let edit = |text: &str, caret| match on_enter(text, caret) {
            Enter::Edit(e) => (e.text, e.selection.start),
            Enter::Send => panic!("{text:?} sent"),
        };
        assert_eq!(edit("- one", 5), ("- one\n- ".into(), 8));
        assert_eq!(edit("* one", 5), ("* one\n* ".into(), 8));
        assert_eq!(edit("1. one", 6), ("1. one\n2. ".into(), 10));
        assert_eq!(edit("9) nine", 7), ("9) nine\n10) ".into(), 12));
        assert_eq!(edit("  - deep", 8), ("  - deep\n  - ".into(), 13));
        assert_eq!(edit("> said", 6), ("> said\n> ".into(), 9));
        assert_eq!(edit("- [x] done", 10), ("- [x] done\n- [ ] ".into(), 17));
        assert_eq!(edit("- one\n- ", 8), ("- one\n".into(), 6));
        assert_eq!(edit("1. a\n2. ", 8), ("1. a\n".into(), 5));
        assert_eq!(edit("- onetwo", 5), ("- one\n- two".into(), 8));
        assert_eq!(edit("```", 3), ("```\n".into(), 4));
        assert_eq!(edit("```rust", 7), ("```rust\n".into(), 8));
        assert_eq!(edit("```\nfn x()", 10), ("```\nfn x()\n".into(), 11));
        assert_eq!(on_enter("```\nx\n```", 9), Enter::Send);
        assert_eq!(on_enter("a ``` b", 7), Enter::Send);
        assert_eq!(edit("```\n- a", 7), ("```\n- a\n".into(), 8));
    }

    #[test]
    fn toolbar_wraps_and_unwraps_the_selection() {
        let e = apply(Format::Bold, "make bold now", 5..9);
        assert_eq!(e.text, "make **bold** now");
        assert_eq!(e.selection, 7..11);
        let e = apply(Format::Bold, &e.text, e.selection);
        assert_eq!(e.text, "make bold now");
        assert_eq!(e.selection, 5..9);
        assert_eq!(apply(Format::Code, "`x`", 0..3).text, "x");
        let e = apply(Format::Italic, "ab", 1..1);
        assert_eq!((e.text.as_str(), e.selection), ("a**b", 2..2));
        assert_eq!(apply(Format::Strike, "a b c", 1..4).text, "a ~~b~~ c");
        assert_eq!(apply(Format::Italic, "**b**", 2..3).text, "***b***");
    }

    #[test]
    fn a_mention_is_offered_at_a_word_start_and_completed_with_a_space() {
        let name = |s: &str| Username::parse(s).unwrap();
        let query = |text: &str, caret| mention_query(text, caret).map(|q| (q.range, q.typed));
        assert_eq!(query("hi @al", 6), Some((3..6, "al".into())));
        assert_eq!(query("@", 1), Some((0..1, String::new())));
        assert_eq!(query("hi @al and", 6), Some((3..6, "al".into())));
        assert_eq!(query("hi @al", 4), Some((3..6, String::new())));
        assert_eq!(query("me@al", 5), None);
        assert_eq!(query("hi @al ", 7), None);
        assert_eq!(query("hi al", 5), None);

        let members = [name("Alice"), name("alan"), name("bob"), name("amy")];
        let (alice, bob) = (&members[0], &members[2]);
        let member = |i: usize| MentionTarget::Member(members[i].clone());
        let found = mention_matches("A", &members, Some(alice));
        assert_eq!(found, [member(1), member(3)]);
        assert_eq!(mention_matches("", &members, Some(bob)).len(), 4);
        assert_eq!(mention_matches("zz", &members, None).len(), 0);
        assert_eq!(
            mention_matches("EV", &members, None),
            [MentionTarget::Everyone]
        );

        let many: Vec<Username> = (0..9).map(|i| name(&format!("mem{i}"))).collect();
        let all = mention_matches("", &many, None);
        assert_eq!(all.len(), MENTION_MENU_MAX);
        assert_eq!(all.last(), Some(&MentionTarget::Everyone));
        assert_eq!(mention_matches("m", &many, None).len(), MENTION_MENU_MAX);

        let q = mention_query("hi @al and", 6).unwrap();
        let e = complete_mention("hi @al and", &q, &member(0));
        assert_eq!((e.text.as_str(), e.selection), ("hi @Alice and", 10..10));
        let q = mention_query("hi @al", 6).unwrap();
        let e = complete_mention("hi @al", &q, &member(0));
        assert_eq!((e.text.as_str(), e.selection), ("hi @Alice ", 10..10));
        let q = mention_query("@ev", 3).unwrap();
        let e = complete_mention("@ev", &q, &MentionTarget::Everyone);
        assert_eq!((e.text.as_str(), e.selection), ("@everyone ", 10..10));
    }

    #[test]
    fn mention_spans_are_what_a_sent_message_shows_as_chips() {
        let names = MentionNames::of(&[Username::parse("bob").unwrap()]);
        fn words<'a>(text: &'a str, names: &MentionNames) -> Vec<&'a str> {
            mention_spans(text, names)
                .into_iter()
                .map(|r| &text[r])
                .collect()
        }
        let spans = |text: &'static str| words(text, &names);
        assert_eq!(spans("hi @bob and @EVERYONE!"), ["@bob", "@EVERYONE"]);
        assert_eq!(spans("one\n  two @bob"), ["@bob"]);
        assert_eq!(spans("é @bob"), ["@bob"]);
        assert_eq!(spans("**@bob** > @bob"), ["@bob", "@bob"]);
        assert_eq!(spans("\\*x\\* \\@bob a  \n  @bob"), ["@bob", "@bob"]);
        assert_eq!(
            mention_spans("\\@bob", &names),
            [1..5],
            "an escape stays outside the chip"
        );
        for none in [
            "@nobody",
            "@bobby",
            "me@bob.com",
            "`@bob`",
            "```\n@bob\n```",
            "[@bob](https://e.x)",
            "see https://e.x/@bob",
            "a_@bob",
        ] {
            assert!(spans(none).is_empty(), "{none:?}");
        }

        for content in [
            "hey @bob",
            "> quoted\n> @bob and @everyone",
            "- @bob\n- @nobody",
            "# @bob",
            "x ~~@bob~~ _@everyone_",
            "| a | @bob |\n|---|---|\n| @bob | b |",
        ] {
            assert_eq!(
                mention_spans(content, &names).len(),
                MessageText::of_in(content, &names)
                    .markdown
                    .matches(MENTION_SCHEME)
                    .count(),
                "{content:?}"
            );
        }
        assert!(mention_spans(&(">".repeat(1000) + " @bob"), &names).is_empty());
    }
}

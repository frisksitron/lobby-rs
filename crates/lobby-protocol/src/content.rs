//! Message content: Markdown as the Member typed it, in one dialect that the server validates
//! and every client parses the same way.

use markdown::{Constructs, ParseOptions, mdast::Node};

use crate::limits::MESSAGE_NESTING_MAX;

/// Bidi marks, embeddings, overrides and isolates: `report<U+202E>fdp.bat` shows as
/// `reporttab.pdf` and is a batch file.
pub fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// GFM for chat, not documents: four leading spaces stay text, "text\n---" stays text, raw HTML
/// is literal text, and "about ~5 min" is not strikethrough ("~~this~~" is).
fn parse_options() -> ParseOptions {
    ParseOptions {
        constructs: Constructs {
            code_indented: false,
            heading_setext: false,
            html_flow: false,
            html_text: false,
            frontmatter: false,
            ..Constructs::gfm()
        },
        gfm_strikethrough_single_tilde: false,
        ..ParseOptions::gfm()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContentError {
    #[error("Message is nested too deeply")]
    TooNested,
}

pub fn parse(content: &str) -> Result<Node, ContentError> {
    // markdown-rs takes time quadratic in a run of nested markers on one line and recurses with
    // it, so such a run is refused before the parse.
    if content.lines().any(|l| marker_run(l) > MESSAGE_NESTING_MAX) {
        return Err(ContentError::TooNested);
    }
    let root =
        markdown::to_mdast(content, &parse_options()).map_err(|_| ContentError::TooNested)?;
    if depth_exceeds(&root, MESSAGE_NESTING_MAX) {
        return Err(ContentError::TooNested);
    }
    Ok(root)
}

fn marker_run(line: &str) -> usize {
    let mut rest = line.trim_start();
    let mut run = 0;
    loop {
        if is_thematic_break(rest) {
            return run;
        }
        let after = if let Some(r) = rest.strip_prefix('>') {
            r
        } else if let Some(r) = rest
            .strip_prefix(['-', '*', '+'])
            .filter(|r| r.starts_with([' ', '\t']))
        {
            r
        } else {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            match rest[digits..].strip_prefix(['.', ')']) {
                Some(r) if digits > 0 && r.starts_with([' ', '\t']) => r,
                _ => return run,
            }
        };
        run += 1;
        rest = after.trim_start();
    }
}

/// "- - -" or "* * *": three or more of one marker and nothing else. CommonMark reads it as a rule
/// before it reads a list item, so its markers are no nesting.
fn is_thematic_break(rest: &str) -> bool {
    let mut marks = rest.chars().filter(|c| !matches!(c, ' ' | '\t'));
    let Some(first) = marks.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    let mut count = 1;
    for c in marks {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

fn depth_exceeds(node: &Node, max: usize) -> bool {
    let mut stack = vec![(node, 0)];
    while let Some((node, depth)) = stack.pop() {
        if depth > max {
            return true;
        }
        for child in node.children().into_iter().flatten() {
            stack.push((child, depth + 1));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_nesting_is_refused_and_ordinary_markdown_is_not() {
        for ok in [
            "**hi** there\n\n- a\n  - b\n\n> quote",
            "> > > three deep",
            "1. one\n2. two",
            "* * *",
            "- ".repeat(4000).as_str(),
            format!("> - {}", "* ".repeat(40)).as_str(),
        ] {
            assert!(parse(ok).is_ok(), "{ok}");
        }
        let deep_list: String = (0..40)
            .map(|i| format!("{}- x\n", "  ".repeat(i)))
            .collect();
        for deep in [
            ">".repeat(1000),
            "> ".repeat(4000),
            "- * ".repeat(2000),
            "1. ".repeat(2000),
            deep_list,
        ] {
            assert_eq!(parse(&deep).err(), Some(ContentError::TooNested));
        }
    }

    #[test]
    fn pathological_inline_content_parses_on_a_small_stack() {
        let inputs = [
            "*".repeat(8000),
            "[".repeat(8000),
            "![".repeat(4000),
            "*a ".repeat(2666),
            "**a _b ".repeat(1000),
            "`".repeat(8000),
            "<".repeat(8000),
        ];
        std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                for input in &inputs {
                    let _ = parse(input);
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

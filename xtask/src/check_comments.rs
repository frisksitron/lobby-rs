//! `cargo xtask check-comments`: rustfmt wraps code at 100 columns but never comments, so this
//! holds every comment in the Rust sources of `crates/` and `xtask/`, and in every `Cargo.toml`, to
//! the same width. It also catches the mark of a broken rewrap script, a literal `\n` followed by
//! the comment marker inside a comment, and comments that cite the design docs, which no longer
//! live in the repository.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

const MAX_WIDTH: usize = 100;
const ROOTS: [&str; 2] = ["crates", "xtask"];

#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
    pub problem: &'static str,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.path.display(), self.line, self.problem)
    }
}

pub fn check(workspace: &Path) -> io::Result<Vec<Finding>> {
    let mut files = vec![workspace.join("Cargo.toml")];
    for root in ROOTS {
        collect(&workspace.join(root), &mut files)?;
    }
    files.sort();
    let mut findings = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path)?;
        let shown = path.strip_prefix(workspace).unwrap_or(&path);
        findings.extend(
            check_text(&text, Marker::of(&path))
                .into_iter()
                .map(|(line, problem)| Finding {
                    path: shown.to_owned(),
                    line,
                    problem,
                }),
        );
    }
    Ok(findings)
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                collect(&path, files)?;
            }
        } else if path.extension().is_some_and(|e| e == "rs")
            || path.file_name().is_some_and(|n| n == "Cargo.toml")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// What starts a comment: `//` in Rust, `#` in TOML.
#[derive(Debug, Clone, Copy)]
struct Marker(&'static str);

impl Marker {
    const RUST: Self = Self("//");
    const TOML: Self = Self("#");

    fn of(path: &Path) -> Self {
        if path.extension().is_some_and(|e| e == "toml") {
            Self::TOML
        } else {
            Self::RUST
        }
    }
}

/// One-based line numbers of the comments that break a rule.
fn check_text(text: &str, marker: Marker) -> Vec<(usize, &'static str)> {
    let mut findings = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let Some(comment) = comment_of(line, marker) else {
            continue;
        };
        if line.chars().count() > MAX_WIDTH {
            findings.push((i + 1, "comment over 100 columns"));
        }
        if has_escaped_newline_comment(comment, marker) {
            findings.push((i + 1, "literal \\n followed by a comment marker"));
        }
        if cites_design_docs(comment) {
            findings.push((i + 1, "cites the old design docs; say the why instead"));
        }
    }
    findings
}

/// The comment on a line: one that starts the line, or one after code outside any string.
fn comment_of(line: &str, marker: Marker) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with(marker.0) {
        return Some(trimmed);
    }
    let bytes = line.as_bytes();
    let (mut in_string, mut escaped) = (false, false);
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            _ if escaped => escaped = false,
            b'\\' if in_string => escaped = true,
            b'"' => in_string = !in_string,
            // The markers are ASCII, so a match starts on a char boundary.
            _ if !in_string && bytes[i..].starts_with(marker.0.as_bytes()) => {
                return Some(&line[i..]);
            }
            _ => {}
        }
    }
    None
}

fn has_escaped_newline_comment(comment: &str, marker: Marker) -> bool {
    comment
        .match_indices("\\n")
        .any(|(i, _)| comment[i + 2..].trim_start().starts_with(marker.0))
}

/// The design docs left the repository, so a comment that points at one points at nothing.
fn cites_design_docs(comment: &str) -> bool {
    const DOCS: [&str; 6] = [
        "ARCHITECTURE",
        "PROTOCOL.md",
        "SCHEMA.md",
        "PLAN.md",
        "UX.md",
        "§",
    ];
    DOCS.iter().any(|d| comment.contains(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_and_mangled_comments_are_found_and_code_is_not() {
        let long = format!("    /// {}", "word ".repeat(20));
        let trailing = format!("    let x = 1; // {}", "word ".repeat(20));
        let long_string = format!("    let url = \"https://{}\";", "a".repeat(100));
        let mangled = "    // The first half of a sentence.\\n    // the second half.";
        let in_string = "    let s = \"a\\n// b\";";
        let text = [
            long.as_str(),
            trailing.as_str(),
            long_string.as_str(),
            mangled,
            in_string,
            "    /// Decoded under the server's limits (ARCHITECTURE §4.12).",
        ]
        .join("\n");
        assert_eq!(
            check_text(&text, Marker::RUST),
            [
                (1, "comment over 100 columns"),
                (2, "comment over 100 columns"),
                (4, "literal \\n followed by a comment marker"),
                (6, "cites the old design docs; say the why instead"),
            ]
        );
    }

    #[test]
    fn cargo_toml_comments_are_checked_and_values_are_not() {
        let long = format!("# {}", "word ".repeat(20));
        let long_value = format!("windows = {{ features = [\"{}\"] }}", "A".repeat(100));
        let text = [
            long.as_str(),
            long_value.as_str(),
            "# The OS idle time; the version gpui already\\n# builds.",
            "url = \"https://example.com/#anchor\" # a short note",
        ]
        .join("\n");
        assert_eq!(
            check_text(&text, Marker::TOML),
            [
                (1, "comment over 100 columns"),
                (3, "literal \\n followed by a comment marker"),
            ]
        );
    }
}

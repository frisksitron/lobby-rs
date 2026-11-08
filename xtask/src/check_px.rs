//! `cargo xtask check-px`: the desktop's layout sizes are `u(design_px)` (see `scale.rs`), which
//! follow the person's UI scale. A bare `px(..)` in a view file does not, so it must say why it
//! stays in pixels with a `// px: <why>` comment on its line: a hairline, an OS value, or an API
//! that takes only `Pixels`.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

const ROOT: &str = "crates/lobby-desktop/src";
/// Directories and files where pixels are the unit: the dev tools, the automation channel, the
/// OS window and the scale helpers themselves.
const SKIP_DIRS: [&str; 4] = ["gallery", "automation", "host", "bin"];
const SKIP_FILES: [&str; 3] = ["window_bounds.rs", "main.rs", "scale.rs"];
const MARKER: &str = "// px:";

#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: `px(..)` does not follow the UI scale: write `u(..)`, or add `{MARKER} <why>`",
            self.path.display(),
            self.line
        )
    }
}

pub fn check(workspace: &Path) -> io::Result<Vec<Finding>> {
    let mut files = Vec::new();
    collect(&workspace.join(ROOT), true, &mut files)?;
    files.sort();
    let mut findings = Vec::new();
    for path in files {
        let shown = path.strip_prefix(workspace).unwrap_or(&path).to_owned();
        findings.extend(
            check_text(&fs::read_to_string(&path)?)
                .into_iter()
                .map(|line| Finding {
                    path: shown.clone(),
                    line,
                }),
        );
    }
    Ok(findings)
}

fn collect(dir: &Path, top: bool, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !(top && SKIP_DIRS.contains(&name)) {
                collect(&path, false, files)?;
            }
        } else if name.ends_with(".rs") && !(top && SKIP_FILES.contains(&name)) {
            files.push(path);
        }
    }
    Ok(())
}

/// The 1-based lines with a `px(` call and no marker, above the file's `#[cfg(test)]`.
fn check_text(text: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    for (ix, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let code = line.split("//").next().unwrap_or(line);
        if calls_px(code) && !line.contains(MARKER) {
            lines.push(ix + 1);
        }
    }
    lines
}

/// `px(` that is the free function, not a method (`.px(`) or part of a longer name (`fn px(`).
fn calls_px(code: &str) -> bool {
    code.match_indices("px(").any(|(at, _)| {
        let before = code[..at].chars().next_back();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && !code[..at].trim_end().ends_with("fn")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_px_call_needs_its_reason() {
        let text = "a.w(px(3.))\nb.px(u(4.))\nfn px(self) -> f32 {\nlet y = 2; // px(\n\
                    c.h(px(1.)) // px: hairline\nd.gap(gpui_kit::px(2.))\n";
        assert_eq!(check_text(text), vec![1, 6]);
    }

    #[test]
    fn tests_are_left_alone() {
        assert_eq!(
            check_text("x\n#[cfg(test)]\nmod t { let a = px(1.); }\n"),
            Vec::<usize>::new()
        );
    }
}

//! `cargo xtask check-kit`: the desktop draws its controls on gpui-kit's unstyled base layer and
//! keeps only a few of the kit's styled components. Those reach the crate through one file,
//! `crates/lobby-desktop/src/kit.rs`, so a new use of the styled layer shows up there in review.
//! A `component::` path anywhere else in the crate, inline paths included, is a finding.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

const ROOT: &str = "crates/lobby-desktop";
const FACADE: &str = "src/kit.rs";
const PATTERN: &str = "component::";

#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: `gpui_kit::component` is reached only through `{ROOT}/{FACADE}`: import the \
             item from `crate::kit` (add it there if it is missing), or draw it with `crate::ui`",
            self.path.display(),
            self.line
        )
    }
}

pub fn check(workspace: &Path) -> io::Result<Vec<Finding>> {
    let root = workspace.join(ROOT);
    let mut files = Vec::new();
    collect(&root, &mut files)?;
    files.retain(|path| path != &root.join(FACADE));
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

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

/// The 1-based lines whose code, comments left out, names a `component::` path.
fn check_text(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.split("//").next().unwrap_or(line).contains(PATTERN))
        .map(|(ix, _)| ix + 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_component_path_outside_the_facade_is_found_in_code_only() {
        let text = "use gpui_kit::{\n    component::{Icon, button::Button},\n};\n\
                    let t = cx.global::<gpui_kit::component::Theme>();\n\
                    //! Built on `component::Avatar`'s idea\nuse crate::kit::Icon;\n";
        assert_eq!(check_text(text), vec![2, 4]);
    }
}

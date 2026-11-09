//! Commit subjects and the changelog they make. A subject is
//! `<type>[(<scope>)][!]: <description>`; the commits since the last tag are the next release's
//! entry in `CHANGELOG.md`, which `cargo xtask dist` also publishes as the Release's description.
//! [`TABLE`] decides what an entry shows.

use std::{fs, path::Path};

use crate::git;

pub const FILE: &str = "CHANGELOG.md";
const TITLE: &str = "# Changelog";

const TYPES: [&str; 10] = [
    "feat", "fix", "refactor", "perf", "style", "test", "docs", "build", "ops", "chore",
];
const SCOPE_LEN: std::ops::RangeInclusive<usize> = 1..=20;

enum Shown {
    Breaking,
    Type(&'static str),
}

/// What a release shows, in order. A commit goes under the first heading it matches, and a type
/// with no row is hidden.
const TABLE: [(&str, Shown); 4] = [
    ("Breaking", Shown::Breaking),
    ("New", Shown::Type("feat")),
    ("Fixed", Shown::Type("fix")),
    ("Improved", Shown::Type("perf")),
];

struct Subject<'a> {
    kind: &'a str,
    breaking: bool,
    description: &'a str,
}

impl Subject<'_> {
    fn heading(&self) -> Option<&'static str> {
        TABLE
            .iter()
            .find(|(_, shown)| match shown {
                Shown::Breaking => self.breaking,
                Shown::Type(kind) => !self.breaking && *kind == self.kind,
            })
            .map(|(heading, _)| *heading)
    }

    fn line(&self) -> String {
        let mut chars = self.description.chars();
        let first = chars.next().into_iter().flat_map(char::to_uppercase);
        first.chain(chars).collect()
    }
}

/// `None` for a `Revert "..."` subject, which is valid and never shown.
fn parse_subject(subject: &str) -> Result<Option<Subject<'_>>, String> {
    if subject.starts_with("Revert \"") {
        return Ok(None);
    }
    let shape = || "is not `<type>[(<scope>)][!]: <description>`".to_owned();
    let (header, description) = subject.split_once(": ").ok_or_else(shape)?;
    let (header, breaking) = match header.strip_suffix('!') {
        Some(rest) => (rest, true),
        None => (header, false),
    };
    let (kind, scope) = match header.split_once('(') {
        Some((kind, scope)) => (kind, Some(scope.strip_suffix(')').ok_or_else(shape)?)),
        None => (header, None),
    };
    if !TYPES.contains(&kind) {
        return Err(format!("type {kind:?} is none of {}", TYPES.join(", ")));
    }
    if let Some(scope) = scope
        && !(SCOPE_LEN.contains(&scope.len())
            && scope
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
    {
        return Err("scope is not 1 to 20 lowercase letters, digits or hyphens".into());
    }
    if description.is_empty() {
        return Err("description is empty".into());
    }
    if description.starts_with(char::is_uppercase) {
        return Err("description starts with an uppercase letter".into());
    }
    if description.ends_with('.') {
        return Err("description ends with a period".into());
    }
    Ok(Some(Subject {
        kind,
        breaking,
        description,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub version: String,
    pub date: String,
    pub sections: Vec<Section>,
}

impl Entry {
    /// The sections as the Release's notes: no version heading.
    pub fn body(&self) -> String {
        let mut out = String::new();
        for section in &self.sections {
            out += &format!("\n### {}\n\n", section.heading);
            for line in &section.lines {
                out += &format!("- {line}\n");
            }
        }
        out.trim_start().to_owned()
    }

    fn render(&self) -> String {
        let body = self.body();
        let gap = if body.is_empty() { "" } else { "\n" };
        format!("## {} ({})\n{gap}{body}", self.version, self.date)
    }
}

/// The entry for `subjects` (oldest first), or every subject that breaks the rules.
pub fn entry(version: &str, date: &str, subjects: &[String]) -> Result<Entry, Vec<String>> {
    let problems = problems(subjects);
    if !problems.is_empty() {
        return Err(problems);
    }
    let parsed: Vec<Subject> = subjects
        .iter()
        .filter_map(|s| parse_subject(s).ok().flatten())
        .collect();
    let sections = TABLE
        .iter()
        .filter_map(|(heading, _)| {
            let lines: Vec<String> = parsed
                .iter()
                .filter(|s| s.heading() == Some(*heading))
                .map(Subject::line)
                .collect();
            (!lines.is_empty()).then(|| Section {
                heading: (*heading).to_owned(),
                lines,
            })
        })
        .collect();
    Ok(Entry {
        version: version.to_owned(),
        date: date.to_owned(),
        sections,
    })
}

fn problems(subjects: &[String]) -> Vec<String> {
    subjects
        .iter()
        .filter_map(|s| parse_subject(s).err().map(|rule| format!("{s:?} {rule}")))
        .collect()
}

/// `changelog` with `entry` on top, under the title.
pub fn prepend(changelog: &str, entry: &Entry) -> Result<String, String> {
    let rest = changelog
        .strip_prefix(TITLE)
        .ok_or_else(|| format!("{FILE} must start with `{TITLE}`"))?;
    let rest = rest.trim_start();
    let gap = if rest.is_empty() { "" } else { "\n" };
    Ok(format!("{TITLE}\n\n{}{gap}{rest}", entry.render()))
}

/// Every entry of `changelog`, newest first as written.
pub fn parse(changelog: &str) -> Result<Vec<Entry>, String> {
    let mut lines = changelog
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty());
    if lines.next().map(|(_, line)| line) != Some(TITLE) {
        return Err(format!("{FILE} must start with `{TITLE}`"));
    }
    let mut entries: Vec<Entry> = Vec::new();
    for (n, line) in lines {
        let bad = || format!("{FILE} line {}: {line:?} is not part of the format", n + 1);
        if let Some(heading) = line.strip_prefix("## ") {
            let (version, date) = heading
                .strip_suffix(')')
                .and_then(|h| h.split_once(" ("))
                .ok_or_else(bad)?;
            entries.push(Entry {
                version: version.to_owned(),
                date: date.to_owned(),
                sections: Vec::new(),
            });
        } else if let Some(heading) = line.strip_prefix("### ") {
            let entry = entries.last_mut().ok_or_else(bad)?;
            entry.sections.push(Section {
                heading: heading.to_owned(),
                lines: Vec::new(),
            });
        } else if let Some(text) = line.strip_prefix("- ") {
            let section = entries
                .last_mut()
                .and_then(|e| e.sections.last_mut())
                .ok_or_else(bad)?;
            section.lines.push(text.to_owned());
        } else {
            return Err(bad());
        }
    }
    Ok(entries)
}

pub fn read(workspace: &Path) -> Result<String, String> {
    fs::read_to_string(workspace.join(FILE)).map_err(|e| format!("{FILE}: {e}"))
}

/// The subjects of the commits since the last tag, oldest first, merges left out.
pub fn subjects_since_last_tag(workspace: &Path) -> Result<Vec<String>, String> {
    let tag = git(workspace, &["describe", "--tags", "--abbrev=0"]).map_err(|e| {
        format!("no tag is reachable from HEAD, so there is no release to start from ({e})")
    })?;
    let range = format!("{tag}..HEAD");
    let log = git(
        workspace,
        &["log", "--no-merges", "--reverse", "--format=%s", &range],
    )?;
    Ok(log.lines().map(str::to_owned).collect())
}

/// The committer date of HEAD, so an entry needs no clock.
pub fn head_date(workspace: &Path) -> Result<String, String> {
    git(workspace, &["show", "-s", "--format=%cs", "HEAD"])
}

/// `cargo xtask check-commits`: every subject since the last tag that breaks the rules.
pub fn check(workspace: &Path) -> Result<Vec<String>, String> {
    Ok(problems(&subjects_since_last_tag(workspace)?))
}

/// `cargo xtask changelog [<version>]`: the entry the next release would write.
pub fn preview(workspace: &Path, version: Option<&str>) -> Result<String, String> {
    let subjects = subjects_since_last_tag(workspace)?;
    let date = head_date(workspace)?;
    let entry = entry(version.unwrap_or("Unreleased"), &date, &subjects)
        .map_err(|problems| problems.join("\n"))?;
    Ok(entry.render())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_become_an_entry_that_the_changelog_reads_back() {
        let subjects: Vec<String> = [
            "feat(chat): mention people with @name",
            "chore: bump a pin",
            "fix: stop drags from selecting text",
            "Revert \"feat: something we took back\"",
            "perf(voice): decode on the worker thread",
            "refactor!: rename the config key",
            "feat: open lobby:// links",
        ]
        .map(String::from)
        .to_vec();

        let next = entry("0.2.0", "2026-11-10", &subjects).unwrap();
        let text = prepend(
            "# Changelog\n\n## 0.1.3 (2026-11-09)\n\n### New\n\n- Old\n",
            &next,
        );
        let parsed = parse(&text.unwrap()).unwrap();

        let section = |heading: &str, lines: &[&str]| Section {
            heading: heading.into(),
            lines: lines.iter().map(|l| (*l).into()).collect(),
        };
        let expected = vec![
            section("Breaking", &["Rename the config key"]),
            section("New", &["Mention people with @name", "Open lobby:// links"]),
            section("Fixed", &["Stop drags from selecting text"]),
            section("Improved", &["Decode on the worker thread"]),
        ];
        assert_eq!(parsed[0].sections, expected);
        assert_eq!(
            (parsed[0].version.as_str(), parsed[0].date.as_str()),
            ("0.2.0", "2026-11-10")
        );
        assert_eq!(parsed[1].version, "0.1.3");

        let bad = ["fix: Stop it.", "added a thing", "feat(Chat): x", "wip: x"].map(String::from);
        assert_eq!(entry("0.2.0", "2026-11-10", &bad).unwrap_err().len(), 4);
    }
}

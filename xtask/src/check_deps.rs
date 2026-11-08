//! `cargo xtask check-deps`: checks the crate boundaries on the resolved `cargo metadata` graph.
//! Per-crate rules count normal dependencies only, followed transitively; dev-dependencies are
//! allowed and build-dependencies never reach the binary. aws-lc is refused over every kind of
//! edge: a second rustls provider in a test or build script is still a second provider.
//!
//! `lobby-desktop`'s `automation` and `gallery` features are for dev builds only. The crate
//! refuses to compile either without debug assertions; this check refuses what would ship them
//! anyway: the feature in the defaults, a normal dependency that turns it on, or a release
//! profile with debug assertions.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    path::PathBuf,
    process::Command,
};

use serde_json::Value;

type Pattern = &'static str;

pub struct Rule {
    pub krate: &'static str,
    pub forbidden: &'static [Pattern],
}

pub const RULES: &[Rule] = &[
    Rule {
        krate: "lobby-protocol",
        forbidden: &[
            "tokio*", "gpui*", "sqlx*", "axum*", "reqwest", "hyper*", "mio",
        ],
    },
    Rule {
        krate: "lobby-server",
        forbidden: &["gpui*", "cpal", "reqwest"],
    },
    Rule {
        krate: "lobby-client",
        forbidden: &["gpui*", "sqlx*", "axum*"],
    },
    Rule {
        krate: "lobby-desktop",
        forbidden: &["sqlx*", "axum*"],
    },
];

/// ring is the single rustls provider.
pub const AWS_LC: &[Pattern] = &["aws-lc-rs", "aws-lc-sys"];

#[derive(Debug, Default, Clone)]
pub struct Options {
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub krate: String,
    pub forbidden: String,
    pub path: Vec<String>,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} must not depend on {} (via {})",
            self.krate,
            self.forbidden,
            self.path.join(" -> ")
        )
    }
}

pub fn check(opts: &Options) -> Result<Vec<Violation>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["metadata", "--format-version", "1"]);
    if let Some(path) = &opts.manifest_path {
        cmd.arg("--manifest-path").arg(path);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let metadata: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("cargo metadata output is not JSON: {e}"))?;
    let mut found = violations(&metadata)?;
    let root = metadata["workspace_root"]
        .as_str()
        .ok_or("metadata has no workspace_root")?;
    let manifest = std::fs::read_to_string(PathBuf::from(root).join("Cargo.toml"))
        .map_err(|e| format!("cannot read the workspace manifest: {e}"))?;
    found.extend(release_profile_violations(&manifest));
    Ok(found)
}

pub const GUARDED: &[(&str, &str)] = &[
    ("lobby-desktop", "automation"),
    ("lobby-desktop", "gallery"),
];

/// A dev-only feature reaching a build that is not a dev one: in the package's defaults, or
/// turned on by a normal or build dependency (a dev-dependency, like the crate's own for its
/// tests, is fine).
pub fn guarded_violations(metadata: &Value) -> Vec<Violation> {
    let mut found = Vec::new();
    let Some(packages) = metadata["packages"].as_array() else {
        return found;
    };
    for (pkg, &(krate, feature)) in packages
        .iter()
        .flat_map(|pkg| GUARDED.iter().map(move |g| (pkg, g)))
    {
        let name = pkg["name"].as_str().unwrap_or_default();
        let by_default = pkg["features"]["default"]
            .as_array()
            .is_some_and(|d| d.iter().any(|f| f.as_str() == Some(feature)));
        if name == krate && by_default {
            found.push(Violation {
                krate: krate.to_owned(),
                forbidden: format!("the `{feature}` feature by default (dev builds only)"),
                path: vec![krate.to_owned()],
            });
        }
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            let turns_on = dep["name"].as_str() == Some(krate)
                && dep["features"]
                    .as_array()
                    .is_some_and(|f| f.iter().any(|f| f.as_str() == Some(feature)));
            if turns_on && dep["kind"].as_str() != Some("dev") {
                found.push(Violation {
                    krate: name.to_owned(),
                    forbidden: format!(
                        "{krate}'s `{feature}` feature through a {} dependency (dev builds only)",
                        dep["kind"].as_str().unwrap_or("normal")
                    ),
                    path: vec![name.to_owned(), krate.to_owned()],
                });
            }
        }
    }
    found
}

pub fn release_profile_violations(manifest: &str) -> Vec<Violation> {
    let mut section = String::new();
    let mut found = Vec::new();
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') {
            section = line
                .trim_matches(|c| c == '[' || c == ']')
                .trim()
                .to_owned();
            continue;
        }
        let release = section == "profile.release" || section.starts_with("profile.release.");
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if release && compact == "debug-assertions=true" {
            found.push(Violation {
                krate: "workspace".to_owned(),
                forbidden: format!(
                    "debug-assertions in [{section}]: the dev-only feature guards need them off"
                ),
                path: vec!["Cargo.toml".to_owned()],
            });
        }
    }
    found
}

pub fn violations(metadata: &Value) -> Result<Vec<Violation>, String> {
    let graph = Graph::parse(metadata)?;
    let mut found = Vec::new();
    for member in &graph.members {
        let name = graph.name(member);
        let forbidden: Vec<Pattern> = RULES
            .iter()
            .filter(|r| r.krate == name)
            .flat_map(|r| r.forbidden.iter().copied())
            .collect();
        found.extend(graph.find(member, Edges::Normal, &forbidden));
        found.extend(graph.find(member, Edges::All, AWS_LC));
    }
    found.extend(guarded_violations(metadata));
    Ok(found)
}

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

#[derive(Clone, Copy)]
enum Edges {
    Normal,
    All,
}

struct Graph {
    names: HashMap<String, String>,
    normal_deps: HashMap<String, Vec<String>>,
    all_deps: HashMap<String, Vec<String>>,
    members: Vec<String>,
}

impl Graph {
    fn parse(metadata: &Value) -> Result<Graph, String> {
        let names = metadata["packages"]
            .as_array()
            .ok_or("metadata has no packages")?
            .iter()
            .filter_map(|p| Some((p["id"].as_str()?.to_owned(), p["name"].as_str()?.to_owned())))
            .collect();
        let nodes = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("metadata has no resolve graph (was --no-deps used?)")?;
        let mut normal_deps = HashMap::new();
        let mut all_deps = HashMap::new();
        for node in nodes {
            let id = node["id"].as_str().ok_or("node without id")?.to_owned();
            let deps = node["deps"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            let pkgs = |keep: fn(&Value) -> bool| -> Vec<String> {
                deps.iter()
                    .filter(|d| keep(d))
                    .filter_map(|d| d["pkg"].as_str().map(str::to_owned))
                    .collect()
            };
            normal_deps.insert(id.clone(), pkgs(is_normal));
            all_deps.insert(id, pkgs(|_| true));
        }
        let members = metadata["workspace_members"]
            .as_array()
            .ok_or("metadata has no workspace_members")?
            .iter()
            .filter_map(|m| m.as_str().map(str::to_owned))
            .collect();
        Ok(Graph {
            names,
            normal_deps,
            all_deps,
            members,
        })
    }

    fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.names.get(id).map_or(id, String::as_str)
    }

    fn find(&self, member: &str, edges: Edges, forbidden: &[Pattern]) -> Vec<Violation> {
        let mut found = Vec::new();
        for (pkg, path) in self.closure(member, edges) {
            let pkg_name = self.name(&pkg);
            if let Some(pattern) = forbidden.iter().find(|p| matches(p, pkg_name)) {
                found.push(Violation {
                    krate: self.name(member).to_owned(),
                    forbidden: format!("{pkg_name} (rule {pattern:?})"),
                    path: path.iter().map(|id| self.name(id).to_owned()).collect(),
                });
            }
        }
        found
    }

    /// Every package reachable over `edges`, with the first path found to it.
    fn closure(&self, root: &str, edges: Edges) -> Vec<(String, Vec<String>)> {
        let deps = match edges {
            Edges::Normal => &self.normal_deps,
            Edges::All => &self.all_deps,
        };
        let mut seen = HashSet::from([root.to_owned()]);
        let mut queue = VecDeque::from([vec![root.to_owned()]]);
        let mut out = Vec::new();
        while let Some(path) = queue.pop_front() {
            let last = path.last().expect("paths are never empty");
            for dep in deps.get(last).into_iter().flatten() {
                if seen.insert(dep.clone()) {
                    let mut next = path.clone();
                    next.push(dep.clone());
                    out.push((dep.clone(), next.clone()));
                    queue.push_back(next);
                }
            }
        }
        out
    }
}

/// `dep_kinds[].kind` is `null` for a normal dependency, `"dev"` or `"build"` otherwise.
fn is_normal(dep: &Value) -> bool {
    dep["dep_kinds"]
        .as_array()
        .is_some_and(|kinds| kinds.iter().any(|k| k["kind"].is_null()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn dep(pkg: &str, kind: Option<&str>) -> Value {
        json!({"name": pkg, "pkg": pkg, "dep_kinds": [{"kind": kind, "target": null}]})
    }

    fn metadata(nodes: Vec<(&str, Vec<Value>)>, members: &[&str]) -> Value {
        json!({
            "packages": nodes.iter().map(|(id, _)| json!({"id": id, "name": id})).collect::<Vec<_>>(),
            "resolve": {"nodes": nodes.iter().map(|(id, deps)| json!({"id": id, "deps": deps})).collect::<Vec<_>>()},
            "workspace_members": members,
        })
    }

    #[test]
    fn aws_lc_is_refused_everywhere() {
        let m = metadata(
            vec![
                ("lobby-desktop", vec![dep("rustls", None)]),
                ("rustls", vec![dep("aws-lc-rs", None)]),
                ("aws-lc-rs", vec![]),
            ],
            &["lobby-desktop"],
        );
        assert_eq!(violations(&m).unwrap().len(), 1);
    }
}

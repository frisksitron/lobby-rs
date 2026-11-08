//! `cargo xtask check-api`: rules about what the crates export, read from rustdoc's JSON.
//!
//! A text search for `pub async fn` cannot tell an exported function from one on a `pub(crate)`
//! type, so the rules read the items rustdoc keeps in its public API. The JSON output is
//! unstable, so the build sets `RUSTC_BOOTSTRAP` and the toolchain in `rust-toolchain.toml` fixes
//! the format.

use std::{io, path::Path};

use serde_json::Value;

const CLIENT: &str = "lobby-client";
const FORMAT_VERSION: u64 = 57;

#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub krate: &'static str,
    pub item: String,
    pub problem: &'static str,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}: {}", self.krate, self.item, self.problem)
    }
}

pub fn check(workspace: &Path) -> io::Result<Vec<Finding>> {
    let json = rustdoc_json(workspace, CLIENT)?;
    async_fns(&json).map_err(io::Error::other).map(|names| {
        names
            .into_iter()
            .map(|item| Finding {
                krate: CLIENT,
                item,
                problem: "an exported `async fn`: spawn on the client's runtime and return a \
                          `ClientTask` or a channel",
            })
            .collect()
    })
}

fn rustdoc_json(workspace: &Path, package: &str) -> io::Result<Value> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    // Cargo reads a relative target dir from where it runs, which is the workspace. A target dir
    // of its own, because `RUSTC_BOOTSTRAP` makes the next clippy and test build recompile
    // every dependency in the shared one.
    let target = workspace
        .join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or("target".into()))
        .join("check-api");
    let status = crate::workspace_cargo(cargo)
        .current_dir(workspace)
        .env("RUSTC_BOOTSTRAP", "1")
        .env("CARGO_TARGET_DIR", &target)
        .args([
            "rustdoc",
            "--quiet",
            "-p",
            package,
            "--",
            "-Z",
            "unstable-options",
        ])
        .args(["--output-format", "json"])
        // `#[doc(hidden)]` hides an item from the docs, not from callers.
        .arg("--document-hidden-items")
        .args(["-A", "warnings"])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "cargo rustdoc -p {package} failed"
        )));
    }
    let file = target
        .join("doc")
        .join(format!("{}.json", package.replace('-', "_")));
    serde_json::from_slice(&std::fs::read(&file)?)
        .map_err(|e| io::Error::other(format!("{}: {e}", file.display())))
}

/// The `path::name` of each async function in the crate's public API. Fails on a JSON shape this
/// code does not know, so a format change cannot turn the rule into one that passes on anything.
fn async_fns(doc: &Value) -> Result<Vec<String>, String> {
    let version = doc["format_version"].as_u64();
    if version != Some(FORMAT_VERSION) {
        return Err(format!(
            "rustdoc JSON format {version:?}, expected {FORMAT_VERSION}: update check_api.rs"
        ));
    }
    let index = doc["index"]
        .as_object()
        .ok_or("rustdoc JSON has no index")?;
    let mut functions = 0;
    let mut found = Vec::new();
    for item in index.values() {
        let Some(function) = item["inner"].get("function") else {
            continue;
        };
        functions += 1;
        let is_async = function["header"]["is_async"]
            .as_bool()
            .ok_or("a function has no header.is_async")?;
        if is_async {
            let name = item["name"].as_str().unwrap_or("?");
            let file = item["span"]["filename"].as_str().unwrap_or("?");
            found.push(format!("{file}: fn {name}"));
        }
    }
    if functions == 0 {
        return Err("rustdoc JSON lists no functions: the crate was not read".into());
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn function(name: &str, is_async: bool) -> Value {
        json!({"name": name, "span": {"filename": "src/a.rs"},
               "inner": {"function": {"header": {"is_async": is_async}}}})
    }

    #[test]
    fn an_async_function_is_found_and_a_changed_format_is_refused() {
        let doc = json!({"format_version": FORMAT_VERSION,
            "index": {"1": function("run", true), "2": function("walk", false)}});
        assert_eq!(async_fns(&doc), Ok(vec!["src/a.rs: fn run".to_owned()]));
        let empty = json!({"format_version": FORMAT_VERSION, "index": {}});
        assert!(async_fns(&empty).is_err());
        let newer = json!({"format_version": FORMAT_VERSION + 1, "index": {}});
        assert!(async_fns(&newer).is_err());
    }
}

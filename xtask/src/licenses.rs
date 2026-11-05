//! The third-party license notices a release ships.
//!
//! The crates linked into `deckly` and `deckly-web` for one target come from
//! `cargo metadata`; each crate's own license files are copied from its
//! source folder. Identical texts are written once.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::cargo;

/// The workspace packages that end up in a release.
const SHIPPED: [&str; 2] = ["deckly", "deckly-web"];

pub fn third_party(root: &Path, target: &str) -> Result<String, String> {
    let output = Command::new(cargo())
        .args(["metadata", "--format-version", "1", "--locked"])
        .args(["--filter-platform", target])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cannot read cargo metadata: {e}"))?;
    let crates = shipped_crates(&metadata)?;
    Ok(render(&crates))
}

/// One third-party crate and its license files.
struct Crate {
    name: String,
    version: String,
    license: String,
    repository: String,
    texts: Vec<String>,
}

/// The registry crates reachable from [`SHIPPED`] through normal (not dev or
/// build) dependencies.
fn shipped_crates(metadata: &Value) -> Result<Vec<Crate>, String> {
    let packages: BTreeMap<&str, &Value> = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no dependency graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();

    let mut queue: VecDeque<&str> = packages
        .iter()
        .filter(|(_, p)| {
            p["source"].is_null() && SHIPPED.contains(&p["name"].as_str().unwrap_or(""))
        })
        .map(|(id, _)| *id)
        .collect();
    if queue.len() != SHIPPED.len() {
        return Err("cargo metadata is missing deckly or deckly-web".to_owned());
    }
    let mut seen = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        let deps = nodes.get(id).and_then(|n| n["deps"].as_array());
        for dep in deps.into_iter().flatten() {
            let normal = dep["dep_kinds"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|k| k["kind"].is_null()));
            if let (true, Some(pkg)) = (normal, dep["pkg"].as_str()) {
                queue.push_back(pkg);
            }
        }
    }

    let mut crates: Vec<Crate> = seen
        .into_iter()
        .filter_map(|id| packages.get(id))
        .filter(|p| !p["source"].is_null())
        .map(|p| {
            let text = |key: &str| p[key].as_str().unwrap_or("").to_owned();
            let dir = p["manifest_path"]
                .as_str()
                .and_then(|m| Path::new(m).parent());
            Crate {
                name: text("name"),
                version: text("version"),
                license: p["license"].as_str().unwrap_or("not stated").to_owned(),
                repository: text("repository"),
                texts: dir.map(license_texts).unwrap_or_default(),
            }
        })
        .collect();
    crates.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(crates)
}

/// Whether a file in a crate's folder holds license or notice text.
fn is_license_file(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "COPYRIGHT",
        "NOTICE",
        "UNLICENSE",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

/// The license texts in a crate folder, including a REUSE `LICENSES` folder.
fn license_texts(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut add = |dir: &Path, all: bool| {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_file() && (all || is_license_file(&name)) {
                files.push(path);
            }
        }
    };
    add(dir, false);
    add(&dir.join("LICENSES"), true);
    files.sort();
    files
        .iter()
        .filter_map(|f| fs::read(f).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect()
}

fn render(crates: &[Crate]) -> String {
    let mut out = String::from(
        "Third-party software in Deckly\n\
         ==============================\n\n\
         Deckly is MIT licensed (see LICENSE). Its programs include the\n\
         following open source crates, under the licenses stated for each.\n\
         Their license texts follow the list.\n\n",
    );
    let mut by_text: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for c in crates {
        let label = format!("{} {}", c.name, c.version);
        out.push_str(&format!("- {label} ({})", c.license));
        if !c.repository.is_empty() {
            out.push_str(&format!(" {}", c.repository));
        }
        if c.texts.is_empty() {
            out.push_str(
                " [the package ships no license file; standard texts: https://spdx.org/licenses/]",
            );
        }
        out.push('\n');
        for text in &c.texts {
            by_text.entry(text).or_default().push(label.clone());
        }
    }
    for (text, users) in by_text {
        out.push_str("\n\n");
        out.push_str(&"-".repeat(72));
        out.push_str(&format!("\nUsed by: {}\n\n", users.join(", ")));
        out.push_str(text);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn license_files_are_recognised_by_name() {
        for name in [
            "LICENSE",
            "LICENSE-MIT",
            "license-apache",
            "COPYING",
            "NOTICE.md",
            "UNLICENSE",
        ] {
            assert!(is_license_file(name), "{name}");
        }
        for name in ["README.md", "Cargo.toml", "src"] {
            assert!(!is_license_file(name), "{name}");
        }
    }

    #[test]
    fn only_normal_dependencies_of_shipped_packages_are_listed() {
        let pkg = |id: &str, source: Value| {
            json!({"id": id, "name": id, "version": "1.0.0", "license": "MIT",
                   "source": source, "manifest_path": "/nonexistent/Cargo.toml"})
        };
        let reg = json!("registry+https://github.com/rust-lang/crates.io-index");
        let dep = |pkg: &str, kind: Value| json!({"pkg": pkg, "dep_kinds": [{"kind": kind}]});
        let metadata = json!({
            "packages": [
                pkg("deckly", Value::Null), pkg("deckly-web", Value::Null),
                pkg("deckly-core", Value::Null), pkg("xtask", Value::Null),
                pkg("shipped", reg.clone()), pkg("nested", reg.clone()),
                pkg("devonly", reg.clone()), pkg("buildonly", reg.clone()),
                pkg("toolonly", reg),
            ],
            "resolve": {"nodes": [
                {"id": "deckly", "deps": [
                    dep("deckly-core", Value::Null), dep("devonly", json!("dev")),
                    dep("buildonly", json!("build")),
                ]},
                {"id": "deckly-core", "deps": [dep("shipped", Value::Null)]},
                {"id": "shipped", "deps": [dep("nested", Value::Null)]},
                {"id": "deckly-web", "deps": []},
                {"id": "xtask", "deps": [dep("toolonly", Value::Null)]},
            ]},
        });
        let names: Vec<String> = shipped_crates(&metadata)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["nested", "shipped"]);
    }

    #[test]
    fn identical_license_texts_are_written_once() {
        let make = |name: &str| Crate {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            license: "MIT".to_owned(),
            repository: String::new(),
            texts: vec!["MIT License text".to_owned()],
        };
        let text = render(&[make("a"), make("b")]);
        assert_eq!(text.matches("MIT License text").count(), 1);
        assert!(text.contains("Used by: a 1.0.0, b 1.0.0"));
    }
}

//! Application bundles in the user, local and system Applications folders.

use super::{Application, Catalog, walk};
use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub(super) fn discover() -> Catalog {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots.extend(["/Applications", "/System/Applications"].map(PathBuf::from));
    discover_in(&roots)
}

fn discover_in(roots: &[PathBuf]) -> Catalog {
    let mut catalog = Catalog::default();
    let mut budget = 20_000;
    for root in roots {
        walk(
            root,
            &mut |path| {
                if path
                    .extension()
                    .is_none_or(|e| !e.eq_ignore_ascii_case("app"))
                {
                    return true;
                }
                if path.is_dir()
                    && let (Some(name), Some(target)) =
                        (path.file_stem().and_then(|v| v.to_str()), path.to_str())
                {
                    catalog.add(Application {
                        name: name.into(),
                        target: target.into(),
                        detail: path
                            .parent()
                            .map_or_else(String::new, |p| p.display().to_string()),
                        keywords: String::new(),
                    });
                }
                // Do not expose helper applications inside a bundle.
                false
            },
            &mut budget,
        );
    }
    catalog.truncated |= budget == 0;
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_utilities_but_does_not_descend_into_bundles() {
        let root = std::env::temp_dir().join(format!("deckly-bundles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Music Player.app/Contents/Helper.app")).unwrap();
        std::fs::create_dir_all(root.join("Utilities/Terminal.app")).unwrap();
        let c = discover_in(std::slice::from_ref(&root));
        assert_eq!(c.applications.len(), 2);
        assert!(c.applications.iter().any(|a| a.name == "Music Player"));
        assert!(c.applications.iter().any(|a| a.name == "Terminal"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

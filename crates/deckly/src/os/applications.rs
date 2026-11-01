//! Installed application discovery. Scanned only when the picker opens, on
//! its worker thread; discovery never starts an application.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(windows)]
mod windows;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Application {
    pub name: String,
    /// An Open action target: desktop file, app bundle or Shell parsing name.
    pub target: String,
    pub detail: String,
    pub keywords: String,
}

impl Application {
    pub fn matches(&self, query: &str) -> bool {
        let text = format!("{} {} {}", self.name, self.detail, self.keywords).to_lowercase();
        query.split_whitespace().all(|word| text.contains(word))
    }
}

#[derive(Default)]
pub struct Catalog {
    pub applications: Vec<Application>,
    pub truncated: bool,
}

impl Catalog {
    fn add(&mut self, mut app: Application) {
        if app.name.trim().is_empty()
            || app.name.len() > 2048
            || app.target.is_empty()
            || app.target.len() > 32 * 1024
            || app.target.contains('\0')
        {
            return;
        }
        if self.applications.len() >= MAX_APPLICATIONS {
            self.truncated = true;
            return;
        }
        app.detail = app.detail.chars().take(512).collect();
        app.keywords = app.keywords.chars().take(1024).collect();
        self.applications.push(app);
    }
}

pub const MAX_APPLICATIONS: usize = 10_000;

pub fn discover() -> Result<Catalog, String> {
    #[cfg(target_os = "linux")]
    let mut catalog = linux::discover();
    #[cfg(target_os = "macos")]
    let mut catalog = macos::discover();
    #[cfg(windows)]
    let mut catalog = windows::discover()?;
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    return Err("Installed application discovery is unsupported on this system.".into());

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        catalog
            .applications
            .sort_by_cached_key(|app| (app.name.to_lowercase(), app.target.clone()));
        let mut seen = std::collections::HashSet::new();
        catalog
            .applications
            .retain(|app| seen.insert(app.target.clone()));
        catalog.truncated |= catalog.applications.len() > MAX_APPLICATIONS;
        catalog.applications.truncate(MAX_APPLICATIONS);
        Ok(catalog)
    }
}

/// Bounded traversal of desktop integration directories. Directory symlinks
/// are not followed; file symlinks are allowed (Flatpak exports use them).
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn walk(
    root: &std::path::Path,
    visit: &mut impl FnMut(&std::path::Path) -> bool,
    budget: &mut usize,
) {
    fn descend(
        dir: &std::path::Path,
        depth: usize,
        visit: &mut impl FnMut(&std::path::Path) -> bool,
        budget: &mut usize,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            let path = entry.path();
            if visit(&path) && depth < 5 && entry.file_type().is_ok_and(|t| t.is_dir()) {
                descend(&path, depth + 1, visit, budget);
            }
        }
    }
    descend(root, 0, visit, budget);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_all_words_in_name_and_metadata() {
        let app = Application {
            name: "Spotify (Launcher)".into(),
            target: "/apps/spotify.desktop".into(),
            detail: "Music Player".into(),
            keywords: "Audio;streaming;".into(),
        };
        assert!(app.matches("spotify"));
        assert!(app.matches("music spotify"));
        assert!(app.matches("streaming"));
        assert!(!app.matches("spotify video"));
    }

    #[test]
    fn catalog_bounds_metadata_and_counts_before_retaining_entries() {
        let mut catalog = Catalog::default();
        let app = Application {
            name: "Music".into(),
            target: "/music.desktop".into(),
            detail: "d".repeat(5000),
            keywords: "k".repeat(5000),
        };
        for _ in 0..=MAX_APPLICATIONS {
            catalog.add(app.clone());
        }
        assert_eq!(catalog.applications.len(), MAX_APPLICATIONS);
        assert!(catalog.truncated);
        assert_eq!(catalog.applications[0].detail.len(), 512);
        assert_eq!(catalog.applications[0].keywords.len(), 1024);
    }
}

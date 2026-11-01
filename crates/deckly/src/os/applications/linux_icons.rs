//! XDG icon-theme lookup, including user overrides, inheritance, hicolor and
//! unthemed pixmaps. Metadata is read once on the icon worker, never in UI.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_INDEX: u64 = 256 * 1024;
const MAX_DIRECTORIES: usize = 8192;

pub(super) struct Resolver {
    directories: Vec<PathBuf>,
    unthemed: Vec<PathBuf>,
}

impl Resolver {
    pub fn new() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|p| p.join(".local/share")));
        let mut bases = Vec::new();
        if let Some(data) = data {
            bases.push(data);
        }
        let dirs = std::env::var_os("XDG_DATA_DIRS")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        bases.extend(
            std::env::split_paths(&dirs)
                .filter(|p| p.is_absolute())
                .take(64),
        );
        if let Some(home) = &home {
            bases.push(home.join(".local/share/flatpak/exports/share"));
        }
        bases.push("/var/lib/flatpak/exports/share".into());
        bases.push("/var/lib/snapd/desktop".into());
        let mut roots = Vec::new();
        if let Some(home) = &home {
            roots.push(home.join(".icons"));
        }
        roots.extend(bases.iter().map(|base| base.join("icons")));
        let mut seen = HashSet::new();
        roots.retain(|p| seen.insert(p.clone()));
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|p| p.join(".config")));
        let theme = config
            .and_then(|root| current_theme(&root))
            .unwrap_or_else(|| "default".into());
        let mut resolver = Self::from_roots(&roots, &theme);
        resolver
            .unthemed
            .extend(bases.into_iter().map(|base| base.join("pixmaps")));
        resolver
    }

    fn from_roots(roots: &[PathBuf], theme: &str) -> Self {
        fn append(
            theme: &str,
            roots: &[PathBuf],
            seen: &mut HashSet<String>,
            dirs: &mut Vec<PathBuf>,
        ) {
            if seen.len() >= 16
                || dirs.len() >= MAX_DIRECTORIES
                || !safe_component(theme)
                || !seen.insert(theme.into())
            {
                return;
            }
            let mut inherited = Vec::new();
            let mut candidates = Vec::new();
            for (priority, root) in roots.iter().enumerate() {
                let path = root.join(theme);
                let Some(text) = metadata(&path.join("index.theme")) else {
                    for (order, sub) in [
                        "64x64/apps",
                        "48x48/apps",
                        "32x32/apps",
                        "128x128/apps",
                        "256x256/apps",
                        "512x512/apps",
                        "scalable/apps",
                        "24x24/apps",
                        "16x16/apps",
                    ]
                    .iter()
                    .enumerate()
                    {
                        let dir = path.join(sub);
                        if dir.is_dir() && candidates.len() + dirs.len() < MAX_DIRECTORIES {
                            candidates.push((order as i32, priority, dir));
                        }
                    }
                    continue;
                };
                let fields = ini(&text);
                let get =
                    |section: &str, key: &str| fields.get(&(section, key)).copied().unwrap_or("");
                inherited.extend(
                    get("Icon Theme", "Inherits")
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .take(32)
                        .map(str::to_owned),
                );
                for sub in get("Icon Theme", "Directories")
                    .split(',')
                    .chain(get("Icon Theme", "ScaledDirectories").split(','))
                    .map(str::trim)
                    .take(4096)
                {
                    if candidates.len() + dirs.len() >= MAX_DIRECTORIES {
                        break;
                    }
                    if !safe_subdirectory(sub) {
                        continue;
                    }
                    let directory = path.join(sub);
                    if !directory.is_dir() {
                        continue;
                    }
                    let number = |key, default| {
                        get(sub, key)
                            .parse::<i32>()
                            .unwrap_or(default)
                            .clamp(1, 4096)
                    };
                    let size = number("Size", 64);
                    let scale = number("Scale", 1).min(4);
                    let (low, high) = match get(sub, "Type") {
                        "Scalable" => (number("MinSize", size), number("MaxSize", size)),
                        "Fixed" => (size, size),
                        _ => (
                            size.saturating_sub(number("Threshold", 2)),
                            size.saturating_add(number("Threshold", 2)),
                        ),
                    };
                    let distance = (low * scale - 64).max(64 - high * scale).max(0);
                    candidates.push((distance, priority, directory));
                }
            }
            candidates.sort_by_key(|(distance, priority, _)| (*distance, *priority));
            dirs.extend(candidates.into_iter().map(|(_, _, path)| path));
            for parent in inherited {
                // hicolor must remain the final theme, even when an earlier
                // parent's index names it before another inherited theme.
                if parent != "hicolor" {
                    append(&parent, roots, seen, dirs);
                }
            }
        }
        let mut directories = Vec::new();
        let mut seen = HashSet::new();
        if theme != "hicolor" {
            append(theme, roots, &mut seen, &mut directories);
        }
        // A separate visit budget ensures hicolor survives a cyclic/deep theme.
        append("hicolor", roots, &mut HashSet::new(), &mut directories);
        Self {
            directories,
            unthemed: roots.to_vec(),
        }
    }

    pub fn find(&mut self, name: &str) -> Option<PathBuf> {
        let path = Path::new(name);
        if path.is_absolute() {
            return Some(path.into());
        }
        if !safe_component(name) {
            return None;
        }
        let names = if path
            .extension()
            .is_some_and(|ext| ext == "png" || ext == "svg")
        {
            vec![name.to_owned()]
        } else {
            vec![format!("{name}.png"), format!("{name}.svg")]
        };
        for directory in self.directories.iter().chain(&self.unthemed) {
            for name in &names {
                let path = directory.join(name);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        None
    }
}

fn safe_component(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 255
        && !text.contains(['/', '\\', '\0'])
        && text != "."
        && text != ".."
}

fn safe_subdirectory(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 512
        && Path::new(text)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn metadata(path: &Path) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_INDEX + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= MAX_INDEX).then_some(text)
}

fn ini(text: &str) -> HashMap<(&str, &str), &str> {
    let mut fields = HashMap::new();
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            section = name;
        } else if let Some((key, value)) = line.split_once('=') {
            fields.insert((section, key.trim()), value.trim().trim_matches('"'));
        }
    }
    fields
}

fn current_theme(config: &Path) -> Option<String> {
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|v| v.split(':').any(|d| d == "KDE"));
    let mut sources = vec![
        ("gtk-3.0/settings.ini", "Settings", "gtk-icon-theme-name"),
        ("gtk-4.0/settings.ini", "Settings", "gtk-icon-theme-name"),
    ];
    let kde_source = ("kdeglobals", "Icons", "Theme");
    if kde {
        sources.insert(0, kde_source);
    } else {
        sources.push(kde_source);
    }
    sources.into_iter().find_map(|(file, section, key)| {
        let text = metadata(&config.join(file))?;
        let fields = ini(&text);
        fields
            .get(&(section, key))
            .filter(|value| safe_component(value))
            .map(|value| (*value).to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn theme(root: &Path, name: &str, inherits: &str) {
        std::fs::create_dir_all(root.join(name).join("64x64/apps")).unwrap();
        std::fs::write(root.join(name).join("index.theme"), format!("[Icon Theme]\nDirectories=64x64/apps\nInherits={inherits}\n[64x64/apps]\nSize=64\nType=Fixed\n")).unwrap();
    }
    #[test]
    fn user_overrides_inheritance_cycles_hicolor_and_absolute_paths() {
        let dir = std::env::temp_dir().join(format!("deckly-themes-{}", uuid::Uuid::new_v4()));
        let user = dir.join("user");
        let system = dir.join("system");
        theme(&user, "custom", "parent,hicolor");
        theme(&system, "custom", "parent,hicolor");
        theme(&system, "parent", "custom");
        theme(&system, "hicolor", "");
        for (root, theme, name) in [
            (&user, "custom", "music"),
            (&system, "parent", "terminal"),
            (&system, "hicolor", "browser"),
        ] {
            std::fs::write(
                root.join(theme)
                    .join("64x64/apps")
                    .join(format!("{name}.png")),
                b"fixture",
            )
            .unwrap();
        }
        std::fs::write(
            system.join("custom/64x64/apps/music.png"),
            b"system fixture",
        )
        .unwrap();
        let mut resolver = Resolver::from_roots(&[user.clone(), system.clone()], "custom");
        assert_eq!(
            resolver.find("music"),
            Some(user.join("custom/64x64/apps/music.png"))
        );
        assert_eq!(
            resolver.find("terminal"),
            Some(system.join("parent/64x64/apps/terminal.png"))
        );
        assert_eq!(
            resolver.find("browser.png"),
            Some(system.join("hicolor/64x64/apps/browser.png"))
        );
        assert_eq!(
            resolver.find("/absolute/icon.svg"),
            Some("/absolute/icon.svg".into())
        );
        assert!(resolver.find("../outside").is_none());
        assert!(resolver.find("missing").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

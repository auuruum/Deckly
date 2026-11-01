//! XDG desktop entries, including exported Flatpak and Snap launchers.
//! Execution stays with GIO, which handles Exec field codes, D-Bus activation
//! and terminal applications instead of Deckly reinterpreting their commands.

use super::{Application, Catalog, walk};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_ENTRY_BYTES: u64 = 256 * 1024;

pub(super) fn discover() -> Catalog {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let mut roots = Vec::new();
    if let Some(data) = &data_home {
        roots.push(data.join("applications"));
    }
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    roots.extend(
        std::env::split_paths(&dirs)
            .filter(|p| p.is_absolute())
            .map(|p| p.join("applications")),
    );
    // Exports may be installed after login, before XDG_DATA_DIRS is refreshed.
    if let Some(home) = &home {
        roots.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    roots.push("/var/lib/flatpak/exports/share/applications".into());
    roots.push("/var/lib/snapd/desktop/applications".into());
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let path = std::env::var_os("PATH").unwrap_or_default();
    discover_in(&roots, &locale, &desktops, &|program| {
        executable(program, &path)
    })
}

fn executable(program: &str, search: &std::ffi::OsStr) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let check = |path: &Path| {
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let program = Path::new(program);
    if program.is_absolute() {
        return check(program);
    }
    std::env::split_paths(search).any(|dir| check(&dir.join(program)))
}

fn discover_in(
    roots: &[PathBuf],
    locale: &str,
    desktops: &str,
    available: &impl Fn(&str) -> bool,
) -> Catalog {
    let mut catalog = Catalog::default();
    let mut seen_roots = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut budget = 20_000;
    for root in roots {
        if !seen_roots.insert(root) {
            continue;
        }
        walk(
            root,
            &mut |path| {
                if path.extension().is_none_or(|e| e != "desktop") {
                    return true;
                }
                let Ok(relative) = path.strip_prefix(root) else {
                    return false;
                };
                let id = relative.to_string_lossy().replace('/', "-");
                // Even Hidden=true overrides entries in lower priority roots.
                if !seen_ids.insert(id) {
                    return false;
                }
                if let Some(text) = read_entry(path)
                    && let Some(app) = parse(&text, path, locale, desktops, available)
                {
                    catalog.add(app);
                }
                false
            },
            &mut budget,
        );
    }
    catalog.truncated |= budget == 0;
    catalog
}

fn read_entry(path: &Path) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt;
    // NONBLOCK prevents a hostile FIFO masquerading as a desktop file from
    // trapping the discovery worker. File symlinks remain valid exports.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_ENTRY_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_ENTRY_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_ENTRY_BYTES).then_some(text)
}

fn parse(
    text: &str,
    path: &Path,
    locale: &str,
    desktops: &str,
    available: &impl Fn(&str) -> bool,
) -> Option<Application> {
    let mut fields = HashMap::new();
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry && let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim(), value.trim());
        }
    }
    let get = |key: &str| fields.get(key).copied().unwrap_or("");
    if get("Type") != "Application" || get("Hidden") == "true" || get("NoDisplay") == "true" {
        return None;
    }
    let contains_desktop = |list: &str| {
        list.split(';')
            .filter(|v| !v.is_empty())
            .any(|d| desktops.split(':').any(|current| current == d))
    };
    if (!get("OnlyShowIn").is_empty() && !contains_desktop(get("OnlyShowIn")))
        || contains_desktop(get("NotShowIn"))
        || (!get("TryExec").is_empty() && !available(get("TryExec")))
        || (get("Exec").is_empty() && get("DBusActivatable") != "true")
    {
        return None;
    }
    let localized = |key: &str| {
        locales(locale)
            .iter()
            .find_map(|locale| fields.get(format!("{key}[{locale}]").as_str()).copied())
            .unwrap_or_else(|| get(key))
            .to_owned()
    };
    let name = display_text(&localized("Name"));
    if name.is_empty() {
        return None;
    }
    let generic = display_text(&localized("GenericName"));
    let source = if path.to_string_lossy().contains("/flatpak/") {
        "Flatpak application"
    } else if path.starts_with("/var/lib/snapd") {
        "Snap application"
    } else {
        "Installed application"
    };
    Some(Application {
        name,
        target: path.to_str()?.to_owned(),
        detail: if generic.is_empty() {
            source.into()
        } else {
            format!("{generic} · {source}")
        },
        keywords: format!(
            "{} {} {}",
            localized("Keywords"),
            get("Exec"),
            get("Comment")
        ),
    })
}

fn display_text(raw: &str) -> String {
    let mut text = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        let c = if c == '\\' {
            match chars.next() {
                Some('s' | 'n' | 't' | 'r') => ' ',
                Some(c) => c,
                None => '\\',
            }
        } else {
            c
        };
        text.push(if c.is_control() { ' ' } else { c });
        if text.len() > 2048 {
            break;
        }
    }
    text.trim().to_owned()
}

fn locales(locale: &str) -> Vec<String> {
    let (base, modifier) = locale.split_once('@').unwrap_or((locale, ""));
    let base = base.split('.').next().unwrap_or("");
    let language = base.split('_').next().unwrap_or("");
    let mut result = Vec::new();
    if !modifier.is_empty() {
        result.push(format!("{base}@{modifier}"));
    }
    result.push(base.into());
    if language != base {
        if !modifier.is_empty() {
            result.push(format!("{language}@{modifier}"));
        }
        result.push(language.into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName=Spotify\nExec=spotify %U\n{extra}")
    }
    fn app(text: &str) -> Option<Application> {
        parse(
            text,
            Path::new("/apps/spotify.desktop"),
            "ru_RU.UTF-8",
            "Hyprland",
            &|p| p == "spotify",
        )
    }

    #[test]
    fn localized_names_keywords_and_action_sections() {
        let a = app(&entry("Name[ru]=Музыка\nGenericName=Music\\sPlayer\nKeywords=Audio;\n[Desktop Action Other]\nName=Wrong\nExec=wrong")).unwrap();
        assert_eq!(a.name, "Музыка");
        assert!(a.detail.starts_with("Music Player"));
        assert!(a.matches("audio"));
        assert_eq!(a.target, "/apps/spotify.desktop");
        assert_eq!(
            locales("sr_RS.UTF-8@latin"),
            ["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
    }

    #[test]
    fn hides_non_apps_unavailable_and_invisible_entries() {
        for extra in [
            "Hidden=true",
            "NoDisplay=true",
            "TryExec=missing",
            "OnlyShowIn=KDE;GNOME;",
            "NotShowIn=Hyprland;",
            "Type=Link",
            "Name=",
        ] {
            assert!(app(&entry(extra)).is_none(), "{extra}");
        }
        assert!(app(&entry("TryExec=spotify\nOnlyShowIn=Hyprland;")).is_some());
        assert!(
            app("[Desktop Entry]\nType=Application\nName=Bus app\nDBusActivatable=true").is_some()
        );
        assert!(app("[Desktop Entry]\nType=Application\nName=No launch target").is_none());
    }

    #[test]
    fn user_overrides_nested_ids_symlinks_and_bounds() {
        let dir = std::env::temp_dir().join(format!("deckly-apps-{}", uuid::Uuid::new_v4()));
        let user = dir.join("user");
        let system = dir.join("system");
        std::fs::create_dir_all(user.join("vendor")).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(user.join("spotify.desktop"), entry("Hidden=true")).unwrap();
        std::fs::write(system.join("spotify.desktop"), entry("")).unwrap();
        std::fs::write(user.join("vendor/music.desktop"), entry("Name=User music")).unwrap();
        std::fs::write(
            system.join("vendor-music.desktop"),
            entry("Name=System music"),
        )
        .unwrap();
        std::fs::write(
            user.join("large.desktop"),
            "x".repeat(MAX_ENTRY_BYTES as usize + 1),
        )
        .unwrap();
        std::os::unix::fs::symlink(&system, user.join("loop")).unwrap();
        std::fs::write(dir.join("flatpak.desktop"), entry("Name=Flatpak music")).unwrap();
        std::os::unix::fs::symlink(dir.join("flatpak.desktop"), user.join("export.desktop"))
            .unwrap();
        let c = discover_in(&[user, system], "", "", &|_| true);
        assert_eq!(c.applications.len(), 2);
        assert!(c.applications.iter().any(|a| a.name == "User music"));
        assert!(c.applications.iter().any(|a| a.name == "Flatpak music"));
        assert!(!c.truncated);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "reads this desktop's installed applications"]
    fn installed_spotify_is_discovered() {
        let c = discover();
        assert!(
            c.applications
                .iter()
                .any(|app| app.name.to_lowercase().contains("spotify"))
        );
        eprintln!(
            "Found {} applications, including Spotify",
            c.applications.len()
        );
    }
}

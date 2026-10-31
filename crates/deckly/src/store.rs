//! Where Deckly keeps its files: one JSON file per profile plus a small
//! settings file in the per-user configuration folder.

use deckly_core::{Profile, Settings};
use image::RgbaImage;
use std::path::{Path, PathBuf};

/// Settings files larger than this are ignored.
const MAX_SETTINGS_BYTES: u64 = 4 * 1024 * 1024;

pub fn config_dir() -> Option<PathBuf> {
    let env = |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        env("APPDATA")?
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Application Support")
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))?
    };
    Some(base.join(if cfg!(any(windows, target_os = "macos")) {
        "Deckly"
    } else {
        "deckly"
    }))
}

fn profiles_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("profiles"))
}

/// Loads every profile; damaged files are skipped and reported.
pub fn load_profiles() -> (Vec<Profile>, Vec<String>) {
    let mut profiles = Vec::new();
    let mut errors = Vec::new();
    let Some(dir) = profiles_dir() else {
        return (profiles, errors);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (profiles, errors);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        match Profile::load(&path) {
            Ok(p) if profiles.iter().any(|q: &Profile| q.id == p.id) => {
                errors.push(format!("{}: duplicate profile id", path.display()));
            }
            Ok(p) => profiles.push(p),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    profiles.sort_by_key(|p| p.name.to_lowercase());
    (profiles, errors)
}

fn profile_path(id: &str) -> Option<PathBuf> {
    // Ids are UUIDs we generated; anything else is not used as a file name.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some(profiles_dir()?.join(format!("{id}.json")))
}

pub fn save_profile(profile: &Profile) -> Result<(), String> {
    let path = profile_path(&profile.id).ok_or("no place to save profiles")?;
    profile.save(&path).map_err(|e| e.to_string())
}

pub fn delete_profile(id: &str) -> Result<(), String> {
    let path = profile_path(id).ok_or("invalid profile id")?;
    // Imported and restored pictures live in a folder per profile.
    if let Some(dir) = config_dir().map(|d| d.join("images").join(id)) {
        let _ = std::fs::remove_dir_all(dir);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn screensavers_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("screensavers"))
}

/// A file name for a device serial: its letters and digits, or a hash when
/// it has none.
fn serial_file_name(serial: &str) -> String {
    let clean: String = serial
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(64)
        .collect();
    if clean.is_empty() {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut h = DefaultHasher::new();
        serial.hash(&mut h);
        format!("device-{:016x}", h.finish())
    } else {
        clean
    }
}

/// Saves a device's screensaver picture as a PNG; returns its path.
pub fn save_screensaver(serial: &str, image: &RgbaImage) -> Result<PathBuf, String> {
    let dir = screensavers_dir().ok_or("no configuration folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.png", serial_file_name(serial)));
    image.save(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Deletes a screensaver picture Deckly saved; other files are left alone.
pub fn delete_screensaver(path: &Path) {
    let ours = screensavers_dir().is_some_and(|dir| path.parent() == Some(dir.as_path()));
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// Loads the settings; a missing file gives the defaults, a damaged one the
/// defaults and an error to show.
pub fn load_settings() -> (Settings, Option<String>) {
    let Some(path) = config_dir().map(|d| d.join("settings.json")) else {
        return (Settings::default(), None);
    };
    let read = || -> Result<Option<Settings>, String> {
        let len = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if len > MAX_SETTINGS_BYTES {
            return Err("file is too large".into());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        Settings::from_json(&text)
            .map(Some)
            .map_err(|e| e.to_string())
    };
    match read() {
        Ok(s) => (s.unwrap_or_default(), None),
        Err(e) => (
            Settings::default(),
            Some(format!("{}: {e}", path.display())),
        ),
    }
}

pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let dir = config_dir().ok_or("no configuration folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let tmp = dir.join("settings.json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, dir.join("settings.json")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_path_rejects_unsafe_ids() {
        assert!(profile_path("../evil").is_none());
        assert!(profile_path("").is_none());
        assert!(profile_path("a/b").is_none());
    }

    #[test]
    fn serial_file_names_are_safe() {
        assert_eq!(serial_file_name("A8RKA4212IEK6O"), "A8RKA4212IEK6O");
        assert_eq!(serial_file_name("../x/y"), "xy");
        assert!(serial_file_name("..//").starts_with("device-"));
        assert_eq!(serial_file_name(&"z".repeat(500)).len(), 64);
    }
}

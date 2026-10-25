//! Where Deckly keeps its files: one JSON file per profile plus a small
//! settings file in the per-user configuration folder.

use deckly_core::Profile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub brightness: u8,
    /// Device serial number -> profile id shown on that device.
    pub assignments: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            brightness: 70,
            assignments: BTreeMap::new(),
        }
    }
}

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
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn load_settings() -> Settings {
    config_dir()
        .and_then(|d| std::fs::read_to_string(d.join("settings.json")).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
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
    fn settings_tolerate_missing_fields() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.brightness, 70);
    }
}

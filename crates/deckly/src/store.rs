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
    // DECKLY_CONFIG_DIR runs Deckly against a throwaway folder (development).
    if let Some(dir) = env("DECKLY_CONFIG_DIR") {
        return Some(dir);
    }
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

/// Copies images as well as data, so deleting the original cannot break the copy.
pub fn save_profile_copy(profile: Profile) -> Result<Profile, String> {
    let root = config_dir().ok_or("no configuration folder")?;
    let copy = copy_pictures(profile, &root)?;
    if let Err(error) = save_profile(&copy) {
        let _ = std::fs::remove_dir_all(root.join("images").join(&copy.id));
        return Err(error);
    }
    Ok(copy)
}

fn copy_pictures(mut copy: Profile, root: &Path) -> Result<Profile, String> {
    let dir = root.join("images").join(&copy.id);
    let mut copied = std::collections::HashMap::new();
    let mut result = Ok(());
    copy.for_each_picture(|picture| {
        let Some(from) = picture.as_ref() else { return };
        if result.is_err() {
            return;
        }
        if let Some(to) = copied.get(from) {
            *picture = Some(PathBuf::clone(to));
            return;
        }
        let extension = from
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("image");
        let to = dir.join(format!("{}.{}", uuid::Uuid::new_v4(), extension));
        result = (|| {
            let metadata = std::fs::metadata(from).map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
                return Err("picture is too large or is not a file".to_owned());
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::copy(from, &to).map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_ok() {
            copied.insert(from.clone(), to.clone());
            *picture = Some(to);
        }
    });
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(dir);
        return Err(error);
    }
    Ok(copy)
}

/// Immutable pictures keep rendering caches and editor undo valid. Both
/// created and imported key images are owned by the profile and backed up.
pub fn save_key_image(profile: &str, image: &RgbaImage) -> Result<PathBuf, String> {
    let root = config_dir().ok_or("no configuration folder")?;
    write_key_image(&root, profile, image)
}

fn write_key_image(root: &Path, profile: &str, image: &RgbaImage) -> Result<PathBuf, String> {
    if image.width() == 0 || image.height() == 0 || image.width().max(image.height()) > 512 {
        return Err("invalid key image size".into());
    }
    write_new(root, profile, "png", |output| {
        image
            .write_to(output, image::ImageFormat::Png)
            .map_err(|e| e.to_string())
    })
}

/// Animated key images are kept as the file they came in, so their frames
/// and timing stay as they are.
pub fn save_key_animation(profile: &str, source: &Path) -> Result<PathBuf, String> {
    let root = config_dir().ok_or("no configuration folder")?;
    copy_key_animation(&root, profile, source)
}

fn copy_key_animation(root: &Path, profile: &str, source: &Path) -> Result<PathBuf, String> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| ["gif", "webp", "png", "apng"].contains(&e.as_str()))
        .ok_or("not an animated picture")?;
    let len = std::fs::metadata(source).map_err(|e| e.to_string())?.len();
    if len > MAX_ANIMATION_BYTES {
        return Err("file is too large".into());
    }
    let mut input = std::fs::File::open(source).map_err(|e| e.to_string())?;
    write_new(root, profile, &ext, |output| {
        // The file may grow while it is copied; copy no more than was checked.
        std::io::copy(&mut std::io::Read::take(&mut input, len), output)
            .map(drop)
            .map_err(|e| e.to_string())
    })
}

/// Largest animated key image kept.
const MAX_ANIMATION_BYTES: u64 = 32 * 1024 * 1024;

/// Writes a new, uniquely named file in a profile's images folder.
fn write_new(
    root: &Path,
    profile: &str,
    ext: &str,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<(), String>,
) -> Result<PathBuf, String> {
    if profile.is_empty()
        || !profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        || profile.len() > 128
    {
        return Err("invalid profile id".into());
    }
    let dir = root.join("images").join(profile);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
    let result = (|| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let mut output = std::io::BufWriter::new(file);
        write(&mut output)?;
        std::io::Write::flush(&mut output).map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
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
    fn a_duplicate_owns_page_folder_and_nested_action_pictures() {
        use deckly_core::{DeviceKind, KeyConfig, Page};
        let root = std::env::temp_dir().join(format!("deckly-copy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("original.svg");
        std::fs::write(&original, b"<svg/>").unwrap();
        let mut source = Profile::new("Scenes", DeviceKind::Mk2);
        source.pages[0].icon = Some(original.clone());
        let mut folder = Page::new();
        folder.icon = Some(original.clone());
        let mut multi = KeyConfig::new(deckly_core::profile::MULTI_ACTION, serde_json::json!({}));
        let mut nested = KeyConfig::new("com.example.a", serde_json::json!({"foo": 1}));
        nested.image = Some(original.clone());
        multi.groups[0].push(nested);
        folder.keys.insert(1, multi);
        source.folders.push(folder);
        let mut copy = copy_pictures(source.duplicate("Scenes copy".into()), &root).unwrap();
        std::fs::remove_file(&original).unwrap();
        let mut pictures = 0;
        let copy_id = copy.id.clone();
        copy.for_each_picture(|picture| {
            let Some(path) = picture.as_ref() else { return };
            assert!(path.starts_with(root.join("images").join(&copy_id)));
            assert_eq!(std::fs::read(path).unwrap(), b"<svg/>");
            pictures += 1;
        });
        assert_eq!(pictures, 3);
        assert!(copy_pictures(source.duplicate("Another".into()), &root).is_err());
        assert_eq!(std::fs::read_dir(root.join("images")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

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

    #[test]
    fn key_images_are_owned_immutable_pngs_and_profile_roundtrips() {
        let root = std::env::temp_dir().join(format!("deckly-key-images-{}", uuid::Uuid::new_v4()));
        let image = RgbaImage::from_pixel(288, 288, image::Rgba([12, 34, 56, 255]));
        let first = write_key_image(&root, "test-profile", &image).unwrap();
        let second = write_key_image(&root, "test-profile", &image).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            first.parent(),
            Some(root.join("images/test-profile").as_path())
        );
        assert_eq!(crate::render::decode_file(&first).unwrap(), image);
        let mut profile = Profile::new("Test", deckly_core::DeviceKind::Mk2);
        let mut key = deckly_core::KeyConfig::new(crate::actions::WEBSITE, serde_json::json!({}));
        key.image = Some(first.clone());
        profile.pages[0].keys.insert(0, key);
        let file = root.join("profile.json");
        profile.save(&file).unwrap();
        assert_eq!(Profile::load(&file).unwrap(), profile);
        assert!(write_key_image(&root, "../escape", &image).is_err());
        assert!(write_key_image(&root, "", &image).is_err());
        assert!(write_key_image(&root, "safe", &RgbaImage::new(513, 1)).is_err());
        assert!(write_key_image(&root, "safe", &RgbaImage::new(0, 0)).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn animated_key_images_are_copied_as_they_are() {
        use crate::animation::testing;
        let root = testing::temp("animations");
        let source = testing::temp("source.GIF");
        testing::gif(&source, 16, &[([255, 0, 0], 100), ([0, 0, 255], 100)]);
        let copy = copy_key_animation(&root, "test-profile", &source).unwrap();
        assert_eq!(copy.extension().unwrap(), "gif");
        assert_eq!(
            copy.parent(),
            Some(root.join("images/test-profile").as_path())
        );
        assert_eq!(
            std::fs::read(&copy).unwrap(),
            std::fs::read(&source).unwrap()
        );
        assert!(copy_key_animation(&root, "../escape", &source).is_err());
        let text = testing::temp("notes.txt");
        std::fs::write(&text, "hello").unwrap();
        assert!(copy_key_animation(&root, "test-profile", &text).is_err());
        for path in [source, text] {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

//! Profiles, pages and key assignments.
//!
//! A profile belongs to one device model and holds one or more pages; each
//! page maps key indices (row-major, top-left is 0) to a key configuration.
//! Profiles are stored as JSON in Deckly's own format. Loading never trusts
//! the file: out-of-range keys are dropped and limits are enforced.

use crate::device::{DeviceKind, DeviceModel};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_PAGES: usize = 100;
pub const MAX_FOLDERS: usize = 500;
/// Folders nest at most this deep; deeper folder keys open nothing.
pub const MAX_FOLDER_DEPTH: usize = 16;
pub const MAX_TITLE_CHARS: usize = 200;
/// Profile files larger than this are refused before parsing.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A key that opens a folder; its `folder` setting holds the folder's id.
pub const FOLDER_ACTION: &str = "com.deckly.navigation.folder";
/// The key every folder gets to go back to where it was opened from.
pub const PARENT_FOLDER_ACTION: &str = "com.deckly.navigation.parent-folder";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default = "format_version")]
    pub version: u32,
    pub id: String,
    pub name: String,
    pub device: DeviceKind,
    pub pages: Vec<Page>,
    /// Folders, each one page of keys opened by a folder key. Only folders
    /// that a key leads to are kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<Page>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: String,
    #[serde(default)]
    pub keys: BTreeMap<u8, KeyConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyConfig {
    /// Action UUID, e.g. `com.deckly.system.hotkey`.
    pub action: String,
    #[serde(default)]
    pub title: String,
    /// `None` follows the global title style in the settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_style: Option<TitleStyle>,
    /// Custom key image (PNG or JPEG). `None` uses the action's icon.
    #[serde(default)]
    pub image: Option<PathBuf>,
    /// Action-specific settings.
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct TitleStyle {
    pub show: bool,
    pub color: [u8; 3],
    /// Font size in points at the 72 px key size; scaled for larger keys.
    pub size: u8,
    pub align: TitleAlign,
}

impl TitleStyle {
    pub const MIN_SIZE: u8 = 6;
    pub const MAX_SIZE: u8 = 40;

    pub fn sanitize(&mut self) {
        self.size = self.size.clamp(Self::MIN_SIZE, Self::MAX_SIZE);
    }
}

impl Default for TitleStyle {
    fn default() -> Self {
        Self {
            show: true,
            color: [0xFF, 0xFF, 0xFF],
            size: 12,
            align: TitleAlign::Bottom,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleAlign {
    Top,
    Middle,
    #[default]
    Bottom,
}

impl TitleAlign {
    pub const ALL: [TitleAlign; 3] = [TitleAlign::Top, TitleAlign::Middle, TitleAlign::Bottom];

    pub fn label(self) -> &'static str {
        match self {
            TitleAlign::Top => "Top",
            TitleAlign::Middle => "Middle",
            TitleAlign::Bottom => "Bottom",
        }
    }
}

fn format_version() -> u32 {
    FORMAT_VERSION
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl Page {
    pub fn new() -> Self {
        Self {
            id: new_id(),
            keys: BTreeMap::new(),
        }
    }
}

impl KeyConfig {
    pub fn new(action: impl Into<String>, settings: serde_json::Value) -> Self {
        Self {
            action: action.into(),
            title: String::new(),
            title_style: None,
            image: None,
            settings,
        }
    }

    /// The folder a folder key opens.
    pub fn folder(&self) -> Option<&str> {
        if self.action != FOLDER_ACTION {
            return None;
        }
        self.settings.get("folder").and_then(|v| v.as_str())
    }

    fn set_folder(&mut self, id: &str) {
        if !self.settings.is_object() {
            self.settings = serde_json::json!({});
        }
        if let Some(map) = self.settings.as_object_mut() {
            map.insert("folder".to_owned(), id.into());
        }
    }
}

impl Profile {
    pub fn new(name: impl Into<String>, device: DeviceKind) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_id(),
            name: name.into(),
            device,
            pages: vec![Page::new()],
            folders: Vec::new(),
        }
    }

    pub fn folder(&self, id: &str) -> Option<&Page> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub fn folder_mut(&mut self, id: &str) -> Option<&mut Page> {
        self.folders.iter_mut().find(|f| f.id == id)
    }

    /// Gives every folder key a folder of its own and drops folders no key
    /// leads to. A folder key whose folder is missing gets a new empty one;
    /// a folder that a second key leads to (a pasted key) is copied for that
    /// key, with the folders inside it; a key leading into one of its own
    /// enclosing folders gets an empty one. Every folder keeps a parent
    /// folder key. Past the folder limits a folder key opens nothing.
    pub fn normalize_folders(&mut self) {
        let key_count = self.model().key_count();
        let mut walk = FolderWalk::default();
        let mut order = Vec::new();
        for folder in std::mem::take(&mut self.folders) {
            if !walk.original.contains_key(&folder.id) {
                order.push(folder.id.clone());
                walk.original.insert(folder.id.clone(), folder);
            }
        }
        for page in &mut self.pages {
            walk.resolve(page, &[], key_count);
        }
        // Folders keep their place in the file; new ones go last.
        walk.done.sort_by_key(|f| {
            order
                .iter()
                .position(|id| *id == f.id)
                .unwrap_or(usize::MAX)
        });
        self.folders = walk.done;
    }

    pub fn model(&self) -> &'static DeviceModel {
        DeviceModel::of(self.device)
    }

    /// Brings a profile from an untrusted source within limits so the rest
    /// of the app can rely on them.
    pub fn sanitize(&mut self) {
        let key_count = self.model().key_count();
        self.pages.truncate(MAX_PAGES);
        if self.pages.is_empty() {
            self.pages.push(Page::new());
        }
        self.folders.truncate(MAX_FOLDERS);
        for page in self.pages.iter_mut().chain(&mut self.folders) {
            if page.id.is_empty() {
                page.id = new_id();
            }
            page.keys.retain(|&index, _| index < key_count);
            for key in page.keys.values_mut() {
                if key.title.chars().count() > MAX_TITLE_CHARS {
                    key.title = key.title.chars().take(MAX_TITLE_CHARS).collect();
                }
                if let Some(style) = &mut key.title_style {
                    style.sanitize();
                }
                // Before the global title style existed every key stored
                // one; an untouched default style now follows the global one.
                if key.title_style == Some(TitleStyle::default()) {
                    key.title_style = None;
                }
            }
        }
        self.normalize_folders();
        if self.id.is_empty() {
            self.id = new_id();
        }
    }

    pub fn from_json(text: &str) -> Result<Self, ProfileError> {
        let mut profile: Profile = serde_json::from_str(text).map_err(ProfileError::Parse)?;
        if profile.version > FORMAT_VERSION {
            return Err(ProfileError::NewerFormat(profile.version));
        }
        profile.sanitize();
        Ok(profile)
    }

    pub fn to_json(&self) -> Result<String, ProfileError> {
        serde_json::to_string_pretty(self).map_err(ProfileError::Parse)
    }

    pub fn load(path: &Path) -> Result<Self, ProfileError> {
        let len = std::fs::metadata(path).map_err(ProfileError::Io)?.len();
        if len > MAX_FILE_BYTES {
            return Err(ProfileError::TooLarge(len));
        }
        let text = std::fs::read_to_string(path).map_err(ProfileError::Io)?;
        Self::from_json(&text)
    }

    /// Writes through a temporary file so a crash never leaves a half-written
    /// profile behind.
    pub fn save(&self, path: &Path) -> Result<(), ProfileError> {
        let text = self.to_json()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(ProfileError::Io)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(ProfileError::Io)?;
        std::fs::rename(&tmp, path).map_err(ProfileError::Io)
    }
}

#[derive(Default)]
struct FolderWalk {
    /// The folders as they were before the walk, by id.
    original: HashMap<String, Page>,
    /// Original folders already given to a key.
    used: HashSet<String>,
    done: Vec<Page>,
}

impl FolderWalk {
    /// Resolves the folder keys on a page inside `ancestors`, the original
    /// ids of the folders around it, outermost first.
    fn resolve(&mut self, page: &mut Page, ancestors: &[String], key_count: u8) {
        for key in page.keys.values_mut() {
            if key.action != FOLDER_ACTION {
                continue;
            }
            if ancestors.len() >= MAX_FOLDER_DEPTH || self.done.len() >= MAX_FOLDERS {
                key.set_folder("");
                continue;
            }
            let target = key.folder().unwrap_or_default().to_owned();
            let source = self
                .original
                .get(&target)
                .filter(|_| !ancestors.contains(&target))
                .cloned();
            let mut folder = match source {
                Some(f) if self.used.insert(target.clone()) => f,
                Some(mut copy) => {
                    copy.id = new_id();
                    copy
                }
                None => Page::new(),
            };
            key.set_folder(&folder.id);
            let mut inner = ancestors.to_vec();
            inner.push(target);
            self.resolve(&mut folder, &inner, key_count);
            ensure_parent_key(&mut folder, key_count);
            self.done.push(folder);
        }
    }
}

/// Puts a parent folder key in a folder that has none: top left when that
/// key is free, else on the first free key, else over the top-left key.
fn ensure_parent_key(folder: &mut Page, key_count: u8) {
    if folder
        .keys
        .values()
        .any(|k| k.action == PARENT_FOLDER_ACTION)
    {
        return;
    }
    let slot = (0..key_count)
        .find(|i| !folder.keys.contains_key(i))
        .unwrap_or(0);
    folder.keys.insert(
        slot,
        KeyConfig::new(PARENT_FOLDER_ACTION, serde_json::json!({})),
    );
}

#[derive(Debug)]
pub enum ProfileError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    TooLarge(u64),
    NewerFormat(u32),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::Io(e) => write!(f, "{e}"),
            ProfileError::Parse(e) => write!(f, "invalid profile: {e}"),
            ProfileError::TooLarge(n) => write!(f, "profile file is too large ({n} bytes)"),
            ProfileError::NewerFormat(v) => {
                write!(f, "profile was saved by a newer Deckly (format {v})")
            }
        }
    }
}

impl std::error::Error for ProfileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        let mut key = KeyConfig::new(
            "com.deckly.system.website",
            json!({"url": "https://example.com"}),
        );
        key.title = "Site".into();
        p.pages[0].keys.insert(3, key);
        let text = p.to_json().unwrap();
        assert_eq!(Profile::from_json(&text).unwrap(), p);
    }

    #[test]
    fn sanitize_drops_out_of_range_keys_and_adds_page() {
        let text = r#"{"id":"","name":"x","device":"mk2","pages":[]}"#;
        let p = Profile::from_json(text).unwrap();
        assert_eq!(p.pages.len(), 1);
        assert!(!p.id.is_empty());

        let text = r#"{"id":"a","name":"x","device":"mk2","pages":[{"id":"p","keys":{
            "14":{"action":"a"},"15":{"action":"b"},"200":{"action":"c"}}}]}"#;
        let p = Profile::from_json(text).unwrap();
        assert_eq!(
            p.pages[0].keys.keys().copied().collect::<Vec<_>>(),
            vec![14]
        );
    }

    #[test]
    fn default_title_styles_follow_the_global_style() {
        let text = r#"{"id":"a","name":"x","device":"mk2","pages":[{"id":"p","keys":{
            "0":{"action":"a","title_style":{"show":true,"color":[255,255,255],"size":12,"align":"bottom"}},
            "1":{"action":"b","title_style":{"size":99}},
            "2":{"action":"c"}}}]}"#;
        let p = Profile::from_json(text).unwrap();
        let keys = &p.pages[0].keys;
        assert_eq!(keys[&0].title_style, None);
        assert_eq!(
            keys[&1].title_style.map(|s| s.size),
            Some(TitleStyle::MAX_SIZE)
        );
        assert_eq!(keys[&2].title_style, None);
        assert!(!p.to_json().unwrap().contains("\"title_style\": null"));
    }

    fn folder_key(id: &str) -> KeyConfig {
        KeyConfig::new(FOLDER_ACTION, json!({ "folder": id }))
    }

    #[test]
    fn new_folder_keys_get_a_folder_with_a_parent_key() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.pages[0].keys.insert(4, folder_key(""));
        p.normalize_folders();
        let id = p.pages[0].keys[&4].folder().unwrap().to_owned();
        let folder = p.folder(&id).unwrap();
        assert_eq!(folder.keys[&0].action, PARENT_FOLDER_ACTION);
        // Normalizing again changes nothing.
        let before = p.clone();
        p.normalize_folders();
        assert_eq!(p, before);
        // The folder survives a save and load.
        assert_eq!(Profile::from_json(&p.to_json().unwrap()).unwrap(), p);
    }

    #[test]
    fn unreachable_folders_are_dropped() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.pages[0].keys.insert(0, folder_key(""));
        p.normalize_folders();
        assert_eq!(p.folders.len(), 1);
        p.pages[0].keys.clear();
        p.normalize_folders();
        assert!(p.folders.is_empty());
        assert!(!p.to_json().unwrap().contains("folders"));
    }

    #[test]
    fn a_pasted_folder_key_gets_a_deep_copy() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.pages[0].keys.insert(0, folder_key(""));
        p.normalize_folders();
        let outer = p.pages[0].keys[&0].folder().unwrap().to_owned();
        p.folder_mut(&outer).unwrap().keys.insert(5, folder_key(""));
        p.normalize_folders();
        assert_eq!(p.folders.len(), 2);
        // Paste the outer folder key on another key.
        let copy = p.pages[0].keys[&0].clone();
        p.pages[0].keys.insert(1, copy);
        p.normalize_folders();
        assert_eq!(p.folders.len(), 4);
        let a = p.pages[0].keys[&0].folder().unwrap();
        let b = p.pages[0].keys[&1].folder().unwrap();
        assert_ne!(a, b);
        let inner = |id: &str| p.folder(id).unwrap().keys[&5].folder().unwrap().to_owned();
        assert_ne!(inner(a), inner(b));
    }

    #[test]
    fn folder_cycles_and_missing_folders_are_repaired() {
        // Folder "a" holds a key leading back to itself, and "b" is missing.
        let text = r#"{"id":"x","name":"x","device":"mk2",
            "pages":[{"id":"p","keys":{"0":{"action":"com.deckly.navigation.folder","settings":{"folder":"a"}},
                                      "1":{"action":"com.deckly.navigation.folder","settings":{"folder":"b"}}}}],
            "folders":[{"id":"a","keys":{"3":{"action":"com.deckly.navigation.folder","settings":{"folder":"a"}},
                                         "99":{"action":"junk"}}}]}"#;
        let p = Profile::from_json(text).unwrap();
        assert_eq!(p.folders.len(), 3);
        let a = p.folder("a").unwrap();
        assert_eq!(a.keys[&0].action, PARENT_FOLDER_ACTION);
        assert!(!a.keys.contains_key(&99));
        let inner = a.keys[&3].folder().unwrap();
        assert_ne!(inner, "a");
        assert!(p.folder(inner).is_some());
        assert!(p.folder(p.pages[0].keys[&1].folder().unwrap()).is_some());
    }

    #[test]
    fn folder_nesting_is_bounded() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.pages[0].keys.insert(0, folder_key(""));
        // Nest one level deeper on each pass.
        for _ in 0..MAX_FOLDER_DEPTH + 5 {
            p.normalize_folders();
            for folder in &mut p.folders {
                folder.keys.entry(1).or_insert_with(|| folder_key(""));
            }
        }
        p.normalize_folders();
        assert_eq!(p.folders.len(), MAX_FOLDER_DEPTH);
    }

    #[test]
    fn rejects_newer_format() {
        let text = r#"{"version":99,"id":"a","name":"x","device":"mk2","pages":[]}"#;
        assert!(matches!(
            Profile::from_json(text),
            Err(ProfileError::NewerFormat(99))
        ));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Profile::from_json("not json").is_err());
        assert!(
            Profile::from_json(r#"{"id":"a","name":"x","device":"toaster","pages":[]}"#).is_err()
        );
    }
}

//! Profiles, pages and key assignments.
//!
//! A profile belongs to one device model and holds one or more pages; each
//! page maps key indices (row-major, top-left is 0) to a key configuration.
//! Profiles are stored as JSON in Deckly's own format. Loading never trusts
//! the file: out-of-range keys are dropped and limits are enforced.

use crate::device::{DeviceKind, DeviceModel};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_PAGES: usize = 100;
pub const MAX_TITLE_CHARS: usize = 200;
/// Profile files larger than this are refused before parsing.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default = "format_version")]
    pub version: u32,
    pub id: String,
    pub name: String,
    pub device: DeviceKind,
    pub pages: Vec<Page>,
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
}

impl Profile {
    pub fn new(name: impl Into<String>, device: DeviceKind) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_id(),
            name: name.into(),
            device,
            pages: vec![Page::new()],
        }
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
        for page in &mut self.pages {
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

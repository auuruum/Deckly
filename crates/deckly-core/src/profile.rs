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
pub const MAX_PAGE_NAME_CHARS: usize = 64;
/// The name of the profile made for a device that has none.
pub const DEFAULT_PROFILE_NAME: &str = "Default Profile";
/// Profile files larger than this are refused before parsing.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A key that opens a folder; its `folder` setting holds the folder's id.
pub const FOLDER_ACTION: &str = "com.deckly.navigation.folder";
/// The key every folder gets to go back to where it was opened from.
pub const PARENT_FOLDER_ACTION: &str = "com.deckly.navigation.parent-folder";

/// Runs its actions in order.
pub const MULTI_ACTION: &str = "com.deckly.multi.action";
/// Two Multi Actions on one key; each press runs the next one.
pub const MULTI_ACTION_SWITCH: &str = "com.deckly.multi.switch";
/// Runs one of its actions, picked at random.
pub const RANDOM_ACTION: &str = "com.deckly.multi.random";
/// One action each for a single press, a double press and a hold.
pub const KEY_LOGIC: &str = "com.deckly.multi.key-logic";
/// A pause between the actions of a Multi Action.
pub const DELAY_ACTION: &str = "com.deckly.multi.delay";
/// The actions one list of a Multi Action can hold.
pub const MAX_GROUP_ACTIONS: usize = 100;

/// How many lists of actions a key with this action holds.
pub fn group_count(action: &str) -> usize {
    match action {
        MULTI_ACTION | RANDOM_ACTION => 1,
        MULTI_ACTION_SWITCH => 2,
        KEY_LOGIC => 3,
        _ => 0,
    }
}

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
    /// Empty shows the page's number, see [`Page::title`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Picture shown for the page instead of its number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PathBuf>,
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
    /// The actions a Multi Action, Multi Action Switch, Random Action or
    /// Key Logic key holds, one list per [`group_count`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Vec<KeyConfig>>,
    /// Custom timing of an action inside a Multi Action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delays: Option<ActionDelays>,
    /// The state a many-state action inside a Multi Action asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_state: Option<u8>,
}

/// Timing of one action inside a Multi Action, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDelays {
    /// From the virtual key press to its release.
    pub press: u32,
    /// From the release to the next action.
    pub next: u32,
}

impl Default for ActionDelays {
    fn default() -> Self {
        Self {
            press: Self::MAX,
            next: Self::MAX,
        }
    }
}

impl ActionDelays {
    pub const MIN: u32 = 1;
    pub const MAX: u32 = 100;

    pub fn sanitize(&mut self) {
        self.press = self.press.clamp(Self::MIN, Self::MAX);
        self.next = self.next.clamp(Self::MIN, Self::MAX);
    }
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
            ..Self::default()
        }
    }

    /// The name people see for the page at `index`.
    pub fn title(&self, index: usize) -> String {
        if self.name.is_empty() {
            format!("Page {}", index + 1)
        } else {
            self.name.clone()
        }
    }

    /// The short form for lists and keys: its name or its number.
    pub fn label(&self, index: usize) -> String {
        if self.name.is_empty() {
            (index + 1).to_string()
        } else {
            self.name.clone()
        }
    }
}

impl KeyConfig {
    pub fn new(action: impl Into<String>, settings: serde_json::Value) -> Self {
        let action: String = action.into();
        let groups = vec![Vec::new(); group_count(&action)];
        Self {
            action,
            title: String::new(),
            title_style: None,
            image: None,
            settings,
            groups,
            delays: None,
            desired_state: None,
        }
    }

    /// Whether `member` can go in this key's lists of actions. Lists hold
    /// single actions only; Delay is for sequences, not Key Logic.
    pub fn can_hold(&self, member: &str) -> bool {
        group_count(&self.action) > 0
            && group_count(member) == 0
            && !matches!(member, FOLDER_ACTION | PARENT_FOLDER_ACTION)
            && (member != DELAY_ACTION || self.action != KEY_LOGIC)
    }

    /// Brings the action lists of a key from an untrusted source within
    /// limits: the right number of lists, and only actions that can be in
    /// them. Key Logic has one action per list.
    fn sanitize_groups(&mut self) {
        let lists = group_count(&self.action);
        let mut groups = std::mem::take(&mut self.groups);
        groups.resize(lists, Vec::new());
        let size = if self.action == KEY_LOGIC {
            1
        } else {
            MAX_GROUP_ACTIONS
        };
        for list in &mut groups {
            list.retain(|k| self.can_hold(&k.action));
            list.truncate(size);
            for member in list {
                member.groups.clear();
                if let Some(delays) = &mut member.delays {
                    delays.sanitize();
                }
                sanitize_title(member);
            }
        }
        self.groups = groups;
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

    /// Visits every key on pages and in folders, and every action a key holds.
    pub fn for_each_key(&mut self, mut visit: impl FnMut(&mut KeyConfig)) {
        for page in self.pages.iter_mut().chain(&mut self.folders) {
            for key in page.keys.values_mut() {
                visit(key);
                key.groups.iter_mut().flatten().for_each(&mut visit);
            }
        }
    }

    /// Visits every picture the profile points at: page icons and key images.
    pub fn for_each_picture(&mut self, mut visit: impl FnMut(&mut Option<PathBuf>)) {
        for page in &mut self.pages {
            visit(&mut page.icon);
        }
        self.for_each_key(|key| visit(&mut key.image));
    }

    /// Whether this is still the empty profile made for a device that had
    /// none. Nothing is lost when one goes.
    pub fn is_placeholder(&self) -> bool {
        self.name == DEFAULT_PROFILE_NAME
            && self.folders.is_empty()
            && matches!(self.pages.as_slice(), [page]
                if page.keys.is_empty() && page.name.is_empty() && page.icon.is_none())
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
            page.name = page.name.trim().chars().take(MAX_PAGE_NAME_CHARS).collect();
            page.keys.retain(|&index, _| index < key_count);
            for key in page.keys.values_mut() {
                sanitize_title(key);
                key.sanitize_groups();
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

fn sanitize_title(key: &mut KeyConfig) {
    if key.title.chars().count() > MAX_TITLE_CHARS {
        key.title = key.title.chars().take(MAX_TITLE_CHARS).collect();
    }
    if let Some(style) = &mut key.title_style {
        style.sanitize();
    }
    // Before the global title style existed every key stored one; an
    // untouched default style now follows the global one.
    if key.title_style == Some(TitleStyle::default()) {
        key.title_style = None;
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
    fn group_keys_round_trip_with_their_actions() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        let mut key = KeyConfig::new(MULTI_ACTION_SWITCH, json!({}));
        assert_eq!(key.groups.len(), 2);
        let mut member = KeyConfig::new("com.example.a", json!({"x": 1}));
        member.delays = Some(ActionDelays { press: 1, next: 50 });
        member.desired_state = Some(1);
        key.groups[1].push(member);
        p.pages[0].keys.insert(0, key);
        p.pages[0]
            .keys
            .insert(1, KeyConfig::new("com.example.b", json!({})));
        let text = p.to_json().unwrap();
        assert_eq!(Profile::from_json(&text).unwrap(), p);
        // Plain keys stay as small as before.
        assert_eq!(text.matches("\"groups\"").count(), 1);
        assert!(!text.contains("delays\": null"));
    }

    #[test]
    fn group_lists_are_repaired() {
        let text = r#"{"id":"a","name":"x","device":"mk2","pages":[{"id":"p","keys":{
            "0":{"action":"com.deckly.multi.action","groups":[
                [{"action":"com.deckly.multi.action"},
                 {"action":"com.deckly.navigation.folder"},
                 {"action":"com.deckly.navigation.parent-folder"},
                 {"action":"com.deckly.multi.delay","groups":[[{"action":"x"}]],
                  "delays":{"press":0,"next":99999}}],
                [{"action":"extra"}]]},
            "1":{"action":"com.deckly.multi.key-logic","groups":[
                [{"action":"a"},{"action":"b"}],
                [{"action":"com.deckly.multi.delay"}]]},
            "2":{"action":"com.deckly.multi.switch"},
            "3":{"action":"plain","groups":[[{"action":"x"}]]}}}]}"#;
        let p = Profile::from_json(text).unwrap();
        let keys = &p.pages[0].keys;
        assert_eq!(keys[&0].groups.len(), 1);
        let only = &keys[&0].groups[0];
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].action, DELAY_ACTION);
        assert!(only[0].groups.is_empty());
        assert_eq!(
            only[0].delays,
            Some(ActionDelays {
                press: ActionDelays::MIN,
                next: ActionDelays::MAX
            })
        );
        // Key Logic has one action per press and no pauses.
        assert_eq!(keys[&1].groups.len(), 3);
        assert_eq!(keys[&1].groups[0].len(), 1);
        assert!(keys[&1].groups[1].is_empty());
        assert_eq!(keys[&2].groups.len(), 2);
        assert!(keys[&3].groups.is_empty());
    }

    #[test]
    fn group_lists_are_bounded() {
        let mut key = KeyConfig::new(MULTI_ACTION, json!({}));
        key.groups[0] = vec![KeyConfig::new("a", json!({})); MAX_GROUP_ACTIONS + 5];
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.pages[0].keys.insert(0, key);
        p.sanitize();
        assert_eq!(p.pages[0].keys[&0].groups[0].len(), MAX_GROUP_ACTIONS);
    }

    #[test]
    fn pages_keep_their_name_and_icon() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.pages[0].name = "Scenes".into();
        p.pages[0].icon = Some(PathBuf::from("icon.svg"));
        p.pages.push(Page::new());
        let back = Profile::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.pages[0].title(0), "Scenes");
        assert_eq!(back.pages[1].title(1), "Page 2");
        assert_eq!(back.pages[0].label(0), "Scenes");
        assert_eq!(back.pages[1].label(1), "2");

        p.pages[0].name = format!("  {}  ", "x".repeat(MAX_PAGE_NAME_CHARS + 5));
        p.sanitize();
        assert_eq!(p.pages[0].name.chars().count(), MAX_PAGE_NAME_CHARS);
    }

    #[test]
    fn only_an_untouched_default_profile_is_a_placeholder() {
        let new = || Profile::new(DEFAULT_PROFILE_NAME, DeviceKind::Mk2);
        assert!(new().is_placeholder());
        assert!(!Profile::new("Streaming", DeviceKind::Mk2).is_placeholder());
        let mut with_key = new();
        with_key.pages[0]
            .keys
            .insert(0, KeyConfig::new("a", json!({})));
        assert!(!with_key.is_placeholder());
        let mut with_page = new();
        with_page.pages.push(Page::new());
        assert!(!with_page.is_placeholder());
        let mut named = new();
        named.pages[0].name = "Scenes".into();
        assert!(!named.is_placeholder());
    }

    #[test]
    fn pictures_of_pages_and_keys_are_visited() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.pages[0].icon = Some(PathBuf::from("page.svg"));
        let mut key = KeyConfig::new("a", json!({}));
        key.image = Some(PathBuf::from("key.png"));
        p.pages[0].keys.insert(0, key);
        p.for_each_picture(|picture| {
            *picture = picture.take().map(|path| Path::new("new").join(path));
        });
        assert_eq!(p.pages[0].icon, Some(Path::new("new").join("page.svg")));
        assert_eq!(
            p.pages[0].keys[&0].image,
            Some(Path::new("new").join("key.png"))
        );
    }

    #[test]
    fn every_key_is_visited() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        let mut multi = KeyConfig::new(MULTI_ACTION, json!({}));
        multi.groups[0].push(KeyConfig::new("a", json!({})));
        p.pages[0].keys.insert(0, multi);
        p.pages[0].keys.insert(1, folder_key(""));
        p.normalize_folders();
        let mut seen = 0;
        p.for_each_key(|_| seen += 1);
        // The Multi Action, its action, the folder key and the parent key.
        assert_eq!(seen, 4);
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

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
pub const MAX_PROFILE_NAME_CHARS: usize = 64;
/// Applications one profile can follow; more are dropped on load.
pub const MAX_PROFILE_APPLICATIONS: usize = 64;
const MAX_APPLICATION_CHARS: usize = 256;
const MAX_APPLICATION_ICON_BYTES: usize = 1024;
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
    /// Default for pages and folders without a wallpaper override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallpaper: Option<Wallpaper>,
    pub pages: Vec<Page>,
    /// Folders, each one page of keys opened by a folder key. Only folders
    /// that a key leads to are kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<Page>,
    /// Applications that bring this profile up while they are in the
    /// foreground. Kept here so they leave with the profile when it is
    /// deleted and travel with it in exports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applications: Vec<AppRule>,
}

/// An application that brings a profile up
/// (docs/clean-room/2026-11-02-active-window-profiles.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRule {
    /// As Preferences lists it.
    pub name: String,
    /// A program, bundle, package or window class. A path keeps only its last
    /// part: programs move between versioned folders, and a shared profile
    /// lands on a computer with other folders.
    pub id: String,
    /// Where the system finds its picture on this computer, as the picker or
    /// a chosen file gave it. Empty or gone leaves the plain window icon.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
}

impl AppRule {
    pub fn new(name: &str, id: &str) -> Self {
        let limit = |text: &str| text.trim().chars().take(MAX_APPLICATION_CHARS).collect();
        let id: String = limit(last_part(id));
        Self {
            name: if name.trim().is_empty() {
                id.clone()
            } else {
                limit(name)
            },
            id,
            icon: String::new(),
        }
    }

    /// A source too long to be real leaves the rule without a picture.
    pub fn with_icon(mut self, icon: &str) -> Self {
        if icon.len() <= MAX_APPLICATION_ICON_BYTES {
            self.icon = icon.to_owned();
        }
        self
    }

    /// Whether this is one of the names the system gives an application. Two
    /// applications with the same program file name both match.
    pub fn matches(&self, names: &[String]) -> bool {
        let id = self.id.to_lowercase();
        names.iter().any(|n| last_part(n).to_lowercase() == id)
    }
}

fn last_part(path: &str) -> &str {
    let path = path.trim_end_matches(['/', '\\']);
    path.rsplit(['/', '\\']).next().unwrap_or(path)
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
    /// One picture spread over all keys of the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallpaper: Option<Wallpaper>,
    /// An explicit choice to leave this page or folder without wallpaper.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wallpaper_disabled: bool,
    #[serde(default)]
    pub keys: BTreeMap<u8, KeyConfig>,
}

/// A picture spread over all keys of a page, in the device's rows and
/// columns. The keys show it where their own picture leaves room: empty
/// keys and action icons show it whole, transparent parts of key pictures
/// let it through.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Wallpaper {
    pub image: PathBuf,
    /// In percent. 100 covers all keys with as much of the picture as fits
    /// their shape; more shows a smaller part of it.
    #[serde(default = "Wallpaper::default_zoom")]
    pub zoom: u16,
    /// The middle of the part shown, in thousandths of the picture's width
    /// and height. It moves only as far as the picture reaches.
    #[serde(default = "Wallpaper::middle")]
    pub x: u16,
    #[serde(default = "Wallpaper::middle")]
    pub y: u16,
}

impl Wallpaper {
    pub const MIN_ZOOM: u16 = 100;
    pub const MAX_ZOOM: u16 = 400;
    /// The largest `x` and `y`: the right and bottom edges.
    pub const END: u16 = 1000;

    pub fn new(image: PathBuf) -> Self {
        Self {
            image,
            zoom: Self::default_zoom(),
            x: Self::middle(),
            y: Self::middle(),
        }
    }

    fn default_zoom() -> u16 {
        Self::MIN_ZOOM
    }

    fn middle() -> u16 {
        Self::END / 2
    }

    pub fn sanitize(&mut self) {
        self.zoom = self.zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.x = self.x.min(Self::END);
        self.y = self.y.min(Self::END);
    }

    fn sanitize_optional(wallpaper: &mut Option<Self>) {
        if let Some(wall) = wallpaper {
            if wall.image.as_os_str().is_empty() {
                *wallpaper = None;
            } else {
                wall.sanitize();
            }
        }
    }

    fn visit_picture(wallpaper: &mut Option<Self>, visit: &mut impl FnMut(&mut Option<PathBuf>)) {
        if let Some(wall) = wallpaper {
            let mut image = Some(std::mem::take(&mut wall.image));
            visit(&mut image);
            match image {
                Some(image) => wall.image = image,
                None => *wallpaper = None,
            }
        }
    }
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
    /// Folder exit behavior after this key runs. `None` follows the folder's
    /// parent key; on that parent key it means automatic exit is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_exit: Option<AutoExit>,
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

/// Automatic return to the enclosing folder after an action, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoExit {
    pub enabled: bool,
    pub delay: u8,
}

impl Default for AutoExit {
    fn default() -> Self {
        Self {
            enabled: false,
            delay: 1,
        }
    }
}

impl AutoExit {
    pub const MAX_DELAY: u8 = 60;

    pub fn sanitize(&mut self) {
        self.delay = self.delay.min(Self::MAX_DELAY);
    }
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct TitleStyle {
    pub show: bool,
    pub color: [u8; 3],
    /// Font size in points at the 72 px key size; scaled for larger keys.
    pub size: u8,
    pub align: TitleAlign,
    /// Font family as the system names it. Empty is Deckly's default font;
    /// a family that is not installed is drawn with it too.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub font: String,
    /// Titles were always bold before the font options existed, so bold is
    /// the default and old profiles look the same.
    #[serde(skip_serializing_if = "is_true")]
    pub bold: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub underline: bool,
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl TitleStyle {
    pub const MIN_SIZE: u8 = 6;
    pub const MAX_SIZE: u8 = 40;
    pub const MAX_FONT_CHARS: usize = 64;

    pub fn sanitize(&mut self) {
        self.size = self.size.clamp(Self::MIN_SIZE, Self::MAX_SIZE);
        let font = self.font.trim();
        if font.len() != self.font.len()
            || font.chars().count() > Self::MAX_FONT_CHARS
            || font.chars().any(char::is_control)
        {
            self.font = font
                .chars()
                .filter(|c| !c.is_control())
                .take(Self::MAX_FONT_CHARS)
                .collect();
        }
    }
}

impl Default for TitleStyle {
    fn default() -> Self {
        Self {
            show: true,
            color: [0xFF, 0xFF, 0xFF],
            size: 12,
            align: TitleAlign::Bottom,
            font: String::new(),
            bold: true,
            italic: false,
            underline: false,
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

    pub fn effective_wallpaper<'a>(
        &'a self,
        default: Option<&'a Wallpaper>,
    ) -> Option<&'a Wallpaper> {
        if self.wallpaper_disabled {
            None
        } else {
            self.wallpaper.as_ref().or(default)
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
            auto_exit: None,
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
            wallpaper: None,
            pages: vec![Page::new()],
            folders: Vec::new(),
            applications: Vec::new(),
        }
    }

    /// An independent copy, including fresh ids and links to its own folders.
    /// Its applications stay with the original, so two profiles do not claim one.
    pub fn duplicate(&self, name: String) -> Self {
        let mut copy = self.clone();
        copy.id = new_id();
        copy.name = name;
        copy.applications.clear();
        let mut ids = HashMap::new();
        for page in copy.pages.iter_mut().chain(&mut copy.folders) {
            let id = new_id();
            ids.insert(page.id.clone(), id.clone());
            page.id = id;
        }
        copy.for_each_key(|key| {
            if key.action == FOLDER_ACTION
                && let Some(target) = key.settings.get_mut("folder")
                && let Some(id) = target.as_str().and_then(|id| ids.get(id))
            {
                *target = id.clone().into();
            }
        });
        copy
    }

    /// A free name among profiles for the same model. Existing names are
    /// compared without case; copy suffixes never accumulate on repeated copies.
    pub fn available_name(&self, profiles: &[Profile]) -> String {
        let taken: HashSet<String> = profiles
            .iter()
            .filter(|p| p.device == self.device && p.id != self.id)
            .map(|p| p.name.trim().to_lowercase())
            .collect();
        let name: String = self
            .name
            .trim()
            .chars()
            .take(MAX_PROFILE_NAME_CHARS)
            .collect();
        if !taken.contains(&name.to_lowercase()) {
            return name;
        }
        let mut base = name.as_str();
        // Strip only whole trailing words, preserving names such as "Photocopy".
        loop {
            let without_number = base
                .rsplit_once(' ')
                .filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                .map_or(base, |(head, _)| head);
            let stripped = without_number
                .rsplit_once(' ')
                .filter(|(_, word)| word.eq_ignore_ascii_case("copy"));
            match stripped {
                Some((head, _)) => base = head.trim_end(),
                None => break,
            }
        }
        if !taken.contains(&base.to_lowercase()) {
            return base.to_owned();
        }
        for n in 0..=taken.len() {
            let suffix = if n == 0 {
                " copy".to_owned()
            } else {
                format!(" copy {n}")
            };
            let prefix: String = base
                .chars()
                .take(MAX_PROFILE_NAME_CHARS.saturating_sub(suffix.len()))
                .collect();
            let candidate = format!("{}{suffix}", prefix.trim_end());
            if !taken.contains(&candidate.to_lowercase()) {
                return candidate;
            }
        }
        // There are more candidates than occupied names, so the loop always returns.
        name
    }

    pub fn folder(&self, id: &str) -> Option<&Page> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// Appends an empty page unless the profile has reached the page limit.
    pub fn add_page(&mut self) -> Option<String> {
        if self.pages.len() >= MAX_PAGES {
            return None;
        }
        let page = Page::new();
        let id = page.id.clone();
        self.pages.push(page);
        Some(id)
    }

    /// Deletes a regular page and its unreachable folders, keeping at least one page.
    /// Returns the removed page's former index.
    pub fn remove_page(&mut self, id: &str) -> Option<usize> {
        if self.pages.len() <= 1 {
            return None;
        }
        let index = self.pages.iter().position(|page| page.id == id)?;
        self.pages.remove(index);
        self.normalize_folders();
        Some(index)
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

    /// Visits every picture the profile points at, including wallpapers.
    pub fn for_each_picture(&mut self, mut visit: impl FnMut(&mut Option<PathBuf>)) {
        Wallpaper::visit_picture(&mut self.wallpaper, &mut visit);
        for page in self.pages.iter_mut().chain(&mut self.folders) {
            visit(&mut page.icon);
            Wallpaper::visit_picture(&mut page.wallpaper, &mut visit);
        }
        self.for_each_key(|key| visit(&mut key.image));
    }

    /// Whether this is still the empty profile made for a device that had
    /// none. Nothing is lost when one goes.
    pub fn is_placeholder(&self) -> bool {
        self.name == DEFAULT_PROFILE_NAME
            && self.folders.is_empty()
            && self.applications.is_empty()
            && self.wallpaper.is_none()
            && matches!(self.pages.as_slice(), [page]
                if page.keys.is_empty()
                    && page.name.is_empty()
                    && page.icon.is_none()
                    && page.wallpaper.is_none()
                    && !page.wallpaper_disabled)
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
        Wallpaper::sanitize_optional(&mut self.wallpaper);
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
            if page.wallpaper_disabled {
                page.wallpaper = None;
            }
            Wallpaper::sanitize_optional(&mut page.wallpaper);
            for key in page.keys.values_mut() {
                sanitize_title(key);
                key.sanitize_groups();
            }
        }
        self.normalize_folders();
        if self.id.is_empty() {
            self.id = new_id();
        }
        self.applications = std::mem::take(&mut self.applications)
            .iter()
            .map(|rule| AppRule::new(&rule.name, &rule.id).with_icon(&rule.icon))
            .filter(|rule| !rule.id.is_empty())
            .take(MAX_PROFILE_APPLICATIONS)
            .collect();
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
    if let Some(auto_exit) = &mut key.auto_exit {
        auto_exit.sanitize();
    }
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
    fn auto_exit_defaults_to_inheritance_and_preserves_explicit_settings() {
        let legacy: KeyConfig = serde_json::from_value(json!({
            "action": "com.example.action", "settings": {}
        }))
        .unwrap();
        assert_eq!(legacy.auto_exit, None);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("auto_exit")
                .is_none()
        );
        assert_eq!(
            AutoExit::default(),
            AutoExit {
                enabled: false,
                delay: 1
            }
        );

        let mut profile = Profile::new("Scenes", DeviceKind::Mk2);
        for (index, auto_exit) in [
            None,
            Some(AutoExit {
                enabled: false,
                delay: 9,
            }),
            Some(AutoExit {
                enabled: true,
                delay: 0,
            }),
            Some(AutoExit {
                enabled: true,
                delay: 60,
            }),
        ]
        .into_iter()
        .enumerate()
        {
            let mut key = legacy.clone();
            key.auto_exit = auto_exit;
            profile.pages[0].keys.insert(index as u8, key);
        }
        assert_eq!(
            Profile::from_json(&profile.to_json().unwrap()).unwrap(),
            profile
        );
    }

    #[test]
    fn auto_exit_delays_are_bounded_on_keys_and_group_members() {
        let mut profile = Profile::new("Scenes", DeviceKind::Mk2);
        let mut key = KeyConfig::new(MULTI_ACTION, json!({}));
        key.auto_exit = Some(AutoExit {
            enabled: true,
            delay: 255,
        });
        let mut member = KeyConfig::new("com.example.action", json!({}));
        member.auto_exit = Some(AutoExit {
            enabled: false,
            delay: 101,
        });
        key.groups[0].push(member);
        profile.pages[0].keys.insert(1, key);

        let loaded = Profile::from_json(&profile.to_json().unwrap()).unwrap();
        let key = &loaded.pages[0].keys[&1];
        assert_eq!(
            key.auto_exit,
            Some(AutoExit {
                enabled: true,
                delay: 60
            })
        );
        assert_eq!(
            key.groups[0][0].auto_exit,
            Some(AutoExit {
                enabled: false,
                delay: 60
            })
        );
    }

    #[test]
    fn copied_and_normalized_folders_keep_auto_exit_preferences() {
        let mut profile = Profile::new("Scenes", DeviceKind::Mk2);
        profile.pages[0]
            .keys
            .insert(1, KeyConfig::new(FOLDER_ACTION, json!({})));
        profile.normalize_folders();
        let folder = &mut profile.folders[0];
        folder.keys.get_mut(&0).unwrap().auto_exit = Some(AutoExit {
            enabled: true,
            delay: 3,
        });
        let mut member = KeyConfig::new("com.example.action", json!({}));
        member.auto_exit = Some(AutoExit {
            enabled: false,
            delay: 12,
        });
        folder.keys.insert(1, member);
        let expected = folder.keys.clone();

        let copy = profile.duplicate("Copy".into());
        assert_eq!(copy.folders[0].keys, expected);
        // Pasting the folder key makes an independent folder with the same settings.
        let pasted = profile.pages[0].keys[&1].clone();
        profile.pages[0].keys.insert(2, pasted);
        profile.normalize_folders();
        assert_eq!(profile.folders.len(), 2);
        assert!(profile.folders.iter().all(|folder| folder.keys == expected));
    }

    #[test]
    fn copies_keep_pages_actions_and_their_own_folder_links() {
        let mut source = Profile::new("Scenes", DeviceKind::Mk2);
        source.add_page();
        source.pages[0].name = "Main".into();
        source.pages[0].icon = Some("main.svg".into());
        source.pages[0]
            .keys
            .insert(0, KeyConfig::new(FOLDER_ACTION, serde_json::json!({})));
        source.normalize_folders();
        source.folders[0].keys.insert(
            1,
            KeyConfig::new("com.example.action", serde_json::json!({"value": 42})),
        );
        let copy = source.duplicate("Scenes copy".into());
        assert_ne!(copy.id, source.id);
        assert_eq!(copy.name, "Scenes copy");
        assert_eq!(copy.pages.len(), 2);
        for (a, b) in copy.pages.iter().zip(&source.pages) {
            assert_ne!(a.id, b.id);
            assert_eq!((&a.name, &a.icon), (&b.name, &b.icon));
        }
        assert_ne!(copy.folders[0].id, source.folders[0].id);
        assert_eq!(
            copy.pages[0].keys[&0].folder(),
            Some(copy.folders[0].id.as_str())
        );
        assert_eq!(copy.folders[0].keys, source.folders[0].keys);
        assert_eq!(Profile::from_json(&copy.to_json().unwrap()).unwrap(), copy);
    }

    #[test]
    fn free_names_use_copy_suffixes_and_respect_model_case_and_length() {
        let mut profile = Profile::new("Scenes", DeviceKind::Mk2);
        let mut existing = vec![Profile::new("Scenes", DeviceKind::Mini)];
        assert_eq!(profile.available_name(&existing), "Scenes");
        existing.push(Profile::new("SCENES", DeviceKind::Mk2));
        assert_eq!(profile.available_name(&existing), "Scenes copy");
        existing.push(Profile::new("Scenes copy", DeviceKind::Mk2));
        existing.push(Profile::new("Scenes copy 2", DeviceKind::Mk2));
        profile.name = "Scenes copy 2".into();
        assert_eq!(profile.available_name(&existing), "Scenes copy 1");
        let same_id = profile.clone();
        assert_eq!(profile.available_name(&[same_id]), "Scenes copy 2");
        assert_eq!(
            profile.available_name(&[Profile::new("Scenes copy 2", DeviceKind::Mk2)]),
            "Scenes"
        );
        profile.name = "Ж".repeat(MAX_PROFILE_NAME_CHARS);
        existing.push(Profile::new(profile.name.clone(), DeviceKind::Mk2));
        let name = profile.available_name(&existing);
        assert_eq!(name.chars().count(), MAX_PROFILE_NAME_CHARS);
        assert!(name.ends_with(" copy"));
        profile.name = "Photocopy".into();
        existing.push(Profile::new("Photocopy", DeviceKind::Mk2));
        assert_eq!(profile.available_name(&existing), "Photocopy copy");
    }

    #[test]
    fn adding_pages_preserves_existing_pages_and_stops_at_the_limit() {
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.pages[0]
            .keys
            .insert(1, KeyConfig::new("com.example.a", serde_json::json!({})));
        let original = profile.pages[0].clone();
        let id = profile.add_page().unwrap();
        assert_eq!(profile.pages[0], original);
        assert_eq!(profile.pages[1].id, id);
        assert!(profile.pages[1].keys.is_empty());
        while profile.add_page().is_some() {}
        assert_eq!(profile.pages.len(), MAX_PAGES);
        assert!(profile.add_page().is_none());
    }

    #[test]
    fn deleting_pages_removes_their_folders_but_preserves_the_last_page() {
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        let first = profile.pages[0].id.clone();
        let second = profile.add_page().unwrap();
        for page in &mut profile.pages {
            page.keys
                .insert(1, KeyConfig::new(FOLDER_ACTION, json!({})));
        }
        profile.normalize_folders();
        let remaining_folder = profile.pages[1].keys[&1].folder().unwrap().to_owned();
        assert_eq!(profile.remove_page("missing"), None);
        assert_eq!(profile.remove_page(&first), Some(0));
        assert_eq!(profile.pages[0].id, second);
        assert_eq!(profile.folders.len(), 1);
        assert_eq!(profile.folders[0].id, remaining_folder);
        let before = profile.clone();
        assert_eq!(profile.remove_page(&second), None);
        assert_eq!(profile, before);
        assert_eq!(
            Profile::from_json(&profile.to_json().unwrap()).unwrap(),
            profile
        );
    }
    #[test]
    fn round_trip() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        let mut key = KeyConfig::new(
            "com.deckly.system.website",
            json!({"url": "https://example.com"}),
        );
        key.title = "Site".into();
        p.pages[0].keys.insert(3, key);
        p.applications.push(AppRule::new("Spotify", "Spotify.exe"));
        p.applications
            .push(AppRule::new("Game", "game.exe").with_icon(r"C:\Games\game.exe"));
        let text = p.to_json().unwrap();
        assert_eq!(Profile::from_json(&text).unwrap(), p);
        assert_eq!(text.matches("\"icon\"").count(), 1);
    }

    #[test]
    fn older_profile_files_list_no_applications_and_new_empty_ones_stay_small() {
        let text = r#"{"id":"a","name":"x","device":"mk2","pages":[{"id":"p"}]}"#;
        let p = Profile::from_json(text).unwrap();
        assert!(p.applications.is_empty());
        assert!(!p.to_json().unwrap().contains("applications"));
    }

    #[test]
    fn a_rule_matches_by_the_last_part_of_a_name_in_any_case() {
        let rule = AppRule::new("Discord", r"C:\Users\me\AppData\Local\Discord\Discord.exe");
        assert_eq!(rule.id, "Discord.exe");
        let running = r"C:\Users\me\AppData\Local\Discord\app-1.0.9\discord.EXE";
        assert!(rule.matches(&["other".into(), running.into()]));
        assert!(!rule.matches(&[r"C:\Discord.exe.bak".into()]));
        assert!(!rule.matches(&[]));

        let package = AppRule::new("Calculator", "Microsoft.Calc_8wek!App");
        assert!(package.matches(&["microsoft.calc_8wek!app".into()]));
        let bundle = AppRule::new("Music", "/Applications/Music.app/");
        assert_eq!(bundle.id, "Music.app");
        assert!(bundle.matches(&["/System/Applications/Music.app".into()]));
        assert!(!bundle.matches(&["com.apple.Music".into()]));
    }

    #[test]
    fn a_rule_without_a_name_shows_its_id() {
        assert_eq!(AppRule::new("  ", "/usr/bin/gimp").name, "gimp");
    }

    #[test]
    fn sanitize_bounds_applications_and_drops_empty_ones() {
        let long = "x".repeat(1000);
        let huge = "x".repeat(MAX_APPLICATION_ICON_BYTES + 1);
        let rules: Vec<String> = (0..=MAX_PROFILE_APPLICATIONS)
            .map(|i| format!(r#"{{"name":"{long}","id":"/a/b/{i}.exe","icon":"{huge}"}}"#))
            .collect();
        let text = format!(
            r#"{{"id":"a","name":"x","device":"mk2","pages":[],
            "applications":[{{"name":"none","id":" / "}},{}]}}"#,
            rules.join(",")
        );
        let p = Profile::from_json(&text).unwrap();
        assert_eq!(p.applications.len(), MAX_PROFILE_APPLICATIONS);
        assert_eq!(p.applications[0].id, "0.exe");
        assert!(p.applications[0].icon.is_empty());
        assert_eq!(
            p.applications[0].name.chars().count(),
            MAX_APPLICATION_CHARS
        );
    }

    #[test]
    fn a_copy_does_not_claim_the_applications_of_its_original() {
        let mut p = Profile::new("Game", DeviceKind::Mk2);
        p.applications.push(AppRule::new("Game", "game.exe"));
        assert!(p.duplicate("Copy".into()).applications.is_empty());
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
    fn wallpapers_round_trip_and_old_pages_have_none() {
        let text = r#"{"id":"a","name":"x","device":"mk2","pages":[{"id":"p","keys":{}}]}"#;
        let mut p = Profile::from_json(text).unwrap();
        assert_eq!(p.pages[0].wallpaper, None);
        assert!(!p.to_json().unwrap().contains("wallpaper"));
        p.pages[0].wallpaper = Some(Wallpaper {
            zoom: 250,
            x: 100,
            y: 900,
            ..Wallpaper::new("images/a/wall.png".into())
        });
        let back = Profile::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn profile_wallpaper_respects_page_and_folder_overrides_after_saving() {
        let mut p = Profile::new(DEFAULT_PROFILE_NAME, DeviceKind::Mk2);
        let default = Wallpaper::new("default.png".into());
        let local = Wallpaper::new("folder.png".into());
        p.wallpaper = Some(default.clone());
        assert!(!p.is_placeholder());
        p.add_page();
        p.pages[1].wallpaper_disabled = true;
        p.pages[0]
            .keys
            .insert(0, KeyConfig::new(FOLDER_ACTION, serde_json::json!({})));
        p.normalize_folders();
        assert_eq!(
            p.folders[0].effective_wallpaper(p.wallpaper.as_ref()),
            Some(&default)
        );
        p.folders[0].wallpaper = Some(local.clone());
        let p = Profile::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(
            p.pages[0].effective_wallpaper(p.wallpaper.as_ref()),
            Some(&default)
        );
        assert_eq!(p.pages[1].effective_wallpaper(p.wallpaper.as_ref()), None);
        assert_eq!(
            p.folders[0].effective_wallpaper(p.wallpaper.as_ref()),
            Some(&local)
        );
        let mut disabled = p.folders[0].clone();
        disabled.wallpaper_disabled = true;
        assert_eq!(disabled.effective_wallpaper(p.wallpaper.as_ref()), None);
    }

    #[test]
    fn wallpapers_are_brought_within_limits() {
        let text = r#"{"id":"a","name":"x","device":"mk2","pages":[
            {"id":"p","wallpaper":{"image":"w.png","zoom":9999,"x":5000,"y":7}},
            {"id":"q","wallpaper":{"image":"w.png"}},
            {"id":"r","wallpaper":{"image":"","zoom":200}}]}"#;
        let p = Profile::from_json(text).unwrap();
        let w = p.pages[0].wallpaper.as_ref().unwrap();
        assert_eq!((w.zoom, w.x, w.y), (Wallpaper::MAX_ZOOM, Wallpaper::END, 7));
        assert_eq!(p.pages[1].wallpaper, Some(Wallpaper::new("w.png".into())));
        assert_eq!(p.pages[2].wallpaper, None);
        let zero = r#"{"id":"a","name":"x","device":"mk2","pages":[
            {"id":"p","wallpaper":{"image":"w.png","zoom":0}}]}"#;
        let p = Profile::from_json(zero).unwrap();
        assert_eq!(
            p.pages[0].wallpaper.as_ref().unwrap().zoom,
            Wallpaper::MIN_ZOOM
        );
    }

    #[test]
    fn wallpapers_are_pictures_of_the_profile() {
        let mut p = Profile::new("Walls", DeviceKind::Mk2);
        p.pages[0].wallpaper = Some(Wallpaper::new("a.png".into()));
        p.add_page();
        p.pages[1].wallpaper = Some(Wallpaper::new("b.png".into()));
        let mut seen = Vec::new();
        p.for_each_picture(|picture| {
            if let Some(path) = picture.take() {
                seen.push(path.clone());
                // Pictures that go away take their wallpaper with them.
                *picture = (path != Path::new("b.png")).then(|| path.with_extension("jpg"));
            }
        });
        assert_eq!(seen, [PathBuf::from("a.png"), PathBuf::from("b.png")]);
        assert_eq!(
            p.pages[0].wallpaper.as_ref().map(|w| w.image.clone()),
            Some(PathBuf::from("a.jpg"))
        );
        assert_eq!(p.pages[1].wallpaper, None);
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
            keys[&1].title_style.as_ref().map(|s| s.size),
            Some(TitleStyle::MAX_SIZE)
        );
        assert_eq!(keys[&2].title_style, None);
        assert!(!p.to_json().unwrap().contains("\"title_style\": null"));
    }

    #[test]
    fn title_styles_without_font_options_keep_their_look() {
        let text = r#"{"show":false,"color":[1,2,3],"size":20,"align":"top"}"#;
        let style: TitleStyle = serde_json::from_str(text).unwrap();
        assert_eq!(style.font, "");
        assert!(style.bold);
        assert!(!style.italic && !style.underline);
        // Untouched font options are not written, so the file is unchanged.
        let back: serde_json::Value = serde_json::to_value(&style).unwrap();
        assert_eq!(
            back,
            serde_json::from_str::<serde_json::Value>(text).unwrap()
        );
    }

    #[test]
    fn font_options_round_trip() {
        let mut p = Profile::new("Fonts", DeviceKind::Mk2);
        let mut key = KeyConfig::new("a", serde_json::json!({}));
        key.title_style = Some(TitleStyle {
            font: "DejaVu Serif".into(),
            bold: false,
            italic: true,
            underline: true,
            ..TitleStyle::default()
        });
        p.pages[0].keys.insert(0, key);
        let back = Profile::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn font_names_are_trimmed_and_limited() {
        let long = "F".repeat(TitleStyle::MAX_FONT_CHARS + 10);
        let mut style = TitleStyle {
            font: format!("  {long}\n"),
            ..TitleStyle::default()
        };
        style.sanitize();
        assert_eq!(style.font, "F".repeat(TitleStyle::MAX_FONT_CHARS));
        style.font = " Noto\u{7}Sans ".into();
        style.sanitize();
        assert_eq!(style.font, "NotoSans");
        style.font = "Noto Sans".into();
        style.sanitize();
        assert_eq!(style.font, "Noto Sans");
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
        let mut following = new();
        following
            .applications
            .push(AppRule::new("Game", "game.exe"));
        assert!(!following.is_placeholder());
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

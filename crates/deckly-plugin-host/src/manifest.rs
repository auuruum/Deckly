//! Plugin manifests (`manifest.json` in a `<uuid>.sdPlugin` folder).
//!
//! Manifests come from third parties and are read leniently: a field with an
//! unexpected type reads as missing instead of rejecting the plugin, and
//! every path is checked to stay inside the plugin folder.

use serde_json::Value;
use std::fmt;
use std::path::{Component, Path, PathBuf};

pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const MAX_ACTIONS: usize = 256;
pub const MAX_STATES: usize = 16;
const MAX_TEXT_CHARS: usize = 500;
/// Entries looked at when matching one path part ignoring case.
const MAX_FOLDER_ENTRIES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub uuid: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub icon: String,
    pub category: String,
    pub category_icon: String,
    pub code_path: String,
    pub code_path_win: String,
    pub code_path_mac: String,
    /// `CodePathLin`, which some plugins add next to the official fields.
    pub code_path_lin: String,
    pub property_inspector: String,
    pub sdk_version: u64,
    /// `Nodejs.Version`, when the plugin asks for Node.
    pub node_version: Option<String>,
    /// `Nodejs.Debug`.
    pub node_debug: String,
    /// `OS[].Platform`: "windows", "mac".
    pub platforms: Vec<String>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub uuid: String,
    pub name: String,
    pub icon: String,
    pub tooltip: String,
    pub states: Vec<State>,
    pub property_inspector: String,
    /// "Keypad", "Encoder", "Neo". Defaults to keypad only.
    pub controllers: Vec<String>,
    pub automatic_states: bool,
    pub visible: bool,
    pub user_title: bool,
    pub in_multi_actions: bool,
    pub in_key_logic: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub image: String,
    pub title: String,
    pub show_title: bool,
}

/// How a plugin's code runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A JavaScript file run by Node.
    Node(PathBuf),
    /// An executable for this platform.
    Native(PathBuf),
    /// A Windows executable on another platform, run through Wine.
    Windows(PathBuf),
    /// An HTML page run by a web engine.
    Html(PathBuf),
}

#[derive(Debug)]
pub enum ManifestError {
    Io(std::io::Error),
    TooLarge(u64),
    /// Marketplace installs keep the manifest encrypted.
    Encrypted,
    Parse(String),
    Invalid(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "cannot read manifest.json: {e}"),
            ManifestError::TooLarge(n) => write!(f, "manifest.json is too large ({n} bytes)"),
            ManifestError::Encrypted => write!(
                f,
                "this copy is encrypted by the official marketplace and cannot be read; \
                 get the plugin from its author's own download"
            ),
            ManifestError::Parse(e) => write!(f, "manifest.json is not valid JSON: {e}"),
            ManifestError::Invalid(e) => write!(f, "invalid manifest: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Plugin and action ids: reverse-DNS names.
pub fn valid_uuid(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !id.starts_with('.')
        && !id.contains("..")
}

impl Manifest {
    /// Reads `manifest.json` from a plugin folder. The folder name (without
    /// `.sdPlugin`) is the plugin id when the manifest has none.
    pub fn load(dir: &Path) -> Result<Self, ManifestError> {
        let path = dir.join("manifest.json");
        let len = std::fs::metadata(&path).map_err(ManifestError::Io)?.len();
        if len > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge(len));
        }
        let bytes = std::fs::read(&path).map_err(ManifestError::Io)?;
        let folder = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".sdPlugin"))
            .unwrap_or_default();
        Self::parse(&bytes, folder)
    }

    pub fn parse(bytes: &[u8], folder_id: &str) -> Result<Self, ManifestError> {
        if bytes.starts_with(b"ELGATO") {
            return Err(ManifestError::Encrypted);
        }
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let v: Value =
            serde_json::from_slice(bytes).map_err(|e| ManifestError::Parse(e.to_string()))?;
        if !v.is_object() {
            return Err(ManifestError::Invalid("not an object".into()));
        }
        let mut uuid = text(&v, "UUID");
        if uuid.is_empty() {
            uuid = folder_id.to_owned();
        }
        if !valid_uuid(&uuid) {
            return Err(ManifestError::Invalid(format!("bad plugin id \"{uuid}\"")));
        }
        let name = text(&v, "Name");
        if name.is_empty() {
            return Err(ManifestError::Invalid("no Name".into()));
        }
        let actions: Vec<Action> = v
            .get("Actions")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .take(MAX_ACTIONS)
                    .filter_map(parse_action)
                    .collect()
            })
            .unwrap_or_default();
        let node = v.get("Nodejs").filter(|n| n.is_object());
        Ok(Self {
            uuid,
            name,
            version: text(&v, "Version"),
            author: text(&v, "Author"),
            description: text(&v, "Description"),
            icon: text(&v, "Icon"),
            category: text(&v, "Category"),
            category_icon: text(&v, "CategoryIcon"),
            code_path: text(&v, "CodePath"),
            code_path_win: text(&v, "CodePathWin"),
            code_path_mac: text(&v, "CodePathMac"),
            code_path_lin: text(&v, "CodePathLin"),
            property_inspector: text(&v, "PropertyInspectorPath"),
            sdk_version: v.get("SDKVersion").and_then(Value::as_u64).unwrap_or(2),
            node_version: node.map(|n| text(n, "Version")),
            node_debug: node.map(|n| text(n, "Debug")).unwrap_or_default(),
            platforms: v
                .get("OS")
                .and_then(Value::as_array)
                .map(|list| list.iter().map(|o| text(o, "Platform")).take(4).collect())
                .unwrap_or_default(),
            actions,
        })
    }

    /// The name the action list groups this plugin's actions under.
    pub fn category_name(&self) -> &str {
        if self.category.is_empty() {
            &self.name
        } else {
            &self.category
        }
    }

    pub fn action(&self, uuid: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.uuid == uuid)
    }

    /// The code paths to try here: this platform's own, the general one,
    /// then other platforms' as a last resort. A JavaScript or HTML plugin
    /// runs anywhere, and a Windows program may run through Wine.
    fn code_paths(&self) -> Vec<&str> {
        let (win, mac, lin) = (
            self.code_path_win.as_str(),
            self.code_path_mac.as_str(),
            self.code_path_lin.as_str(),
        );
        let general = self.code_path.as_str();
        let order = if cfg!(windows) {
            [win, general, mac, lin]
        } else if cfg!(target_os = "macos") {
            [mac, general, win, lin]
        } else {
            [lin, general, win, mac]
        };
        order.into_iter().filter(|p| !p.is_empty()).collect()
    }

    /// Works out how to run the plugin on this platform.
    pub fn entry(&self, dir: &Path) -> Result<Entry, String> {
        let mut first_error = None;
        for code in self.code_paths() {
            match code_entry(dir, code) {
                Ok(entry) => return Ok(entry),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| format!("no code for {}", current_platform())))
    }
}

fn code_entry(dir: &Path, code: &str) -> Result<Entry, String> {
    let path = safe_join(dir, code).ok_or_else(|| format!("bad code path \"{code}\""))?;
    // Windows plugins may leave out ".exe", and their names often contain
    // dots ("com.example.tool"), so append rather than replace.
    let Some(path) = find_file(dir, code).or_else(|| find_file(dir, &format!("{code}.exe"))) else {
        return Err(format!("{} is missing", path.display()));
    };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "js" | "mjs" | "cjs" => return Ok(Entry::Node(path)),
        "html" | "htm" => return Ok(Entry::Html(path)),
        _ => {}
    }
    match program_kind(&path) {
        ProgramKind::ThisPlatform => Ok(Entry::Native(path)),
        ProgramKind::Windows => Ok(Entry::Windows(path)),
        ProgramKind::Other => Err(format!(
            "this plugin's program is not made for {}",
            current_platform()
        )),
    }
}

/// Plugins built on BarRaider's helper library check that the signed
/// official app started them and turn themselves off otherwise
/// (docs/clean-room/2026-10-28-plugin-host.md).
pub fn needs_official_app(dir: &Path) -> bool {
    dir.join("BRUtils.dll").is_file()
}

/// The platform name used in manifests and the registration info.
pub fn current_platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ProgramKind {
    ThisPlatform,
    /// A Windows program on another platform.
    Windows,
    Other,
}

/// Reads an executable's header, so a program for another system is told
/// apart before it fails to start.
fn program_kind(path: &Path) -> ProgramKind {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic));
    if read.is_err() {
        return ProgramKind::Other;
    }
    let windows = magic.starts_with(b"MZ");
    let here = if cfg!(windows) {
        windows
    } else if cfg!(target_os = "macos") {
        matches!(
            magic,
            [0xCF, 0xFA, 0xED, 0xFE] | [0xCE, 0xFA, 0xED, 0xFE] | [0xCA, 0xFE, 0xBA, 0xBE]
        ) || magic.starts_with(b"#!")
    } else {
        magic == *b"\x7FELF" || magic.starts_with(b"#!")
    };
    match (here, windows) {
        (true, _) => ProgramKind::ThisPlatform,
        (false, true) => ProgramKind::Windows,
        (false, false) => ProgramKind::Other,
    }
}

/// Joins a manifest path onto the plugin folder. Absolute paths and paths
/// leaving the folder are refused. Backslashes count as separators.
pub fn safe_join(dir: &Path, relative: &str) -> Option<PathBuf> {
    let relative = relative.replace('\\', "/");
    let rel = Path::new(relative.trim_start_matches("./"));
    let mut out = dir.to_path_buf();
    let mut parts = 0;
    for c in rel.components() {
        match c {
            Component::Normal(p) => {
                out.push(p);
                parts += 1;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    (parts > 0).then_some(out)
}

/// Finds a file a manifest names. Plugins are mostly written on Windows,
/// where "Images/Key.png" also opens "images/key.PNG", so on a miss each part
/// of the path is matched ignoring case.
pub fn find_file(dir: &Path, relative: &str) -> Option<PathBuf> {
    let exact = safe_join(dir, relative)?;
    if exact.is_file() {
        return Some(exact);
    }
    let mut found = dir.to_path_buf();
    for part in exact.strip_prefix(dir).ok()?.components() {
        let wanted = part.as_os_str();
        let direct = found.join(wanted);
        found = if direct.exists() {
            direct
        } else {
            let wanted = wanted.to_str()?;
            let name = std::fs::read_dir(&found)
                .ok()?
                .flatten()
                .take(MAX_FOLDER_ENTRIES)
                .map(|entry| entry.file_name())
                .find(|name| {
                    name.to_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(wanted))
                })?;
            found.join(name)
        };
    }
    found.is_file().then_some(found)
}

/// Finds the file for a manifest image path, which leaves out the extension:
/// the high-resolution PNG, the PNG, then SVG.
pub fn image_file(dir: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let base = safe_join(dir, relative)?;
    let name = base.file_name()?.to_str()?.to_owned();
    let has_ext = Path::new(&name)
        .extension()
        .is_some_and(|e| matches!(e.to_str(), Some("png" | "svg" | "jpg" | "jpeg")));
    let candidates = if has_ext {
        vec![name.clone()]
    } else {
        vec![
            format!("{name}@2x.png"),
            format!("{name}.png"),
            format!("{name}.svg"),
        ]
    };
    let folder = base.parent()?.strip_prefix(dir).ok()?;
    candidates
        .into_iter()
        .find_map(|c| find_file(dir, folder.join(c).to_str()?))
}

fn parse_action(v: &Value) -> Option<Action> {
    let uuid = text(v, "UUID");
    if !valid_uuid(&uuid) {
        return None;
    }
    let mut controllers: Vec<String> = v
        .get("Controllers")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .take(4)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if controllers.is_empty() {
        controllers.push("Keypad".into());
    }
    let states = v
        .get("States")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .take(MAX_STATES)
                .map(|s| State {
                    image: text(s, "Image"),
                    title: text(s, "Title"),
                    show_title: flag(s, "ShowTitle", true),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Action {
        name: Some(text(v, "Name"))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| uuid.clone()),
        uuid,
        icon: text(v, "Icon"),
        tooltip: text(v, "Tooltip"),
        states,
        property_inspector: text(v, "PropertyInspectorPath"),
        controllers,
        automatic_states: !flag(v, "DisableAutomaticStates", false),
        visible: flag(v, "VisibleInActionsList", true),
        user_title: flag(v, "UserTitleEnabled", true),
        in_multi_actions: flag(v, "SupportedInMultiActions", true),
        in_key_logic: flag(v, "SupportedInKeyLogicActions", true),
    })
}

impl Action {
    pub fn on_keys(&self) -> bool {
        self.controllers.iter().any(|c| c == "Keypad")
    }
}

fn text(v: &Value, field: &str) -> String {
    let s = match v.get(field) {
        Some(Value::String(s)) => s.trim(),
        _ => return String::new(),
    };
    s.chars().take(MAX_TEXT_CHARS).collect()
}

fn flag(v: &Value, field: &str, default: bool) -> bool {
    v.get(field).and_then(Value::as_bool).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "Name": "Sample", "Version": "1.2.0.0", "Author": "Someone",
        "CodePath": "bin/plugin.js", "Nodejs": {"Version": "20", "Debug": "enabled"},
        "SDKVersion": 2, "OS": [{"Platform": "windows", "MinimumVersion": "10"}],
        "Category": "Things", "PropertyInspectorPath": "ui/pi.html",
        "Actions": [
            {"UUID": "com.example.sample.one", "Name": "One", "Icon": "imgs/one",
             "States": [{"Image": "imgs/key", "Title": "Hi"}, {"Image": "imgs/key2", "ShowTitle": false}]},
            {"UUID": "com.example.sample.dial", "Controllers": ["Encoder"], "States": []},
            {"UUID": "../evil", "Name": "Bad"},
            {"Name": "No id"}
        ]
    }"#;

    #[test]
    fn parses_fields_and_skips_bad_actions() {
        let m = Manifest::parse(SAMPLE.as_bytes(), "com.example.sample").unwrap();
        assert_eq!(m.uuid, "com.example.sample");
        assert_eq!(m.node_version.as_deref(), Some("20"));
        assert_eq!(m.node_debug, "enabled");
        assert_eq!(m.platforms, ["windows"]);
        assert_eq!(m.category_name(), "Things");
        assert_eq!(m.actions.len(), 2);
        let one = &m.actions[0];
        assert_eq!(one.states.len(), 2);
        assert_eq!(one.states[0].title, "Hi");
        assert!(!one.states[1].show_title);
        assert!(one.on_keys() && one.automatic_states && one.visible);
        assert!(!m.actions[1].on_keys());
        assert_eq!(m.actions[1].name, "com.example.sample.dial");
    }

    #[test]
    fn manifest_id_wins_over_folder_and_bom_is_skipped() {
        let text = "\u{FEFF}{\"UUID\":\"com.a.b\",\"Name\":\"X\"}";
        let m = Manifest::parse(text.as_bytes(), "other").unwrap();
        assert_eq!(m.uuid, "com.a.b");
        assert!(m.actions.is_empty());
    }

    #[test]
    fn rejects_encrypted_garbage_and_bad_ids() {
        assert!(matches!(
            Manifest::parse(b"ELGATO\x01\x00junk", "x"),
            Err(ManifestError::Encrypted)
        ));
        assert!(Manifest::parse(b"[1,2]", "x").is_err());
        assert!(Manifest::parse(b"not json", "x").is_err());
        assert!(Manifest::parse(br#"{"Name":"X"}"#, "").is_err());
        assert!(Manifest::parse(br#"{"Name":"X","UUID":"a/b"}"#, "").is_err());
        assert!(Manifest::parse(br#"{"UUID":"a.b"}"#, "").is_err());
        // Wrong field types read as missing.
        let m = Manifest::parse(
            br#"{"UUID":"a.b","Name":"X","Version":5,"Actions":"no"}"#,
            "",
        )
        .unwrap();
        assert_eq!(m.version, "");
    }

    #[test]
    fn paths_stay_inside_the_plugin() {
        let dir = Path::new("plugins/x.sdPlugin");
        assert_eq!(
            safe_join(dir, "bin\\plugin.js"),
            Some(dir.join("bin").join("plugin.js"))
        );
        assert_eq!(safe_join(dir, "./a"), Some(dir.join("a")));
        assert_eq!(safe_join(dir, "../a"), None);
        assert_eq!(safe_join(dir, "a/../../b"), None);
        assert_eq!(safe_join(dir, "/etc/passwd"), None);
        assert_eq!(safe_join(dir, ""), None);
        if cfg!(windows) {
            assert_eq!(safe_join(dir, "C:\\Windows\\x.exe"), None);
        }
    }

    #[test]
    fn entries_follow_the_code_path() {
        let dir = std::env::temp_dir().join(format!("deckly-entry-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/plugin.js"), "").unwrap();
        std::fs::write(dir.join("page.html"), "").unwrap();
        std::fs::write(dir.join("tool.exe"), b"MZ\x90\x00").unwrap();
        std::fs::write(dir.join("com.example.tool.exe"), b"MZ\x90\x00").unwrap();
        let mut m = Manifest::parse(SAMPLE.as_bytes(), "com.example.sample").unwrap();
        assert_eq!(m.entry(&dir), Ok(Entry::Node(dir.join("bin/plugin.js"))));
        m.code_path = "page.html".into();
        assert_eq!(m.entry(&dir), Ok(Entry::Html(dir.join("page.html"))));
        m.code_path = "missing.js".into();
        assert!(m.entry(&dir).is_err());
        m.code_path = "tool".into();
        if cfg!(windows) {
            assert_eq!(m.entry(&dir), Ok(Entry::Native(dir.join("tool.exe"))));
            // Dots in the name are not an extension.
            m.code_path = "com.example.tool".into();
            assert_eq!(
                m.entry(&dir),
                Ok(Entry::Native(dir.join("com.example.tool.exe")))
            );
        }
        assert!(!needs_official_app(&dir));
        std::fs::write(dir.join("BRUtils.dll"), "").unwrap();
        assert!(needs_official_app(&dir));
        m.code_path = "tool.exe".into();
        // A Windows program goes through Wine elsewhere.
        let tool = dir.join("tool.exe");
        let expected = if cfg!(windows) {
            Entry::Native(tool)
        } else {
            Entry::Windows(tool)
        };
        assert_eq!(m.entry(&dir), Ok(expected));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deckly-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_paths(general: &str, win: &str, mac: &str, lin: &str) -> Manifest {
        let mut m = Manifest::parse(SAMPLE.as_bytes(), "com.example.sample").unwrap();
        m.code_path = general.into();
        m.code_path_win = win.into();
        m.code_path_mac = mac.into();
        m.code_path_lin = lin.into();
        m
    }

    #[test]
    fn this_platform_comes_first_then_the_rest() {
        let m = with_paths("general", "win", "mac", "lin");
        let first = if cfg!(windows) {
            "win"
        } else if cfg!(target_os = "macos") {
            "mac"
        } else {
            "lin"
        };
        assert_eq!(m.code_paths()[0], first);
        assert_eq!(m.code_paths()[1], "general");
        assert_eq!(m.code_paths().len(), 4);
        assert!(with_paths("", "", "", "").code_paths().is_empty());
    }

    #[test]
    fn a_script_for_another_platform_still_runs() {
        let dir = sample_dir("other-code");
        std::fs::write(dir.join("win.js"), "").unwrap();
        let m = with_paths("", "win.js", "", "");
        assert_eq!(m.entry(&dir), Ok(Entry::Node(dir.join("win.js"))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_code_path_falls_through_to_the_next() {
        let dir = sample_dir("next-code");
        std::fs::write(dir.join("plugin.js"), "").unwrap();
        let m = with_paths("gone.js", "plugin.js", "plugin.js", "plugin.js");
        assert_eq!(m.entry(&dir), Ok(Entry::Node(dir.join("plugin.js"))));
        let none = with_paths("", "", "", "");
        assert!(none.entry(&dir).is_err_and(|e| e.starts_with("no code")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A manifest may leave ".exe" out on any system, not only Windows.
    #[cfg(not(windows))]
    #[test]
    fn windows_programs_without_the_extension_are_found_everywhere() {
        let dir = sample_dir("no-ext");
        std::fs::write(dir.join("com.example.tool.exe"), b"MZ\x90\x00").unwrap();
        let m = with_paths("com.example.tool", "", "", "");
        assert_eq!(
            m.entry(&dir),
            Ok(Entry::Windows(dir.join("com.example.tool.exe")))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_for_a_third_system_is_refused() {
        let dir = sample_dir("foreign");
        std::fs::write(dir.join("tool"), b"\x00\x01\x02\x03\x04").unwrap();
        let m = with_paths("tool", "", "", "");
        assert!(m.entry(&dir).is_err_and(|e| e.contains("not made for")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Windows and default macOS volumes ignore case; the search must then
    /// find the file as well, on the first try.
    #[test]
    fn files_are_found_whatever_case_the_manifest_uses() {
        let dir = sample_dir("case");
        std::fs::create_dir_all(dir.join("Bin/Sub")).unwrap();
        std::fs::write(dir.join("Bin/Sub/Plugin.JS"), "").unwrap();
        std::fs::write(dir.join("Bin/Key.png"), "").unwrap();
        let found = find_file(&dir, "bin\\sub\\plugin.js").unwrap();
        assert!(found.is_file());
        assert_eq!(found.parent(), Some(dir.join("Bin/Sub").as_path()));
        assert!(find_file(&dir, "BIN/key.PNG").is_some());
        assert!(image_file(&dir, "bin/key").is_some());
        assert_eq!(find_file(&dir, "bin/none.js"), None);
        assert_eq!(find_file(&dir, "../bin/sub/plugin.js"), None);
        assert_eq!(find_file(&dir, "bin"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_or_unreadable_folder_is_a_plain_miss() {
        let dir = sample_dir("miss");
        assert_eq!(find_file(&dir, "no/such/folder/file"), None);
        assert_eq!(find_file(&dir.join("gone"), "file"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn images_prefer_high_resolution() {
        let dir = std::env::temp_dir().join(format!("deckly-img-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("imgs")).unwrap();
        std::fs::write(dir.join("imgs/a.png"), "").unwrap();
        std::fs::write(dir.join("imgs/a@2x.png"), "").unwrap();
        std::fs::write(dir.join("imgs/b.svg"), "").unwrap();
        assert_eq!(image_file(&dir, "imgs/a"), Some(dir.join("imgs/a@2x.png")));
        assert_eq!(image_file(&dir, "imgs/b"), Some(dir.join("imgs/b.svg")));
        assert_eq!(image_file(&dir, "imgs/a.png"), Some(dir.join("imgs/a.png")));
        assert_eq!(image_file(&dir, "imgs/c"), None);
        assert_eq!(image_file(&dir, "../imgs/a"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

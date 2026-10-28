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

    /// Works out how to run the plugin on this platform.
    pub fn entry(&self, dir: &Path) -> Result<Entry, String> {
        let platform_path = if cfg!(windows) {
            &self.code_path_win
        } else if cfg!(target_os = "macos") {
            &self.code_path_mac
        } else {
            ""
        };
        let code = if platform_path.is_empty() {
            &self.code_path
        } else {
            platform_path
        };
        if code.is_empty() {
            return Err(format!("no code for {}", current_platform()));
        }
        let path = safe_join(dir, code).ok_or_else(|| format!("bad code path \"{code}\""))?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match ext.as_str() {
            "js" | "mjs" | "cjs" => return exists(path).map(Entry::Node),
            "html" | "htm" => return exists(path).map(Entry::Html),
            _ => {}
        }
        // Windows plugins may leave out ".exe".
        let path = if cfg!(windows) && ext.is_empty() && !path.is_file() {
            path.with_extension("exe")
        } else {
            path
        };
        let path = exists(path)?;
        if !native_for_this_platform(&path) {
            return Err(format!(
                "this plugin's program is not made for {}",
                current_platform()
            ));
        }
        Ok(Entry::Native(path))
    }
}

fn exists(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("{} is missing", path.display()))
    }
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

/// Checks an executable's header against this platform's format, so a
/// Windows program is reported instead of failing to start on Linux.
fn native_for_this_platform(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic));
    if read.is_err() {
        return false;
    }
    if cfg!(windows) {
        magic.starts_with(b"MZ")
    } else if cfg!(target_os = "macos") {
        matches!(
            magic,
            [0xCF, 0xFA, 0xED, 0xFE] | [0xCE, 0xFA, 0xED, 0xFE] | [0xCA, 0xFE, 0xBA, 0xBE]
        ) || magic.starts_with(b"#!")
    } else {
        magic == *b"\x7FELF" || magic.starts_with(b"#!")
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
    candidates
        .into_iter()
        .map(|c| base.with_file_name(c))
        .find(|p| p.is_file())
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
        let mut m = Manifest::parse(SAMPLE.as_bytes(), "com.example.sample").unwrap();
        assert_eq!(m.entry(&dir), Ok(Entry::Node(dir.join("bin/plugin.js"))));
        m.code_path = "page.html".into();
        assert_eq!(m.entry(&dir), Ok(Entry::Html(dir.join("page.html"))));
        m.code_path = "missing.js".into();
        assert!(m.entry(&dir).is_err());
        m.code_path = "tool".into();
        if cfg!(windows) {
            assert_eq!(m.entry(&dir), Ok(Entry::Native(dir.join("tool.exe"))));
        }
        // A Windows program is refused elsewhere.
        m.code_path = "tool.exe".into();
        assert_eq!(m.entry(&dir).is_ok(), cfg!(windows));
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

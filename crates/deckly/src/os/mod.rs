//! Desktop integration used by the built-in actions: key and text input,
//! media keys and opening files or URLs, and starting Deckly at login,
//! keeping it in the tray and running it once. Each platform lives in its
//! own module; a platform without an implementation returns an error instead
//! of silently doing nothing.

pub mod autostart;
pub mod instance;
pub mod tray;

mod command;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{media_key, open, open_url, send_hotkey, type_text};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use self::unix::{open, open_url};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::{media_key, send_hotkey, type_text};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::{media_key, send_hotkey, type_text};

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod other;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub use self::other::{media_key, send_hotkey, type_text};

/// The system's version as plain numbers, such as `10.0.19045`, for plugins.
/// .NET plugins parse it as a version and crash on anything else, so it never
/// holds text or an empty value.
pub fn system_version() -> String {
    numeric_version(&version_text()).unwrap_or_else(|| "1.0".to_owned())
}

fn version_text() -> String {
    #[cfg(windows)]
    return self::windows::version_text();
    #[cfg(target_os = "linux")]
    return std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    #[cfg(target_os = "macos")]
    return std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    String::new()
}

/// The numbers a version text starts with: `6.12.1-arch1` gives `6.12.1`.
/// At least two parts and at most four, as a .NET `Version` wants them.
fn numeric_version(text: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in text.trim().split('.').take(4) {
        let end = part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len());
        let digits = &part[..end];
        if digits.parse::<i32>().is_err() {
            break;
        }
        parts.push(digits);
        if end < part.len() {
            break;
        }
    }
    if parts.len() == 1 {
        parts.push("0");
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// A key combination in Deckly's own notation. `key` uses egui key names
/// ("A", "F5", "Space", "Left", ...).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Windows key / Super key / Command key.
    pub meta: bool,
    pub key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKey {
    PlayPause,
    Next,
    Previous,
    Stop,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl MediaKey {
    pub const ALL: [MediaKey; 7] = [
        MediaKey::PlayPause,
        MediaKey::Next,
        MediaKey::Previous,
        MediaKey::Stop,
        MediaKey::VolumeUp,
        MediaKey::VolumeDown,
        MediaKey::Mute,
    ];

    pub fn id(self) -> &'static str {
        match self {
            MediaKey::PlayPause => "play_pause",
            MediaKey::Next => "next",
            MediaKey::Previous => "previous",
            MediaKey::Stop => "stop",
            MediaKey::VolumeUp => "volume_up",
            MediaKey::VolumeDown => "volume_down",
            MediaKey::Mute => "mute",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaKey::PlayPause => "Play / Pause",
            MediaKey::Next => "Next Track",
            MediaKey::Previous => "Previous Track",
            MediaKey::Stop => "Stop",
            MediaKey::VolumeUp => "Volume Up",
            MediaKey::VolumeDown => "Volume Down",
            MediaKey::Mute => "Mute",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn versions_are_plain_numbers() {
        assert_eq!(numeric_version("10.0.19045").as_deref(), Some("10.0.19045"));
        assert_eq!(
            numeric_version(
                "6.12.1-arch1-1
"
            )
            .as_deref(),
            Some("6.12.1")
        );
        assert_eq!(numeric_version("15").as_deref(), Some("15.0"));
        assert_eq!(numeric_version("1.2.3.4.5").as_deref(), Some("1.2.3.4"));
        assert_eq!(numeric_version("1.x").as_deref(), Some("1.0"));
        assert_eq!(numeric_version(""), None);
        assert_eq!(numeric_version("linux"), None);
        assert_eq!(numeric_version("99999999999.1"), None);
    }

    /// An empty version crashed .NET plugins as soon as they started.
    #[test]
    fn this_system_reports_a_version_plugins_can_parse() {
        let version = system_version();
        let parts: Vec<&str> = version.split('.').collect();
        assert!((2..=4).contains(&parts.len()), "{version}");
        assert!(parts.iter().all(|p| p.parse::<i32>().is_ok()), "{version}");
    }
}

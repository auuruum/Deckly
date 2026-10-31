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

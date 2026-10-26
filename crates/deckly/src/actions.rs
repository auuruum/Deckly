//! Built-in actions.
//!
//! Every key behavior is an action with a stable UUID and JSON settings
//! (see AGENTS.md, "Everything is an action"). The set and grouping follow
//! the official app's built-in categories; names and icons are Deckly's own.

use crate::os::{self, Hotkey, MediaKey};
use deckly_core::KeyConfig;
use egui_phosphor::regular as icon;
use serde_json::{Value, json};

pub const HOTKEY: &str = "com.deckly.system.hotkey";
pub const OPEN: &str = "com.deckly.system.open";
pub const WEBSITE: &str = "com.deckly.system.website";
pub const TEXT: &str = "com.deckly.system.text";
pub const MULTIMEDIA: &str = "com.deckly.system.multimedia";
pub const NEXT_PAGE: &str = "com.deckly.navigation.next-page";
pub const PREVIOUS_PAGE: &str = "com.deckly.navigation.previous-page";
pub const GO_TO_PAGE: &str = "com.deckly.navigation.go-to-page";
pub const SWITCH_PROFILE: &str = "com.deckly.navigation.switch-profile";
pub const BRIGHTNESS: &str = "com.deckly.device.brightness";
pub const SLEEP: &str = "com.deckly.device.sleep";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    System,
    Navigation,
    Device,
}

impl Category {
    pub const ALL: [Category; 3] = [Category::Navigation, Category::Device, Category::System];

    pub fn label(self) -> &'static str {
        match self {
            Category::System => "System",
            Category::Navigation => "Navigation",
            Category::Device => "Device",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Category::System => icon::DESKTOP_TOWER,
            Category::Navigation => icon::ARROWS_OUT_CARDINAL,
            Category::Device => icon::SQUARES_FOUR,
        }
    }
}

pub struct ActionDef {
    pub uuid: &'static str,
    pub name: &'static str,
    pub category: Category,
    pub icon: &'static str,
    pub tooltip: &'static str,
    pub defaults: fn() -> Value,
}

pub const ACTIONS: &[ActionDef] = &[
    ActionDef {
        uuid: WEBSITE,
        name: "Website",
        category: Category::System,
        icon: icon::GLOBE,
        tooltip: "Open a URL in the default browser",
        defaults: || json!({ "url": "https://" }),
    },
    ActionDef {
        uuid: HOTKEY,
        name: "Hotkey",
        category: Category::System,
        icon: icon::KEYBOARD,
        tooltip: "Press a key combination",
        defaults: || json!({}),
    },
    ActionDef {
        uuid: OPEN,
        name: "Open",
        category: Category::System,
        icon: icon::ROCKET_LAUNCH,
        tooltip: "Open an application or file",
        defaults: || json!({ "path": "" }),
    },
    ActionDef {
        uuid: TEXT,
        name: "Text",
        category: Category::System,
        icon: icon::TEXT_T,
        tooltip: "Type a piece of text",
        defaults: || json!({ "text": "", "enter": false }),
    },
    ActionDef {
        uuid: MULTIMEDIA,
        name: "Multimedia",
        category: Category::System,
        icon: icon::PLAY_PAUSE,
        tooltip: "Media playback and volume keys",
        defaults: || json!({ "media": "play_pause" }),
    },
    ActionDef {
        uuid: NEXT_PAGE,
        name: "Next Page",
        category: Category::Navigation,
        icon: icon::ARROW_RIGHT,
        tooltip: "Go to the next page",
        defaults: || json!({}),
    },
    ActionDef {
        uuid: PREVIOUS_PAGE,
        name: "Previous Page",
        category: Category::Navigation,
        icon: icon::ARROW_LEFT,
        tooltip: "Go to the previous page",
        defaults: || json!({}),
    },
    ActionDef {
        uuid: GO_TO_PAGE,
        name: "Go to Page",
        category: Category::Navigation,
        icon: icon::HASH,
        tooltip: "Jump to a page",
        defaults: || json!({ "page": 1 }),
    },
    ActionDef {
        uuid: SWITCH_PROFILE,
        name: "Switch Profile",
        category: Category::Navigation,
        icon: icon::STACK,
        tooltip: "Switch this device to another profile",
        defaults: || json!({ "profile": "" }),
    },
    ActionDef {
        uuid: BRIGHTNESS,
        name: "Brightness",
        category: Category::Device,
        icon: icon::SUN,
        tooltip: "Change the key brightness",
        defaults: || json!({ "mode": "up", "value": 10 }),
    },
    ActionDef {
        uuid: SLEEP,
        name: "Sleep",
        category: Category::Device,
        icon: icon::MOON_STARS,
        tooltip: "Put this device to sleep now",
        defaults: || json!({}),
    },
];

pub fn find(uuid: &str) -> Option<&'static ActionDef> {
    ACTIONS.iter().find(|a| a.uuid == uuid)
}

pub fn new_key(def: &ActionDef) -> KeyConfig {
    KeyConfig::new(def.uuid, (def.defaults)())
}

/// The icon drawn on a key that has no custom image.
pub fn key_icon(key: &KeyConfig) -> &'static str {
    if key.action == MULTIMEDIA {
        return match media_setting(&key.settings) {
            Some(MediaKey::Next) => icon::SKIP_FORWARD,
            Some(MediaKey::Previous) => icon::SKIP_BACK,
            Some(MediaKey::VolumeUp) => icon::SPEAKER_HIGH,
            Some(MediaKey::VolumeDown) => icon::SPEAKER_LOW,
            Some(MediaKey::Mute) => icon::SPEAKER_X,
            _ => icon::PLAY_PAUSE,
        };
    }
    find(&key.action).map_or(icon::PUZZLE_PIECE, |a| a.icon)
}

// Settings accessors. Missing or mistyped fields read as defaults.

pub fn str_setting<'a>(settings: &'a Value, field: &str) -> &'a str {
    settings.get(field).and_then(Value::as_str).unwrap_or("")
}

pub fn bool_setting(settings: &Value, field: &str) -> bool {
    settings
        .get(field)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn int_setting(settings: &Value, field: &str, default: i64) -> i64 {
    settings
        .get(field)
        .and_then(Value::as_i64)
        .unwrap_or(default)
}

pub fn set_setting(settings: &mut Value, field: &str, value: Value) {
    if !settings.is_object() {
        *settings = json!({});
    }
    if let Some(map) = settings.as_object_mut() {
        map.insert(field.to_owned(), value);
    }
}

pub fn media_setting(settings: &Value) -> Option<MediaKey> {
    MediaKey::from_id(str_setting(settings, "media"))
}

pub fn hotkey_setting(settings: &Value) -> Hotkey {
    Hotkey {
        ctrl: bool_setting(settings, "ctrl"),
        shift: bool_setting(settings, "shift"),
        alt: bool_setting(settings, "alt"),
        meta: bool_setting(settings, "meta"),
        key: str_setting(settings, "key").to_owned(),
    }
}

pub fn set_hotkey(settings: &mut Value, hotkey: &Hotkey) {
    set_setting(settings, "ctrl", json!(hotkey.ctrl));
    set_setting(settings, "shift", json!(hotkey.shift));
    set_setting(settings, "alt", json!(hotkey.alt));
    set_setting(settings, "meta", json!(hotkey.meta));
    set_setting(settings, "key", json!(hotkey.key));
}

pub fn hotkey_label(hotkey: &Hotkey) -> String {
    let meta = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Win"
    };
    let parts: Vec<&str> = [
        (hotkey.ctrl, "Ctrl"),
        (hotkey.shift, "Shift"),
        (hotkey.alt, "Alt"),
        (hotkey.meta, meta),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .chain((!hotkey.key.is_empty()).then_some(hotkey.key.as_str()))
    .collect();
    parts.join(" + ")
}

/// What the runtime must do after an action ran.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done,
    NextPage,
    PreviousPage,
    /// Zero-based page index.
    GoToPage(usize),
    SwitchProfile(String),
    Brightness(BrightnessChange),
    /// Sleep now, as if Sleep After had run out.
    Sleep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrightnessChange {
    Up(u8),
    Down(u8),
    Set(u8),
}

impl BrightnessChange {
    pub fn apply(self, current: u8) -> u8 {
        match self {
            BrightnessChange::Up(n) => current.saturating_add(n).min(100),
            BrightnessChange::Down(n) => current.saturating_sub(n),
            BrightnessChange::Set(n) => n.min(100),
        }
    }
}

/// Runs a key's action when it is pressed.
pub fn run(key: &KeyConfig) -> Result<Outcome, String> {
    let s = &key.settings;
    match key.action.as_str() {
        HOTKEY => {
            let hotkey = hotkey_setting(s);
            if hotkey.key.is_empty() {
                return Err("no hotkey recorded".into());
            }
            os::send_hotkey(&hotkey).map(|_| Outcome::Done)
        }
        OPEN => os::open(str_setting(s, "path")).map(|_| Outcome::Done),
        WEBSITE => {
            let url = str_setting(s, "url").trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() < 10 {
                return Err(format!("not a web address: \"{url}\""));
            }
            os::open(url).map(|_| Outcome::Done)
        }
        TEXT => {
            let mut text = str_setting(s, "text").to_owned();
            if bool_setting(s, "enter") {
                text.push('\n');
            }
            os::type_text(&text).map(|_| Outcome::Done)
        }
        MULTIMEDIA => {
            let media = media_setting(s).ok_or("no media key chosen")?;
            os::media_key(media).map(|_| Outcome::Done)
        }
        NEXT_PAGE => Ok(Outcome::NextPage),
        PREVIOUS_PAGE => Ok(Outcome::PreviousPage),
        GO_TO_PAGE => {
            let page = int_setting(s, "page", 1).clamp(1, deckly_core::profile::MAX_PAGES as i64);
            Ok(Outcome::GoToPage(page as usize - 1))
        }
        SWITCH_PROFILE => {
            let id = str_setting(s, "profile");
            if id.is_empty() {
                return Err("no profile chosen".into());
            }
            Ok(Outcome::SwitchProfile(id.to_owned()))
        }
        BRIGHTNESS => {
            let value = int_setting(s, "value", 10).clamp(0, 100) as u8;
            let change = match str_setting(s, "mode") {
                "down" => BrightnessChange::Down(value),
                "set" => BrightnessChange::Set(value),
                _ => BrightnessChange::Up(value),
            };
            Ok(Outcome::Brightness(change))
        }
        SLEEP => Ok(Outcome::Sleep),
        other => Err(format!("unknown action {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_unique() {
        for a in ACTIONS {
            assert_eq!(ACTIONS.iter().filter(|b| b.uuid == a.uuid).count(), 1);
        }
    }

    #[test]
    fn navigation_outcomes() {
        let key = |uuid: &str, settings| KeyConfig::new(uuid, settings);
        assert_eq!(run(&key(NEXT_PAGE, json!({}))), Ok(Outcome::NextPage));
        assert_eq!(
            run(&key(GO_TO_PAGE, json!({"page": 3}))),
            Ok(Outcome::GoToPage(2))
        );
        assert_eq!(
            run(&key(GO_TO_PAGE, json!({"page": -5}))),
            Ok(Outcome::GoToPage(0))
        );
        assert_eq!(
            run(&key(GO_TO_PAGE, json!("garbage"))),
            Ok(Outcome::GoToPage(0))
        );
        assert!(run(&key(SWITCH_PROFILE, json!({}))).is_err());
        assert_eq!(run(&key(SLEEP, json!({}))), Ok(Outcome::Sleep));
        assert!(run(&key("com.example.nope", json!({}))).is_err());
    }

    #[test]
    fn brightness_changes_stay_in_range() {
        assert_eq!(BrightnessChange::Up(30).apply(90), 100);
        assert_eq!(BrightnessChange::Down(30).apply(10), 0);
        assert_eq!(BrightnessChange::Set(250).apply(10), 100);
    }

    #[test]
    fn website_requires_http_url() {
        let key = KeyConfig::new(WEBSITE, json!({"url": "file:///etc/passwd"}));
        assert!(run(&key).is_err());
    }

    #[test]
    fn hotkey_round_trip_and_label() {
        let hk = Hotkey {
            ctrl: true,
            shift: true,
            alt: false,
            meta: false,
            key: "S".into(),
        };
        let mut s = json!(null);
        set_hotkey(&mut s, &hk);
        assert_eq!(hotkey_setting(&s), hk);
        assert_eq!(hotkey_label(&hk), "Ctrl + Shift + S");
    }
}

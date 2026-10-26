//! App settings: per-device preferences, which profile each device shows and
//! the global key title style. Stored as one JSON file; loading never trusts
//! it and brings every value within limits.
//!
//! The set of settings follows the official app's Preferences window
//! (docs/clean-room/2026-10-26-preferences.md).

use crate::profile::TitleStyle;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const MAX_DEVICE_NAME_CHARS: usize = 64;
/// Devices remembered in the settings file; more are dropped on load.
pub const MAX_DEVICES: usize = 256;
pub const DEFAULT_BRIGHTNESS: u8 = 70;
/// Longer screensaver paths are dropped on load.
const MAX_PATH_BYTES: usize = 4096;
/// Sleep After choices in minutes; 0 means never.
pub const SLEEP_CHOICES: [u32; 10] = [0, 1, 2, 5, 10, 15, 30, 60, 120, 240];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Brightness for devices without their own setting. Older Deckly
    /// versions kept a single brightness for all devices here.
    pub brightness: u8,
    /// Device serial number -> profile id shown on that device.
    pub assignments: BTreeMap<String, String>,
    /// Device serial number -> that device's preferences.
    pub devices: BTreeMap<String, DeviceSettings>,
    /// Title style of keys that do not set their own.
    pub title_style: TitleStyle,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            brightness: DEFAULT_BRIGHTNESS,
            assignments: BTreeMap::new(),
            devices: BTreeMap::new(),
            title_style: TitleStyle::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceSettings {
    /// Shown instead of the model name; empty uses the model name.
    pub name: String,
    /// A disabled device is left alone: no images, its keys do nothing.
    pub enabled: bool,
    pub orientation: Orientation,
    /// Minutes without a key press before the device sleeps; 0 is never.
    pub sleep_minutes: u32,
    pub brightness: u8,
    /// Profile shown when the device connects.
    pub default_profile: Option<String>,
    /// Image shown on the keys instead of turning the backlight off when
    /// the device sleeps; a PNG cropped to the device's screen.
    pub screensaver: Option<PathBuf>,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            orientation: Orientation::Standard,
            sleep_minutes: 0,
            brightness: DEFAULT_BRIGHTNESS,
            default_profile: None,
            screensaver: None,
        }
    }
}

/// How key content is turned on the device. The first four turn only the
/// icons and text of each key; `UpsideDown` turns the whole device, so key
/// positions are mirrored too (docs/clean-room/2026-10-26-orientation-screensaver-standby.md).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    #[default]
    Standard,
    /// For a device lying on its side: key content turned a quarter turn
    /// counter-clockwise.
    Rotate90,
    Rotate180,
    /// Key content turned a quarter turn clockwise.
    Rotate270,
    /// The device is used turned by 180°.
    UpsideDown,
}

/// Extra clockwise turn applied to every key image, in quarter turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

impl Orientation {
    /// Choices that turn only icons and text.
    pub const ICONS: [Orientation; 4] = [
        Orientation::Standard,
        Orientation::Rotate90,
        Orientation::Rotate180,
        Orientation::Rotate270,
    ];
    /// Choices that turn the whole device.
    pub const DEVICE: [Orientation; 1] = [Orientation::UpsideDown];

    pub fn label(self) -> &'static str {
        match self {
            Orientation::Standard => "Standard",
            Orientation::Rotate90 => "90°",
            Orientation::Rotate180 => "180°",
            Orientation::Rotate270 => "270°",
            Orientation::UpsideDown => "Upside Down",
        }
    }

    /// The clockwise turn added to each key image.
    pub fn turn(self) -> Turn {
        match self {
            Orientation::Standard => Turn::None,
            Orientation::Rotate90 => Turn::ThreeQuarters,
            Orientation::Rotate180 | Orientation::UpsideDown => Turn::Half,
            Orientation::Rotate270 => Turn::Quarter,
        }
    }

    /// Maps a key index as the user sees it to the physical key, and back:
    /// turning by 180° is its own inverse. Only `UpsideDown` moves keys.
    pub fn map_key(self, key: u8, key_count: u8) -> u8 {
        match self {
            Orientation::UpsideDown => key_count.saturating_sub(1).saturating_sub(key),
            _ => key,
        }
    }
}

pub fn sleep_label(minutes: u32) -> String {
    match minutes {
        0 => "Never".into(),
        1 => "1 minute".into(),
        60 => "1 hour".into(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

impl Settings {
    /// A device's preferences; unknown devices get the defaults.
    pub fn device(&self, serial: &str) -> DeviceSettings {
        self.devices
            .get(serial)
            .cloned()
            .unwrap_or_else(|| self.new_device())
    }

    pub fn device_mut(&mut self, serial: &str) -> &mut DeviceSettings {
        let fresh = self.new_device();
        self.devices.entry(serial.to_owned()).or_insert(fresh)
    }

    fn new_device(&self) -> DeviceSettings {
        DeviceSettings {
            brightness: self.brightness,
            ..DeviceSettings::default()
        }
    }

    /// Forgets every reference to a deleted profile.
    pub fn forget_profile(&mut self, id: &str) {
        self.assignments.retain(|_, p| p != id);
        for d in self.devices.values_mut() {
            if d.default_profile.as_deref() == Some(id) {
                d.default_profile = None;
            }
        }
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let mut settings: Settings = serde_json::from_str(text)?;
        settings.sanitize();
        Ok(settings)
    }

    /// Brings settings from an untrusted file within limits.
    pub fn sanitize(&mut self) {
        self.brightness = self.brightness.min(100);
        while self.devices.len() > MAX_DEVICES {
            self.devices.pop_last();
        }
        while self.assignments.len() > MAX_DEVICES {
            self.assignments.pop_last();
        }
        for d in self.devices.values_mut() {
            d.name = d.name.trim().chars().take(MAX_DEVICE_NAME_CHARS).collect();
            d.brightness = d.brightness.min(100);
            if !SLEEP_CHOICES.contains(&d.sleep_minutes) {
                d.sleep_minutes = 0;
            }
            if d.screensaver
                .as_ref()
                .is_some_and(|p| p.as_os_str().len() > MAX_PATH_BYTES)
            {
                d.screensaver = None;
            }
        }
        self.title_style.sanitize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_use_defaults() {
        let s = Settings::from_json("{}").unwrap();
        assert_eq!(s, Settings::default());
        // Files written before per-device settings existed.
        let s = Settings::from_json(r#"{"brightness":40,"assignments":{"A1":"p"}}"#).unwrap();
        assert_eq!(s.device("A1").brightness, 40);
        assert!(s.device("A1").enabled);
    }

    #[test]
    fn round_trip() {
        let mut s = Settings::default();
        let d = s.device_mut("SER");
        d.name = "Desk".into();
        d.orientation = Orientation::Rotate270;
        d.sleep_minutes = 30;
        d.screensaver = Some(PathBuf::from("screensavers/SER.png"));
        s.title_style.size = 16;
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(Settings::from_json(&text).unwrap(), s);
    }

    #[test]
    fn sanitize_bounds_values() {
        let long = "x".repeat(500);
        let text = format!(
            r#"{{"brightness":250,"devices":{{"S":{{"name":"{long}","brightness":200,"sleep_minutes":7}}}},
            "title_style":{{"size":255}}}}"#
        );
        let s = Settings::from_json(&text).unwrap();
        assert_eq!(s.brightness, 100);
        let d = s.device("S");
        assert_eq!(d.name.chars().count(), MAX_DEVICE_NAME_CHARS);
        assert_eq!(d.brightness, 100);
        assert_eq!(d.sleep_minutes, 0);
        assert_eq!(s.title_style.size, 40);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Settings::from_json("[1,2]").is_err());
        assert!(Settings::from_json(r#"{"devices":{"S":{"orientation":"sideways"}}}"#).is_err());
    }

    #[test]
    fn upside_down_reverses_keys() {
        let o = Orientation::UpsideDown;
        assert_eq!(o.map_key(0, 15), 14);
        assert_eq!(o.map_key(14, 15), 0);
        assert_eq!(o.map_key(7, 15), 7);
        for k in 0..15 {
            assert_eq!(o.map_key(o.map_key(k, 15), 15), k);
            assert_eq!(Orientation::Standard.map_key(k, 15), k);
        }
        // Out-of-range input never underflows.
        assert_eq!(o.map_key(200, 15), 0);
        assert_eq!(o.map_key(0, 0), 0);
    }

    #[test]
    fn icon_turns_leave_keys_in_place() {
        for o in Orientation::ICONS {
            for k in 0..15 {
                assert_eq!(o.map_key(k, 15), k);
            }
        }
    }

    // Turns observed in the official app
    // (docs/clean-room/2026-10-26-orientation-screensaver-standby.md).
    #[test]
    fn orientation_turns() {
        assert_eq!(Orientation::Standard.turn(), Turn::None);
        assert_eq!(Orientation::Rotate90.turn(), Turn::ThreeQuarters);
        assert_eq!(Orientation::Rotate180.turn(), Turn::Half);
        assert_eq!(Orientation::Rotate270.turn(), Turn::Quarter);
        assert_eq!(Orientation::UpsideDown.turn(), Turn::Half);
    }

    #[test]
    fn orientation_names_are_stable() {
        let all: Vec<Orientation> = Orientation::ICONS
            .into_iter()
            .chain(Orientation::DEVICE)
            .collect();
        let json = serde_json::to_string(&all).unwrap();
        assert_eq!(
            json,
            r#"["standard","rotate90","rotate180","rotate270","upside_down"]"#
        );
    }

    #[test]
    fn overlong_screensaver_path_is_dropped() {
        let long = "x".repeat(MAX_PATH_BYTES + 1);
        let text = format!(r#"{{"devices":{{"S":{{"screensaver":"{long}"}}}}}}"#);
        assert_eq!(
            Settings::from_json(&text).unwrap().device("S").screensaver,
            None
        );
    }

    #[test]
    fn forgetting_a_profile_clears_references() {
        let mut s = Settings::default();
        s.assignments.insert("A".into(), "p1".into());
        s.assignments.insert("B".into(), "p2".into());
        s.device_mut("A").default_profile = Some("p1".into());
        s.forget_profile("p1");
        assert!(!s.assignments.contains_key("A"));
        assert!(s.assignments.contains_key("B"));
        assert_eq!(s.device("A").default_profile, None);
    }

    #[test]
    fn sleep_labels() {
        assert_eq!(sleep_label(0), "Never");
        assert_eq!(sleep_label(1), "1 minute");
        assert_eq!(sleep_label(15), "15 minutes");
        assert_eq!(sleep_label(60), "1 hour");
        assert_eq!(sleep_label(240), "4 hours");
    }
}

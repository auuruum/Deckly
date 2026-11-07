//! How the short feedback marks on keys look: the failure mark, the success
//! mark after an action ran, and the ring that counts down a folder's Auto
//! Exit. One global setting for all profiles and keys
//! (docs/clean-room/2026-11-07-feedback-marks.md).
//!
//! Loading never trusts the file: every value is clamped, unknown icons fall
//! back to the mark's own default, and missing fields take that mark's
//! default, so a partial entry keeps the right icon.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Icons a failure or success mark can show, by their Phosphor names. The
/// app maps each name to its glyph.
pub const MARK_ICONS: [&str; 20] = [
    "warning",
    "warning-circle",
    "warning-octagon",
    "x-circle",
    "x-square",
    "x",
    "prohibit",
    "thumbs-down",
    "smiley-sad",
    "bug",
    "check-circle",
    "check",
    "check-square",
    "check-fat",
    "seal-check",
    "thumbs-up",
    "smiley",
    "star",
    "heart",
    "sparkle",
];

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "RawMarks")]
pub struct FeedbackMarks {
    /// Shown when an action failed.
    pub failure: MarkStyle,
    /// Shown after an action ran, for example a finished Multi Action.
    pub success: MarkStyle,
    /// The Auto Exit countdown on a folder's Exit key.
    pub folder_exit: RingStyle,
}

impl Default for FeedbackMarks {
    fn default() -> Self {
        Self {
            failure: MarkStyle::failure(),
            success: MarkStyle::success(),
            folder_exit: RingStyle::default(),
        }
    }
}

/// A failure or success mark: an icon over the dimmed key for a while.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MarkStyle {
    pub visible: bool,
    /// One of [`MARK_ICONS`].
    pub icon: String,
    pub color: [u8; 3],
    /// Icon size in percent of the key.
    pub size: u8,
    /// How long the mark stays, in milliseconds.
    pub duration_ms: u32,
}

impl MarkStyle {
    pub const MIN_SIZE: u8 = 30;
    pub const MAX_SIZE: u8 = 90;
    pub const MIN_DURATION_MS: u32 = 250;
    pub const MAX_DURATION_MS: u32 = 10_000;

    pub fn failure() -> Self {
        Self {
            visible: true,
            icon: "warning".to_owned(),
            color: [0xF0, 0xB4, 0x3C],
            size: 60,
            duration_ms: 1500,
        }
    }

    pub fn success() -> Self {
        Self {
            visible: true,
            icon: "check-circle".to_owned(),
            color: [0x4C, 0xC3, 0x5E],
            size: 60,
            duration_ms: 1500,
        }
    }

    pub fn duration(&self) -> Duration {
        Duration::from_millis(u64::from(self.duration_ms))
    }

    /// Takes the fields `raw` has over `self`, within limits.
    fn merged(mut self, raw: RawMark) -> Self {
        if let Some(visible) = raw.visible {
            self.visible = visible;
        }
        if let Some(icon) = raw.icon.filter(|i| MARK_ICONS.contains(&i.as_str())) {
            self.icon = icon;
        }
        if let Some(color) = raw.color {
            self.color = color;
        }
        if let Some(size) = raw.size {
            self.size = clamp_u8(size, Self::MIN_SIZE, Self::MAX_SIZE);
        }
        if let Some(ms) = raw.duration_ms {
            self.duration_ms = clamp_u32(ms, Self::MIN_DURATION_MS, Self::MAX_DURATION_MS);
        }
        self
    }

    /// Brings values set in code within limits.
    pub fn sanitize(&mut self, default: &MarkStyle) {
        if !MARK_ICONS.contains(&self.icon.as_str()) {
            self.icon.clone_from(&default.icon);
        }
        self.size = self.size.clamp(Self::MIN_SIZE, Self::MAX_SIZE);
        self.duration_ms = self
            .duration_ms
            .clamp(Self::MIN_DURATION_MS, Self::MAX_DURATION_MS);
    }
}

/// The Auto Exit countdown ring. It lasts as long as the folder's delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RingStyle {
    pub visible: bool,
    /// Colour of the part that has filled.
    pub color: [u8; 3],
    /// Diameter in percent of the key.
    pub size: u8,
}

impl RingStyle {
    pub const MIN_SIZE: u8 = 50;
    pub const MAX_SIZE: u8 = 100;

    fn merged(mut self, raw: RawRing) -> Self {
        if let Some(visible) = raw.visible {
            self.visible = visible;
        }
        if let Some(color) = raw.color {
            self.color = color;
        }
        if let Some(size) = raw.size {
            self.size = clamp_u8(size, Self::MIN_SIZE, Self::MAX_SIZE);
        }
        self
    }

    pub fn sanitize(&mut self) {
        self.size = self.size.clamp(Self::MIN_SIZE, Self::MAX_SIZE);
    }
}

impl Default for RingStyle {
    fn default() -> Self {
        Self {
            visible: true,
            color: [0xE6, 0xE7, 0xEA],
            size: 80,
        }
    }
}

impl FeedbackMarks {
    /// Brings values set in code within limits.
    pub fn sanitize(&mut self) {
        self.failure.sanitize(&MarkStyle::failure());
        self.success.sanitize(&MarkStyle::success());
        self.folder_exit.sanitize();
    }
}

fn clamp_u8(value: u64, min: u8, max: u8) -> u8 {
    u8::try_from(value.clamp(u64::from(min), u64::from(max))).unwrap_or(max)
}

fn clamp_u32(value: u64, min: u32, max: u32) -> u32 {
    u32::try_from(value.clamp(u64::from(min), u64::from(max))).unwrap_or(max)
}

/// What a settings file holds. Every field may be missing, numbers may be
/// any size; [`FeedbackMarks`] fills in and clamps.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMarks {
    failure: RawMark,
    success: RawMark,
    folder_exit: RawRing,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMark {
    visible: Option<bool>,
    icon: Option<String>,
    color: Option<[u8; 3]>,
    size: Option<u64>,
    duration_ms: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawRing {
    visible: Option<bool>,
    color: Option<[u8; 3]>,
    size: Option<u64>,
}

impl From<RawMarks> for FeedbackMarks {
    fn from(raw: RawMarks) -> Self {
        Self {
            failure: MarkStyle::failure().merged(raw.failure),
            success: MarkStyle::success().merged(raw.success),
            folder_exit: RingStyle::default().merged(raw.folder_exit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> FeedbackMarks {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn round_trip() {
        let marks = FeedbackMarks {
            failure: MarkStyle {
                visible: false,
                icon: "x-circle".into(),
                color: [255, 0, 0],
                size: 45,
                duration_ms: 4000,
            },
            success: MarkStyle {
                icon: "thumbs-up".into(),
                duration_ms: 250,
                ..MarkStyle::success()
            },
            folder_exit: RingStyle {
                visible: false,
                color: [1, 2, 3],
                size: 100,
            },
        };
        let text = serde_json::to_string(&marks).unwrap();
        assert_eq!(parse(&text), marks);
    }

    #[test]
    fn missing_fields_take_each_marks_own_default() {
        assert_eq!(parse("{}"), FeedbackMarks::default());
        let marks = parse(r#"{"success":{"color":[1,2,3]},"failure":{"visible":false}}"#);
        assert_eq!(marks.success.icon, "check-circle");
        assert_eq!(marks.success.color, [1, 2, 3]);
        assert_eq!(marks.success.duration_ms, 1500);
        assert_eq!(marks.failure.icon, "warning");
        assert!(!marks.failure.visible);
        assert_eq!(marks.folder_exit, RingStyle::default());
    }

    #[test]
    fn bad_values_are_clamped_or_replaced() {
        let marks = parse(
            r#"{"failure":{"icon":"../../evil","size":0,"duration_ms":0},
                "success":{"icon":"","size":100000000000,"duration_ms":99999999999},
                "folder_exit":{"size":7}}"#,
        );
        assert_eq!(marks.failure.icon, "warning");
        assert_eq!(marks.failure.size, MarkStyle::MIN_SIZE);
        assert_eq!(marks.failure.duration_ms, MarkStyle::MIN_DURATION_MS);
        assert_eq!(marks.success.icon, "check-circle");
        assert_eq!(marks.success.size, MarkStyle::MAX_SIZE);
        assert_eq!(marks.success.duration_ms, MarkStyle::MAX_DURATION_MS);
        assert_eq!(marks.folder_exit.size, RingStyle::MIN_SIZE);
        // Wrong types are rejected like the rest of the settings file.
        assert!(serde_json::from_str::<FeedbackMarks>(r#"{"failure":{"size":-1}}"#).is_err());
        assert!(serde_json::from_str::<FeedbackMarks>(r#"{"success":"big"}"#).is_err());
    }

    #[test]
    fn sanitize_fixes_values_set_in_code() {
        let mut marks = FeedbackMarks::default();
        marks.failure.icon = "nope".into();
        marks.failure.size = 255;
        marks.success.duration_ms = u32::MAX;
        marks.folder_exit.size = 0;
        marks.sanitize();
        assert_eq!(marks.failure.icon, "warning");
        assert_eq!(marks.failure.size, MarkStyle::MAX_SIZE);
        assert_eq!(marks.success.duration_ms, MarkStyle::MAX_DURATION_MS);
        assert_eq!(marks.folder_exit.size, RingStyle::MIN_SIZE);
    }

    #[test]
    fn durations() {
        let mut style = MarkStyle::failure();
        assert_eq!(style.duration(), Duration::from_millis(1500));
        style.duration_ms = 250;
        assert_eq!(style.duration(), Duration::from_millis(250));
    }

    #[test]
    fn defaults_use_listed_icons() {
        let marks = FeedbackMarks::default();
        assert!(MARK_ICONS.contains(&marks.failure.icon.as_str()));
        assert!(MARK_ICONS.contains(&marks.success.icon.as_str()));
    }
}

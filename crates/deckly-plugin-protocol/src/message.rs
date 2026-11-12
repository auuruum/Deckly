//! The messages. JSON objects tagged by `type`, in snake case. Unknown
//! message types read as `Unknown` and unknown fields are ignored, so a new
//! feature is a new message type or field, never a changed one.

use crate::limits::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// The protocol number this crate speaks. The host opens with it in
/// `hello`; the plugin answers with its own in `ready`.
pub const PROTOCOL: u32 = 1;
/// The oldest protocol number the host still accepts in `ready`.
pub const MIN_PROTOCOL: u32 = 1;

/// Checks a received message against the limits.
pub trait Validate {
    fn validate(&self) -> Result<(), String>;
}

/// Host to plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// The first message, before anything else.
    Hello(Hello),
    DeviceConnected {
        device: Device,
    },
    DeviceDisconnected {
        device: String,
    },
    /// A key with one of the plugin's actions became visible.
    KeyAppear {
        key: KeyInfo,
    },
    KeyDisappear {
        key: KeyInfo,
    },
    KeyDown {
        key: KeyInfo,
    },
    KeyUp {
        key: KeyInfo,
    },
    /// The key's settings changed outside the plugin.
    Settings {
        key: KeyInfo,
    },
    GlobalSettings {
        settings: Value,
    },
    /// The computer woke from sleep.
    SystemWake,
    /// Asks for the options of a choice field marked `dynamic`, when its
    /// form opens. The plugin answers with `choices` and the same request
    /// number; Deckly gives up waiting after a few seconds.
    GetChoices {
        request: u64,
        /// The field's key.
        field: String,
        /// The key whose form asks; none for the plugin-wide form.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<KeyInfo>,
    },
    /// The user pressed a button field.
    Button {
        /// The button's key.
        field: String,
        /// The key whose form holds the button; none for the plugin-wide
        /// form. The key need not be on a device.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<KeyInfo>,
    },
    /// A message type this side does not know; ignored.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// Deckly's version.
    pub app_version: String,
    /// `windows`, `mac` or `linux`.
    pub platform: String,
    /// On Linux `x11` or `wayland`; empty elsewhere or when unknown.
    #[serde(default)]
    pub session: String,
    /// The user's language, as a tag such as `en` or `de-DE`.
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Host services this Deckly offers on this platform. Empty until host
    /// services exist.
    #[serde(default)]
    pub services: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// A stable id that does not reveal the serial number.
    pub id: String,
    pub name: String,
    pub columns: u8,
    pub rows: u8,
}

/// One placed key as the plugin sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyInfo {
    /// The host's id for this key; stays the same while the key exists.
    pub context: String,
    pub action: String,
    /// Empty when no device shows the key (the editor only).
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub column: u8,
    #[serde(default)]
    pub row: u8,
    /// Only for actions with more than one state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<u8>,
    /// A JSON object.
    #[serde(default = "empty_object")]
    pub settings: Value,
    /// Inside a Multi Action, where the key has no place of its own.
    #[serde(default)]
    pub in_multi_action: bool,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Plugin to host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginMessage {
    /// The answer to `hello`; nothing else is accepted before it.
    Ready {
        protocol: u32,
        /// The SDK version the plugin was built with.
        #[serde(default)]
        sdk: String,
    },
    /// `None` clears the title. Without a state it applies to every state.
    SetTitle {
        context: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        state: Option<u8>,
    },
    SetState {
        context: String,
        state: u8,
    },
    /// A face drawn by the host; `None` goes back to the plain key.
    SetFace {
        context: String,
        #[serde(default)]
        face: Option<Face>,
    },
    /// With a format, the blob frame sent right before this message holds
    /// the image; without one the plugin's image is cleared.
    SetImage {
        context: String,
        #[serde(default)]
        format: Option<ImageFormat>,
        #[serde(default)]
        state: Option<u8>,
    },
    /// Settings to store with the key, as a JSON object.
    SetSettings {
        context: String,
        settings: Value,
    },
    SetGlobalSettings {
        settings: Value,
    },
    /// Asks for a `global_settings` message.
    GetGlobalSettings,
    /// The short success mark.
    ShowOk {
        context: String,
    },
    /// The short failure mark.
    ShowAlert {
        context: String,
    },
    /// The answer to `get_choices`: the options, or why there are none.
    Choices {
        request: u64,
        #[serde(default)]
        options: Vec<crate::Choice>,
        /// A short reason shown to the user.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// What a blob holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Svg,
    /// Raw pixels, four bytes each, row by row.
    Rgba {
        width: u32,
        height: u32,
    },
}

/// A small description of what a key shows, drawn by the host with the
/// user's fonts and key style. Every part is optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Face {
    /// A small line at the top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The large text in the middle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// A small line under the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A bar along the bottom, from 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    /// Text colour; the user's title colour when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    /// Fills the key; the key's image shows when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Color>,
    /// The progress bar's colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Color>,
    /// An image file inside the plugin folder (PNG, JPEG or SVG), drawn
    /// above the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// Builders. Text is cut to [`MAX_FACE_TEXT_CHARS`] and progress clamped
/// to 0..=1, so a face built here always passes the host's checks.
impl Face {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn label(mut self, text: impl AsRef<str>) -> Self {
        self.label = Some(face_text(text.as_ref()));
        self
    }

    pub fn value(mut self, text: impl AsRef<str>) -> Self {
        self.value = Some(face_text(text.as_ref()));
        self
    }

    pub fn detail(mut self, text: impl AsRef<str>) -> Self {
        self.detail = Some(face_text(text.as_ref()));
        self
    }

    /// From 0 (empty) to 1 (full); a value that is not a number shows no bar.
    pub fn progress(mut self, fraction: f32) -> Self {
        self.progress = (!fraction.is_nan()).then(|| fraction.clamp(0.0, 1.0));
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    pub fn accent(mut self, color: Color) -> Self {
        self.accent = Some(color);
        self
    }

    /// An image file inside the plugin folder.
    pub fn icon(mut self, path: impl Into<String>) -> Self {
        self.icon = Some(path.into());
        self
    }
}

fn face_text(text: &str) -> String {
    truncate(text, MAX_FACE_TEXT_CHARS).to_owned()
}

impl std::hash::Hash for Face {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.label.hash(h);
        self.value.hash(h);
        self.detail.hash(h);
        self.progress.map(f32::to_bits).hash(h);
        self.color.hash(h);
        self.background.hash(h);
        self.accent.hash(h);
        self.icon.hash(h);
    }
}

/// An RGB colour, written `#rrggbb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Color(pub [u8; 3]);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b])
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b] = self.0;
        write!(f, "#{r:02x}{g:02x}{b:02x}")
    }
}

impl From<Color> for String {
    fn from(c: Color) -> Self {
        c.to_string()
    }
}

impl TryFrom<String> for Color {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        let hex = s
            .strip_prefix('#')
            .filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| format!("bad colour {:?}; expected #rrggbb", truncate(&s, 16)))?;
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
        Ok(Self([byte(0), byte(2), byte(4)]))
    }
}

/// The first `max` characters of `s`.
pub fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((at, _)) => &s[..at],
        None => s,
    }
}

/// Plugin and action ids: reverse-DNS names of letters, digits, `.`, `-`
/// and `_`, not starting with a dot and without `..`.
pub fn valid_id(id: &str) -> bool {
    (1..=MAX_ID_BYTES).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !id.starts_with('.')
        && !id.contains("..")
}

pub(crate) fn check_bytes(what: &str, s: &str, max: usize) -> Result<(), String> {
    if s.len() > max {
        return Err(format!("{what} is longer than {max} bytes"));
    }
    Ok(())
}

pub(crate) fn check_chars(what: &str, s: &str, max: usize) -> Result<(), String> {
    // Bytes bound chars from above, so most strings skip the count.
    if s.len() > max && s.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok(())
}

pub(crate) fn check_count(what: &str, n: usize, max: usize) -> Result<(), String> {
    if n > max {
        return Err(format!("more than {max} {what}"));
    }
    Ok(())
}

/// Settings must be a JSON object of bounded size.
pub fn check_settings(settings: &Value) -> Result<(), String> {
    if !settings.is_object() {
        return Err("settings must be a JSON object".into());
    }
    if json_len(settings) > MAX_SETTINGS_BYTES {
        return Err(format!(
            "settings are larger than {MAX_SETTINGS_BYTES} bytes"
        ));
    }
    Ok(())
}

/// The length of a value written as compact JSON.
pub fn json_len(v: &Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Count(0);
    match serde_json::to_writer(&mut c, v) {
        Ok(()) => c.0,
        Err(_) => usize::MAX,
    }
}

fn check_context(context: &str) -> Result<(), String> {
    if context.is_empty() {
        return Err("empty context".into());
    }
    check_bytes("a context", context, MAX_CONTEXT_BYTES)
}

fn check_state(state: Option<u8>) -> Result<(), String> {
    match state {
        Some(s) if usize::from(s) >= MAX_STATES => Err(format!("state {s} is out of range")),
        _ => Ok(()),
    }
}

impl Validate for Device {
    fn validate(&self) -> Result<(), String> {
        check_bytes("a device id", &self.id, MAX_SHORT_TEXT_BYTES)?;
        check_chars("a device name", &self.name, MAX_NAME_CHARS)
    }
}

impl Validate for KeyInfo {
    fn validate(&self) -> Result<(), String> {
        check_context(&self.context)?;
        if !valid_id(&self.action) {
            return Err("bad action id".into());
        }
        check_bytes("a device id", &self.device, MAX_SHORT_TEXT_BYTES)?;
        check_state(self.state)?;
        check_settings(&self.settings)
    }
}

impl Validate for Hello {
    fn validate(&self) -> Result<(), String> {
        check_bytes("the app version", &self.app_version, MAX_SHORT_TEXT_BYTES)?;
        check_bytes("the platform", &self.platform, MAX_SHORT_TEXT_BYTES)?;
        check_bytes("the session", &self.session, MAX_SHORT_TEXT_BYTES)?;
        check_bytes("the language", &self.language, MAX_SHORT_TEXT_BYTES)?;
        check_count("devices", self.devices.len(), MAX_DEVICES)?;
        self.devices.iter().try_for_each(Validate::validate)?;
        check_count("services", self.services.len(), MAX_SERVICES)?;
        self.services
            .iter()
            .try_for_each(|s| check_bytes("a service name", s, MAX_SERVICE_BYTES))
    }
}

impl Validate for HostMessage {
    fn validate(&self) -> Result<(), String> {
        match self {
            HostMessage::Hello(h) => h.validate(),
            HostMessage::DeviceConnected { device } => device.validate(),
            HostMessage::DeviceDisconnected { device } => {
                check_bytes("a device id", device, MAX_SHORT_TEXT_BYTES)
            }
            HostMessage::KeyAppear { key }
            | HostMessage::KeyDisappear { key }
            | HostMessage::KeyDown { key }
            | HostMessage::KeyUp { key }
            | HostMessage::Settings { key } => key.validate(),
            HostMessage::GlobalSettings { settings } => check_settings(settings),
            HostMessage::GetChoices { field, key, .. } | HostMessage::Button { field, key } => {
                if !crate::form::valid_field_key(field) {
                    return Err("bad field key".into());
                }
                key.as_ref().map_or(Ok(()), Validate::validate)
            }
            HostMessage::SystemWake | HostMessage::Unknown => Ok(()),
        }
    }
}

impl Validate for Face {
    fn validate(&self) -> Result<(), String> {
        for (what, text) in [
            ("the face label", &self.label),
            ("the face value", &self.value),
            ("the face detail", &self.detail),
        ] {
            if let Some(t) = text {
                check_chars(what, t, MAX_FACE_TEXT_CHARS)?;
            }
        }
        if let Some(p) = self.progress
            && !(0.0..=1.0).contains(&p)
        {
            return Err("progress must be between 0 and 1".into());
        }
        if let Some(icon) = &self.icon {
            check_bytes("the face icon path", icon, MAX_PATH_BYTES)?;
        }
        Ok(())
    }
}

impl Validate for ImageFormat {
    fn validate(&self) -> Result<(), String> {
        if let ImageFormat::Rgba { width, height } = *self
            && (width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE)
        {
            return Err(format!("a raw image of {width}x{height} pixels"));
        }
        Ok(())
    }
}

impl ImageFormat {
    /// The blob length a raw image needs; `None` for encoded formats.
    pub fn raw_len(&self) -> Option<usize> {
        match *self {
            ImageFormat::Rgba { width, height } => Some(width as usize * height as usize * 4),
            _ => None,
        }
    }
}

impl Validate for PluginMessage {
    fn validate(&self) -> Result<(), String> {
        match self {
            PluginMessage::Ready { sdk, .. } => {
                check_bytes("the SDK version", sdk, MAX_SHORT_TEXT_BYTES)
            }
            PluginMessage::SetTitle {
                context,
                title,
                state,
            } => {
                check_context(context)?;
                check_state(*state)?;
                match title {
                    Some(t) => check_chars("a title", t, MAX_TITLE_CHARS),
                    None => Ok(()),
                }
            }
            PluginMessage::SetState { context, state } => {
                check_context(context)?;
                check_state(Some(*state))
            }
            PluginMessage::SetFace { context, face } => {
                check_context(context)?;
                face.as_ref().map_or(Ok(()), Validate::validate)
            }
            PluginMessage::SetImage {
                context,
                format,
                state,
            } => {
                check_context(context)?;
                check_state(*state)?;
                format.as_ref().map_or(Ok(()), Validate::validate)
            }
            PluginMessage::SetSettings { context, settings } => {
                check_context(context)?;
                check_settings(settings)
            }
            PluginMessage::SetGlobalSettings { settings } => check_settings(settings),
            PluginMessage::ShowOk { context } | PluginMessage::ShowAlert { context } => {
                check_context(context)
            }
            PluginMessage::Choices { options, error, .. } => {
                crate::form::check_choices(options, error.as_deref())
            }
            PluginMessage::GetGlobalSettings | PluginMessage::Unknown => Ok(()),
        }
    }
}

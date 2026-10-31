//! Turns profiles of the official Stream Deck app into Deckly profiles.
//!
//! The format is described in docs/clean-room/2026-10-31-official-profiles.md.
//! Everything read here comes from a file and is untrusted: fields of the
//! wrong type read as missing, and [`Profile::sanitize`] has the last word.
//! Plugin actions keep their UUID and settings, so they start working once
//! their plugin is installed. Official built-in actions without a Deckly
//! counterpart stay on their key the same way and are listed in the report.

use crate::actions;
use crate::os::{Hotkey, MediaKey};
use deckly_core::profile::{self, MAX_FOLDERS, MAX_GROUP_ACTIONS, MAX_PAGES, group_count};
use deckly_core::{
    ActionDelays, DeviceKind, DeviceModel, ELGATO_VENDOR_ID, KeyConfig, Page, Profile, TitleAlign,
    TitleStyle,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// The official app's own actions start with this.
const BUILT_IN: &str = "com.elgato.streamdeck.";
/// The key code Qt uses for "no key"; it fills unused hotkey slots.
const QT_NO_KEY: i64 = 0x01FF_FFFF;
/// Letters of the 5-bit alphabet of folder names; the name ends in `Z`.
const FOLDER_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTVW";

/// One official profile as found on disk or in an archive.
pub struct RawProfile {
    /// Lowercase UUID, from the `<uuid>.sdProfile` folder name.
    pub id: String,
    pub manifest: Value,
    /// Page manifests by lowercase page UUID.
    pub pages: BTreeMap<String, Value>,
}

pub struct Converted {
    pub profile: Profile,
    pub report: Report,
}

/// What the conversion could not carry over exactly.
#[derive(Default)]
pub struct Report {
    pub keys: usize,
    /// Official built-in action -> keys. Deckly has no such action yet.
    pub unsupported: BTreeMap<String, usize>,
    /// Plugin UUID -> its name and the keys using it.
    pub plugins: BTreeMap<String, (String, usize)>,
    /// Actions on dials and touch strips, which Deckly does not show.
    pub dials: usize,
    /// Keys whose states had different pictures or titles; one was kept.
    pub state_variants: usize,
    /// Hotkey keys with no Deckly name, or more than one hotkey in a row.
    pub hotkeys_changed: usize,
    /// Pictures a key names but the source does not hold.
    pub missing_images: usize,
}

/// The page or profile UUID a folder is named after: the plain UUID, or the
/// shorter form used inside `.streamDeckProfile` files.
pub fn folder_id(name: &str) -> Option<String> {
    if let Ok(id) = uuid::Uuid::parse_str(name) {
        return Some(id.hyphenated().to_string());
    }
    let text = name.strip_suffix('Z').filter(|t| t.len() == 26)?;
    let mut bits = 0u128;
    for (i, c) in text.bytes().enumerate() {
        let digit = FOLDER_ALPHABET.iter().position(|&a| a == c)? as u128;
        // 26 digits hold 130 bits; a UUID uses the first 128.
        bits = if i < 25 {
            bits << 5 | digit
        } else {
            bits << 3 | digit >> 2
        };
    }
    Some(uuid::Uuid::from_u128(bits).hyphenated().to_string())
}

/// Profile ids in the order the official app lists them. `value` is its
/// `Devices` setting: Qt's `@Variant(` form as UTF-16, one unit per byte of a
/// data stream with a map for each device. After a device's sorting key come
/// the value's type, then the ids as one text.
pub fn profile_order(value: &[u8]) -> Vec<String> {
    const VARIANT_TYPE: usize = 4;
    let stream: Vec<u8> = value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[low, _]| *low)
        .collect();
    let key: Vec<u8> = "ESDProfilesSorting"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    let mut rest = stream.as_slice();
    while let Some(at) = rest.windows(key.len()).position(|w| w == key) {
        rest = rest.get(at + key.len()..).unwrap_or_default();
        if let Some(ids) = rest.get(VARIANT_TYPE..).and_then(stream_text) {
            order.extend(
                ids.split(',')
                    .filter_map(folder_id)
                    .filter(|id| seen.insert(id.clone())),
            );
        }
    }
    order
}

/// A text in a Qt data stream: its length in bytes, then UTF-16 big-endian.
fn stream_text(stream: &[u8]) -> Option<String> {
    let (length, rest) = stream.split_first_chunk::<4>()?;
    let bytes = rest.get(..u32::from_be_bytes(*length) as usize)?;
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_be_bytes(*unit))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// The model a product code in a manifest stands for.
pub fn model_kind(code: &str) -> Option<DeviceKind> {
    Some(match code {
        "20GAA9901" | "20GAA9902" => DeviceKind::OriginalV2,
        "20GBA9901" => DeviceKind::Mk2,
        "20GAT9901" => DeviceKind::Xl,
        "20GAT9902" => DeviceKind::XlV2,
        "20GAI9901" => DeviceKind::Mini,
        "20GAI9902" => DeviceKind::MiniV2,
        "20GBJ9901" => DeviceKind::Neo,
        "20GBD9901" => DeviceKind::Plus,
        "20GBX9901" => DeviceKind::PlusXl,
        _ => return None,
    })
}

/// A real device's id reads `@(1)[vendor/product/serial]`, which names the
/// exact model; files for any device of a kind have a UUID or nothing.
fn device_from_id(id: &str) -> Option<DeviceKind> {
    let inner = id.split_once('[')?.1.strip_suffix(']')?;
    let mut parts = inner.split('/');
    let vendor: u16 = parts.next()?.parse().ok()?;
    let product: u16 = parts.next()?.parse().ok()?;
    (vendor == ELGATO_VENDOR_ID)
        .then(|| DeviceModel::from_product_id(product))
        .flatten()
        .map(|m| m.kind)
}

fn text<'a>(v: &'a Value, field: &str) -> &'a str {
    v.get(field).and_then(Value::as_str).unwrap_or("")
}

fn list<'a>(v: &'a Value, field: &str) -> &'a [Value] {
    v.get(field)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn number(v: &Value, field: &str) -> Option<i64> {
    let value = v.get(field)?;
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Converts a profile. `image` is asked for each picture, with the page and
/// the path written on the key, and returns where Deckly will keep it.
pub fn convert(
    raw: &RawProfile,
    image: &mut dyn FnMut(&str, &str) -> Option<PathBuf>,
) -> Result<Converted, String> {
    let device = raw.manifest.get("Device").unwrap_or(&Value::Null);
    let code = text(device, "Model");
    let kind = device_from_id(text(device, "UUID"))
        .or_else(|| model_kind(code))
        .ok_or_else(|| format!("device model \"{code}\" is not supported"))?;
    let page_list = raw.manifest.get("Pages").unwrap_or(&Value::Null);
    let mut listed: Vec<String> = list(page_list, "Pages")
        .iter()
        .filter_map(Value::as_str)
        .filter_map(folder_id)
        .filter(|id| raw.pages.contains_key(id))
        .take(MAX_PAGES)
        .collect();
    if listed.is_empty() {
        listed.extend(
            ["Current", "Default"]
                .iter()
                .filter_map(|f| folder_id(text(page_list, f)))
                .find(|id| raw.pages.contains_key(id)),
        );
    }

    let mut run = Run {
        model: DeviceModel::of(kind),
        image,
        report: Report::default(),
    };
    let mut pages = Vec::new();
    let mut waiting = Vec::new();
    for id in &listed {
        if let Some(page) = raw.pages.get(id) {
            let page = run.page(id, page);
            waiting.extend(folder_targets(&page));
            pages.push(page);
        }
    }
    // Folders are pages no list names; only those a key leads to are kept.
    let mut folders = Vec::new();
    let mut seen: HashSet<String> = listed.iter().cloned().collect();
    while let Some(id) = waiting.pop() {
        if folders.len() >= MAX_FOLDERS {
            break;
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(page) = raw.pages.get(&id) {
            let page = run.page(&id, page);
            waiting.extend(folder_targets(&page));
            folders.push(page);
        }
    }

    let name = text(&raw.manifest, "Name").trim();
    let mut profile = Profile {
        version: profile::FORMAT_VERSION,
        id: raw.id.clone(),
        name: if name.is_empty() {
            "Imported Profile".to_owned()
        } else {
            name.chars().take(profile::MAX_TITLE_CHARS).collect()
        },
        device: kind,
        pages,
        folders,
    };
    profile.sanitize();
    Ok(Converted {
        profile,
        report: run.report,
    })
}

fn folder_targets(page: &Page) -> Vec<String> {
    page.keys
        .values()
        .filter_map(KeyConfig::folder)
        .map(str::to_owned)
        .collect()
}

/// The conversion of one profile.
struct Run<'a> {
    model: &'static DeviceModel,
    image: &'a mut dyn FnMut(&str, &str) -> Option<PathBuf>,
    report: Report,
}

impl Run<'_> {
    fn page(&mut self, id: &str, source: &Value) -> Page {
        let picture = text(source, "Icon");
        let mut page = Page {
            id: id.to_owned(),
            name: text(source, "Name").to_owned(),
            icon: (!picture.is_empty())
                .then(|| (self.image)(id, picture))
                .flatten(),
            keys: BTreeMap::new(),
        };
        for controller in list(source, "Controllers") {
            let Some(actions) = controller.get("Actions").and_then(Value::as_object) else {
                continue;
            };
            if !matches!(text(controller, "Type"), "" | "Keypad") {
                self.report.dials += actions.len();
                continue;
            }
            for (position, action) in actions {
                if let Some(index) = self.key_index(position) {
                    self.report.keys += 1;
                    page.keys.insert(index, self.key(action, id, false));
                }
            }
        }
        page
    }

    /// Positions read `column,row`.
    fn key_index(&self, position: &str) -> Option<u8> {
        let (column, row) = position.split_once(',')?;
        let (column, row): (u8, u8) = (column.parse().ok()?, row.parse().ok()?);
        (column < self.model.columns && row < self.model.rows)
            .then(|| row * self.model.columns + column)
    }

    fn key(&mut self, source: &Value, page: &str, in_group: bool) -> KeyConfig {
        let empty = json!({});
        let settings = source.get("Settings").filter(|s| s.is_object());
        let settings = settings.unwrap_or(&empty);
        let (action, settings) = self.action(text(source, "UUID"), settings);
        let mut key = KeyConfig::new(action, settings);
        if !in_group {
            self.fill_groups(&mut key, source, page);
        }
        self.appearance(&mut key, source, page);
        self.note_plugin(&key, source);
        if key.action == actions::MULTI_ACTION_SWITCH {
            let side = number(source, "State").unwrap_or(0);
            actions::set_switch_side(&mut key, usize::from(side == 1));
        }
        if let Some(delays) = source.get("MultiActionDelays") {
            let time = |field| number(delays, field).and_then(|n| u32::try_from(n).ok());
            if let (Some(press), Some(next)) = (time("Delay1"), time("Delay2")) {
                let delays = ActionDelays { press, next };
                key.delays = (delays != ActionDelays::default()).then_some(delays);
            }
        }
        if list(source, "States").len() > 1 {
            key.desired_state = number(source, "OverrideState").and_then(|n| u8::try_from(n).ok());
        }
        key
    }

    /// Multi Actions hold lists of actions under `Actions`; Key Logic holds
    /// one action per press there.
    fn fill_groups(&mut self, key: &mut KeyConfig, source: &Value, page: &str) {
        let lists = group_count(&key.action);
        if lists == 0 {
            return;
        }
        for (i, item) in list(source, "Actions").iter().enumerate() {
            let slot = i.min(lists - 1);
            let members: &[Value] = if item.get("UUID").is_some() {
                std::slice::from_ref(item)
            } else {
                list(item, "Actions")
            };
            for member in members.iter().take(MAX_GROUP_ACTIONS) {
                let member = self.key(member, page, true);
                if let Some(group) = key.groups.get_mut(slot) {
                    group.push(member);
                }
            }
        }
    }

    /// Deckly has one title and picture per key, so the current state's are
    /// used, or those of the first state that has any.
    fn appearance(&mut self, key: &mut KeyConfig, source: &Value, page: &str) {
        let states = list(source, "States");
        let current = number(source, "State")
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| states.get(n));
        let pick = |field: &str| {
            current
                .map(|s| text(s, field))
                .filter(|v| !v.is_empty())
                .or_else(|| {
                    states
                        .iter()
                        .map(|s| text(s, field))
                        .find(|v| !v.is_empty())
                })
                .unwrap_or("")
        };
        key.title = pick("Title").to_owned();
        let picture = pick("Image");
        if !picture.is_empty() {
            key.image = (self.image)(page, picture);
            if key.image.is_none() {
                self.report.missing_images += 1;
            }
        }
        key.title_style = current
            .filter(|s| s.get("FontSize").is_some())
            .or_else(|| states.iter().find(|s| s.get("FontSize").is_some()))
            .map(title_style);
        let variants = |field: &str| {
            let values: HashSet<&str> = states
                .iter()
                .map(|s| text(s, field))
                .filter(|v| !v.is_empty())
                .collect();
            values.len() > 1
        };
        if variants("Image") || variants("Title") {
            self.report.state_variants += 1;
        }
    }

    fn note_plugin(&mut self, key: &KeyConfig, source: &Value) {
        if key.action.starts_with("com.deckly.") || key.action.starts_with(BUILT_IN) {
            return;
        }
        let plugin = source.get("Plugin").unwrap_or(&Value::Null);
        let id = match text(plugin, "UUID") {
            "" => key.action.clone(),
            id => id.to_owned(),
        };
        let entry = self.report.plugins.entry(id).or_default();
        if entry.0.is_empty() {
            text(plugin, "Name").clone_into(&mut entry.0);
        }
        entry.1 += 1;
    }

    /// The Deckly action and settings for an official action.
    fn action(&mut self, uuid: &str, s: &Value) -> (String, Value) {
        let (action, settings) = match uuid {
            "com.elgato.streamdeck.multiactions.routine" => (actions::MULTI_ACTION, json!({})),
            "com.elgato.streamdeck.multiactions.routine2" => {
                (actions::MULTI_ACTION_SWITCH, json!({}))
            }
            "com.elgato.streamdeck.multiactions.random" => (actions::RANDOM_ACTION, json!({})),
            "com.elgato.streamdeck.keys.logic" => (actions::KEY_LOGIC, json!({})),
            "com.elgato.streamdeck.multiactions.delay" => (
                actions::DELAY,
                json!({ "delay": number(s, "delay").unwrap_or(1000) }),
            ),
            "com.elgato.streamdeck.profile.openchild" => {
                let id = folder_id(text(s, "ProfileUUID")).unwrap_or_default();
                (actions::CREATE_FOLDER, json!({ "folder": id }))
            }
            "com.elgato.streamdeck.profile.backtoparent" => (actions::PARENT_FOLDER, json!({})),
            "com.elgato.streamdeck.profile.rotate" => {
                let id = folder_id(text(s, "ProfileUUID")).unwrap_or_default();
                (actions::SWITCH_PROFILE, json!({ "profile": id }))
            }
            "com.elgato.streamdeck.page.previous" => (actions::PREVIOUS_PAGE, json!({})),
            "com.elgato.streamdeck.page.next" => (actions::NEXT_PAGE, json!({})),
            "com.elgato.streamdeck.page.goto" => (
                actions::GO_TO_PAGE,
                json!({ "page": number(s, "PageIndex").unwrap_or(1) }),
            ),
            "com.elgato.streamdeck.page.indicator" | "com.elgato.streamdeck.system.pagination" => {
                (actions::PAGE_INDICATOR, json!({}))
            }
            "com.elgato.streamdeck.system.hotkey" => (actions::HOTKEY, self.hotkey(s)),
            "com.elgato.streamdeck.system.open" => {
                (actions::OPEN, json!({ "path": text(s, "path") }))
            }
            "com.elgato.streamdeck.system.openapp" => (actions::OPEN, open_app(s)),
            "com.elgato.streamdeck.system.website" => {
                (actions::WEBSITE, json!({ "url": web_address(s) }))
            }
            "com.elgato.streamdeck.system.text" => (
                actions::TEXT,
                json!({
                    "text": text(s, "pastedText"),
                    "enter": actions::bool_setting(s, "isSendingEnter"),
                }),
            ),
            "com.elgato.streamdeck.system.sleep" => (actions::SLEEP, json!({})),
            "com.elgato.streamdeck.system.multimedia" => match media_key(s) {
                Some(media) => (actions::MULTIMEDIA, json!({ "media": media.id() })),
                None => return self.unsupported(uuid, s),
            },
            other if other.starts_with(BUILT_IN) => return self.unsupported(uuid, s),
            other => return (other.to_owned(), s.clone()),
        };
        (action.to_owned(), settings)
    }

    fn unsupported(&mut self, uuid: &str, settings: &Value) -> (String, Value) {
        *self.report.unsupported.entry(uuid.to_owned()).or_default() += 1;
        (uuid.to_owned(), settings.clone())
    }

    /// A key holds up to four hotkeys in a row, unused ones blank. Deckly
    /// presses one combination per action.
    fn hotkey(&mut self, s: &Value) -> Value {
        let keys: Vec<&Value> = list(s, "Hotkeys")
            .iter()
            .filter(|h| number(h, "QTKeyCode").unwrap_or(QT_NO_KEY) != QT_NO_KEY)
            .collect();
        let mut settings = json!({});
        if let Some(first) = keys.first() {
            let key = key_name(first);
            if key.is_none() || keys.len() > 1 {
                self.report.hotkeys_changed += 1;
            }
            let flag = |field| actions::bool_setting(first, field);
            actions::set_hotkey(
                &mut settings,
                &Hotkey {
                    ctrl: flag("KeyCtrl"),
                    shift: flag("KeyShift"),
                    alt: flag("KeyOption"),
                    meta: flag("KeyCmd"),
                    key: key.unwrap_or_default(),
                },
            );
        }
        settings
    }
}

fn title_style(state: &Value) -> TitleStyle {
    let mut style = TitleStyle::default();
    if let Some(size) = number(state, "FontSize").and_then(|n| u8::try_from(n).ok()) {
        style.size = size;
    }
    if let Some(show) = state.get("ShowTitle").and_then(Value::as_bool) {
        style.show = show;
    }
    if let Some(color) = hex_color(text(state, "TitleColor")) {
        style.color = color;
    }
    style.align = match text(state, "TitleAlignment") {
        "top" => TitleAlign::Top,
        "middle" => TitleAlign::Middle,
        _ => TitleAlign::Bottom,
    };
    style
}

fn hex_color(text: &str) -> Option<[u8; 3]> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let part = |at| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    Some([part(0)?, part(2)?, part(4)?])
}

/// Open Application stores a program and its arguments.
fn open_app(s: &Value) -> Value {
    let program = match text(s, "exec") {
        "" => text(s, "bundle_path"),
        exec => exec,
    };
    let args = text(s, "args").trim();
    let quoted = if program.contains(char::is_whitespace) {
        format!("\"{program}\"")
    } else {
        program.to_owned()
    };
    let path = if args.is_empty() {
        quoted
    } else {
        format!("{quoted} {args}")
    };
    json!({ "path": path })
}

fn web_address(s: &Value) -> String {
    let url = text(s, "path").trim();
    if url.is_empty() || url.contains("://") {
        url.to_owned()
    } else {
        format!("https://{url}")
    }
}

/// `actionIdx` follows the order of the official list and defaults to
/// Play / Pause. Later entries are display and keyboard brightness controls.
fn media_key(s: &Value) -> Option<MediaKey> {
    const ORDER: [MediaKey; 7] = [
        MediaKey::Previous,
        MediaKey::PlayPause,
        MediaKey::Next,
        MediaKey::Stop,
        MediaKey::Mute,
        MediaKey::VolumeUp,
        MediaKey::VolumeDown,
    ];
    let index = usize::try_from(number(s, "actionIdx").unwrap_or(1)).ok()?;
    ORDER.get(index).copied()
}

/// The name Deckly gives a recorded key. Key codes are Qt's public values;
/// the Windows key code covers letters typed on other keyboard layouts.
fn key_name(hotkey: &Value) -> Option<String> {
    let code = number(hotkey, "QTKeyCode")?;
    qt_key_name(code).or_else(|| {
        let vk = number(hotkey, "VKeyCode")?;
        matches!(vk, 0x30..=0x39 | 0x41..=0x5A)
            .then(|| u8::try_from(vk).ok().map(|c| char::from(c).to_string()))
            .flatten()
    })
}

fn qt_key_name(code: i64) -> Option<String> {
    const F1: i64 = 0x0100_0030;
    if let Ok(c) = u8::try_from(code)
        && (c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Some(char::from(c).to_string());
    }
    if (F1..F1 + 24).contains(&code) {
        return Some(format!("F{}", code - F1 + 1));
    }
    let name = match code {
        0x20 => "Space",
        0x27 => "Quote",
        0x2B => "Plus",
        0x2C => "Comma",
        0x2D => "Minus",
        0x2E => "Period",
        0x2F => "Slash",
        0x3B => "Semicolon",
        0x3D => "Equals",
        0x5B => "OpenBracket",
        0x5C => "Backslash",
        0x5D => "CloseBracket",
        0x60 => "Backtick",
        0x0100_0000 => "Escape",
        0x0100_0001 => "Tab",
        0x0100_0003 => "Backspace",
        0x0100_0004 | 0x0100_0005 => "Enter",
        0x0100_0006 => "Insert",
        0x0100_0007 => "Delete",
        0x0100_0010 => "Home",
        0x0100_0011 => "End",
        0x0100_0012 => "Left",
        0x0100_0013 => "Up",
        0x0100_0014 => "Right",
        0x0100_0015 => "Down",
        0x0100_0016 => "PageUp",
        0x0100_0017 => "PageDown",
        _ => return None,
    };
    Some(name.to_owned())
}

/// A profile's name, for messages about it.
pub fn name_of(manifest: &Value) -> &str {
    text(manifest, "Name")
}

/// Parses a manifest; an error names no content of the file.
pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    value
        .is_object()
        .then_some(value)
        .ok_or_else(|| "not an object".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "11111111-1111-4111-8111-111111111111";
    const PAGE_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const PAGE_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const FOLDER: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    const EMPTY: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

    fn raw(actions_a: Value) -> RawProfile {
        let keypad =
            |actions: Value| json!({"Controllers": [{"Type": "Keypad", "Actions": actions}]});
        RawProfile {
            id: PROFILE.to_owned(),
            manifest: json!({
                "Device": {"Model": "20GBA9901", "UUID": "@(1)[4057/128/SERIAL]"},
                "Name": "Stream",
                "Pages": {"Current": PAGE_A, "Default": EMPTY, "Pages": [PAGE_A, PAGE_B]},
            }),
            pages: BTreeMap::from([
                (PAGE_A.to_owned(), keypad(actions_a)),
                (PAGE_B.to_owned(), keypad(json!({}))),
                (
                    FOLDER.to_owned(),
                    keypad(json!({
                        "0,0": {"UUID": "com.elgato.streamdeck.profile.backtoparent"},
                        "4,2": {"UUID": "com.example.plugin.action", "Settings": {"a": 1},
                                "Plugin": {"UUID": "com.example.plugin", "Name": "Example"}},
                    })),
                ),
                (
                    EMPTY.to_owned(),
                    json!({"Controllers": [{"Type": "Keypad", "Actions": null}]}),
                ),
            ]),
        }
    }

    fn run(actions: Value) -> Converted {
        convert(&raw(actions), &mut |page, path| {
            (path != "Images/missing.png").then(|| format!("{page}/{path}").into())
        })
        .unwrap()
    }

    fn one(action: Value) -> KeyConfig {
        run(json!({ "0,0": action })).profile.pages[0].keys[&0].clone()
    }

    #[test]
    fn folder_names_decode_to_uuids() {
        // A pair seen inside a profile file of the official app.
        assert_eq!(
            folder_id("5HSEH1BMVL7QF42H7OFTOW7MQ8Z").as_deref(),
            Some("2c78e885-76f5-4fa7-9051-3e1fdc7cf6d2")
        );
        assert_eq!(
            folder_id("86AAFEB9-0DB7-4BFE-B73E-BBEED57EDABF").as_deref(),
            Some("86aafeb9-0db7-4bfe-b73e-bbeed57edabf")
        );
        assert_eq!(folder_id("../evil"), None);
        assert_eq!(folder_id("UUUUUUUUUUUUUUUUUUUUUUUUUUZ"), None);
        assert_eq!(folder_id(""), None);
    }

    #[test]
    fn the_device_id_wins_over_the_model_code() {
        let mut profile = raw(json!({}));
        profile.manifest["Device"] = json!({"Model": "20GAA9902", "UUID": "@(1)[4057/128/S]"});
        let c = convert(&profile, &mut |_, _| None).unwrap();
        assert_eq!(c.profile.device, DeviceKind::Mk2);
        profile.manifest["Device"] = json!({"Model": "20GBJ9901", "UUID": "some-uuid"});
        let c = convert(&profile, &mut |_, _| None).unwrap();
        assert_eq!(c.profile.device, DeviceKind::Neo);
        profile.manifest["Device"] = json!({"Model": "AI Stream Deck", "UUID": "x"});
        assert!(convert(&profile, &mut |_, _| None).is_err());
    }

    /// `Devices` as the registry holds it, with these sorting texts in it.
    fn devices_setting(sortings: &[&str]) -> Vec<u8> {
        let text = |s: &str| {
            let bytes: Vec<u8> = s.encode_utf16().flat_map(u16::to_be_bytes).collect();
            [(bytes.len() as u32).to_be_bytes().to_vec(), bytes].concat()
        };
        let mut stream = b"@Variant(\0\0\0\x08".to_vec();
        for sorting in sortings {
            stream.extend(text("ESDProfilesPreferred"));
            stream.extend([0, 0, 0, 10]);
            stream.extend(text(PAGE_A));
            stream.extend(text("ESDProfilesSorting"));
            stream.extend([0, 0, 0, 10]);
            stream.extend(text(sorting));
        }
        stream.into_iter().flat_map(|b| [b, 0]).collect()
    }

    #[test]
    fn the_profile_order_is_read_for_every_device() {
        let one = "b9433aac-8ce9-47ad-aa83-9bce812883f0";
        let two = "E5A67A77-94C9-40B8-A913-0399E1C9E40A";
        let three = "10b1491f-6147-4d97-b782-3eb82311afe6";
        let value = devices_setting(&[&format!("{one},{two},nonsense"), &format!("{three},{one}")]);
        assert_eq!(
            profile_order(&value),
            [one, &two.to_lowercase(), three].map(str::to_owned)
        );
    }

    #[test]
    fn a_damaged_devices_setting_gives_no_order() {
        assert!(profile_order(b"").is_empty());
        let mut value = devices_setting(&["b9433aac-8ce9-47ad-aa83-9bce812883f0"]);
        // Cut inside the ids, past the length that announces them.
        value.truncate(value.len() - 20);
        assert!(profile_order(&value).is_empty());
    }

    #[test]
    fn pages_keep_their_name_and_icon() {
        let mut profile = raw(json!({}));
        let page = profile.pages.get_mut(PAGE_A).unwrap();
        page["Name"] = json!("Scenes");
        page["Icon"] = json!("Images/ICON.svg");
        let mut asked = Vec::new();
        let mut image = |page: &str, picture: &str| {
            asked.push((page.to_owned(), picture.to_owned()));
            Some(PathBuf::from("kept.svg"))
        };
        let c = convert(&profile, &mut image).unwrap();
        assert_eq!(c.profile.pages[0].name, "Scenes");
        assert_eq!(c.profile.pages[0].icon, Some(PathBuf::from("kept.svg")));
        assert_eq!(asked, [(PAGE_A.to_owned(), "Images/ICON.svg".to_owned())]);
        // Pages without them stay as they are.
        assert_eq!(c.profile.pages[1].name, "");
        assert_eq!(c.profile.pages[1].icon, None);
    }

    #[test]
    fn pages_and_folders_become_pages_and_folders() {
        let c = run(json!({
            "4,2": {"UUID": "com.elgato.streamdeck.profile.openchild",
                    "Settings": {"ProfileUUID": FOLDER.to_uppercase()},
                    "States": [{"Title": "More"}]},
            "9,9": {"UUID": "com.example.off.the.grid"},
        }));
        let p = &c.profile;
        assert_eq!(p.id, PROFILE);
        assert_eq!(p.name, "Stream");
        assert_eq!(p.device, DeviceKind::Mk2);
        assert_eq!(p.pages.len(), 2);
        assert_eq!(p.pages[0].id, PAGE_A);
        let key = &p.pages[0].keys[&14];
        assert_eq!(key.action, actions::CREATE_FOLDER);
        assert_eq!(key.title, "More");
        assert_eq!(key.folder(), Some(FOLDER));
        let folder = p.folder(FOLDER).unwrap();
        assert_eq!(folder.keys[&0].action, actions::PARENT_FOLDER);
        assert_eq!(folder.keys[&14].action, "com.example.plugin.action");
        assert_eq!(folder.keys[&14].settings, json!({"a": 1}));
        // The empty default page leads nowhere and is left out.
        assert_eq!(p.folders.len(), 1);
        assert_eq!(
            c.report.plugins["com.example.plugin"],
            ("Example".into(), 1)
        );
    }

    #[test]
    fn folder_cycles_do_not_hang() {
        let mut profile = raw(json!({
            "0,0": {"UUID": "com.elgato.streamdeck.profile.openchild",
                    "Settings": {"ProfileUUID": FOLDER}}}));
        profile.pages.insert(
            FOLDER.to_owned(),
            json!({"Controllers": [{"Actions": {"1,0": {
                "UUID": "com.elgato.streamdeck.profile.openchild",
                "Settings": {"ProfileUUID": FOLDER}}}}]}),
        );
        let c = convert(&profile, &mut |_, _| None).unwrap();
        assert!(c.profile.folder(FOLDER).is_some());
    }

    #[test]
    fn system_actions_are_translated() {
        let key = one(
            json!({"UUID": "com.elgato.streamdeck.system.hotkey", "Settings": {
            "Hotkeys": [
                {"KeyCtrl": true, "KeyShift": true, "KeyOption": true, "KeyCmd": false,
                 "QTKeyCode": 88, "VKeyCode": 88},
                {"QTKeyCode": QT_NO_KEY, "VKeyCode": -1}]}}),
        );
        assert_eq!(key.action, actions::HOTKEY);
        let hotkey = actions::hotkey_setting(&key.settings);
        assert!(hotkey.ctrl && hotkey.shift && hotkey.alt && !hotkey.meta);
        assert_eq!(hotkey.key, "X");

        let key = one(
            json!({"UUID": "com.elgato.streamdeck.system.hotkey", "Settings": {
            "Hotkeys": [{"QTKeyCode": 0x0100_003E, "VKeyCode": -1}]}}),
        );
        assert_eq!(actions::hotkey_setting(&key.settings).key, "F15");

        let key = one(json!({"UUID": "com.elgato.streamdeck.system.website",
            "Settings": {"path": "example.com/a", "openInBrowser": true}}));
        assert_eq!(
            actions::str_setting(&key.settings, "url"),
            "https://example.com/a"
        );

        let key = one(json!({"UUID": "com.elgato.streamdeck.system.text",
            "Settings": {"pastedText": "hi", "isSendingEnter": true}}));
        assert_eq!(key.settings, json!({"text": "hi", "enter": true}));

        let key = one(json!({"UUID": "com.elgato.streamdeck.system.open",
            "Settings": {"path": "\"C:\\a b\\c.exe\" -x"}}));
        assert_eq!(
            actions::str_setting(&key.settings, "path"),
            "\"C:\\a b\\c.exe\" -x"
        );

        let key = one(json!({"UUID": "com.elgato.streamdeck.system.openapp",
            "Settings": {"exec": "C:/Program Files/a.exe", "args": "--go"}}));
        assert_eq!(key.action, actions::OPEN);
        assert_eq!(
            actions::str_setting(&key.settings, "path"),
            "\"C:/Program Files/a.exe\" --go"
        );

        let key = one(json!({"UUID": "com.elgato.streamdeck.page.goto",
            "Settings": {"PageIndex": 2}}));
        assert_eq!(key.settings, json!({"page": 2}));

        let key = one(json!({"UUID": "com.elgato.streamdeck.profile.rotate",
            "Settings": {"ProfileUUID": PROFILE.to_uppercase(), "PageIndex": 1}}));
        assert_eq!(actions::str_setting(&key.settings, "profile"), PROFILE);
    }

    #[test]
    fn media_keys_follow_the_official_order() {
        let media = |settings| {
            let key = one(json!({"UUID": "com.elgato.streamdeck.system.multimedia",
                "Settings": settings}));
            (key.action.clone(), actions::media_setting(&key.settings))
        };
        assert_eq!(
            media(json!({})),
            (actions::MULTIMEDIA.to_owned(), Some(MediaKey::PlayPause))
        );
        assert_eq!(media(json!({"actionIdx": 0})).1, Some(MediaKey::Previous));
        assert_eq!(media(json!({"actionIdx": 2})).1, Some(MediaKey::Next));
        assert_eq!(media(json!({"actionIdx": 6})).1, Some(MediaKey::VolumeDown));
        assert_eq!(
            media(json!({"actionIdx": 9})).0,
            "com.elgato.streamdeck.system.multimedia"
        );
    }

    #[test]
    fn multi_actions_keep_their_lists_delays_and_state() {
        let member = |uuid: &str| json!({"UUID": uuid, "Settings": {"x": 1}});
        let mut timed = member("com.example.b");
        timed["MultiActionDelays"] = json!({"Delay1": 5, "Delay2": 50});
        timed["States"] = json!([{}, {}]);
        timed["OverrideState"] = json!(1);
        let key = one(json!({
            "UUID": "com.elgato.streamdeck.multiactions.routine2",
            "State": 1,
            "Actions": [
                {"Actions": [member("com.example.a"),
                             {"UUID": "com.elgato.streamdeck.multiactions.delay",
                              "Settings": {"delay": 3000}}]},
                {"Actions": [timed]},
            ],
        }));
        assert_eq!(key.action, actions::MULTI_ACTION_SWITCH);
        assert_eq!(actions::switch_side(&key), 1);
        assert_eq!(key.groups[0].len(), 2);
        assert_eq!(key.groups[0][1].action, actions::DELAY);
        assert_eq!(key.groups[0][1].settings, json!({"delay": 3000}));
        let timed = &key.groups[1][0];
        assert_eq!(timed.delays, Some(ActionDelays { press: 5, next: 50 }));
        assert_eq!(timed.desired_state, Some(1));
    }

    #[test]
    fn nested_multi_actions_stop_at_one_level() {
        let inner = json!({"UUID": "com.elgato.streamdeck.multiactions.routine",
            "Actions": [{"Actions": [{"UUID": "com.example.deep"}]}]});
        let key = one(json!({"UUID": "com.elgato.streamdeck.multiactions.routine",
            "Actions": [{"Actions": [inner, {"UUID": "com.example.a"}]}]}));
        assert_eq!(key.groups[0].len(), 1);
        assert_eq!(key.groups[0][0].action, "com.example.a");
    }

    #[test]
    fn key_logic_holds_one_action_per_press() {
        let key = one(
            json!({"UUID": "com.elgato.streamdeck.keys.logic", "Actions": [
                {"UUID": "com.example.single"},
                {},
                {"UUID": "com.example.hold"},
            ]}),
        );
        assert_eq!(key.action, actions::KEY_LOGIC);
        assert_eq!(key.groups[0][0].action, "com.example.single");
        assert!(key.groups[1].is_empty());
        assert_eq!(key.groups[2][0].action, "com.example.hold");
    }

    #[test]
    fn titles_pictures_and_styles_come_from_the_states() {
        let key = one(json!({
            "UUID": "com.example.toggle",
            "State": 1,
            "States": [
                {"Title": "Off", "Image": "Images/off.png"},
                {"Image": "Images/on.png", "FontSize": 18, "TitleColor": "#ff8000",
                 "TitleAlignment": "middle", "ShowTitle": true},
            ],
        }));
        // The current state has no title of its own.
        assert_eq!(key.title, "Off");
        assert_eq!(key.image, Some(format!("{PAGE_A}/Images/on.png").into()));
        let style = key.title_style.unwrap();
        assert_eq!(style.size, 18);
        assert_eq!(style.color, [0xFF, 0x80, 0x00]);
        assert_eq!(style.align, TitleAlign::Middle);

        let c = run(json!({"0,0": {"UUID": "com.example.a",
            "States": [{"Image": "Images/missing.png"}, {"Image": "Images/b.png"}]}}));
        assert_eq!(c.report.missing_images, 1);
        assert_eq!(c.report.state_variants, 1);
    }

    #[test]
    fn built_in_actions_without_a_counterpart_are_kept_and_reported() {
        let c = run(json!({"0,0": {"UUID": "com.elgato.streamdeck.system.close",
            "Settings": {"path": "x.exe"}}}));
        let key = &c.profile.pages[0].keys[&0];
        assert_eq!(key.action, "com.elgato.streamdeck.system.close");
        assert_eq!(key.settings, json!({"path": "x.exe"}));
        assert_eq!(
            c.report.unsupported["com.elgato.streamdeck.system.close"],
            1
        );
    }

    #[test]
    fn dials_are_counted_and_left_out() {
        let mut profile = raw(json!({}));
        profile.pages.insert(
            PAGE_B.to_owned(),
            json!({"Controllers": [
                {"Type": "Encoder", "Actions": {"0,0": {"UUID": "com.example.dial"}}},
                {"Type": "{Invalid}", "Actions": null}]}),
        );
        let c = convert(&profile, &mut |_, _| None).unwrap();
        assert_eq!(c.report.dials, 1);
        assert!(c.profile.pages[1].keys.is_empty());
    }

    #[test]
    fn wrongly_typed_fields_read_as_missing() {
        let mut profile = raw(json!({
            "0,0": {"UUID": 5, "Settings": "x", "States": "y", "Actions": 7, "State": "z"},
            "a,b": {"UUID": "com.example.a"},
            "1": {"UUID": "com.example.b"},
        }));
        profile.manifest["Pages"] = json!("nope");
        assert!(convert(&profile, &mut |_, _| None).is_ok());
        let c = convert(&raw(json!([1, 2])), &mut |_, _| None).unwrap();
        assert!(c.profile.pages[0].keys.is_empty());
        assert!(parse(b"[1]").is_err());
        assert!(parse(b"\xEF\xBB\xBF{}").is_ok());
    }
}

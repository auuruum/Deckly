//! Messages of the plugin protocol (docs/clean-room/2026-10-28-plugin-host.md):
//! parsing commands from plugins and property inspectors, and building the
//! events sent to them. Pure functions, no I/O.

use serde_json::{Value, json};

/// Largest message accepted from a plugin; `setImage` carries whole images.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 200;
const MAX_STATE: u64 = 15;

/// What a plugin or property inspector asks the host to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetTitle {
        context: String,
        title: Option<String>,
        state: Option<usize>,
    },
    SetImage {
        context: String,
        image: Option<String>,
        state: Option<usize>,
    },
    SetState {
        context: String,
        state: usize,
    },
    SetSettings {
        context: String,
        settings: Value,
    },
    GetSettings {
        context: String,
    },
    SetGlobalSettings {
        settings: Value,
    },
    GetGlobalSettings,
    OpenUrl {
        url: String,
    },
    LogMessage {
        message: String,
    },
    ShowAlert {
        context: String,
    },
    ShowOk {
        context: String,
    },
    /// Plugin to its property inspector.
    SendToPropertyInspector {
        context: String,
        payload: Value,
    },
    /// Property inspector to its plugin.
    SendToPlugin {
        context: String,
        payload: Value,
    },
    /// Show a profile of the plugin on a device; without a name, the one
    /// shown before. `page` counts from 0.
    SwitchToProfile {
        device: String,
        profile: Option<String>,
        page: Option<usize>,
    },
    /// A command Deckly does not handle (yet), by event name.
    Other(String),
}

pub fn parse_command(text: &str) -> Result<Command, String> {
    if text.len() > MAX_MESSAGE_BYTES {
        return Err("message too large".into());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let event = v
        .get("event")
        .and_then(Value::as_str)
        .ok_or("message without an event")?;
    let context = || str_field(&v, "context");
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    let state = payload
        .get("state")
        .and_then(Value::as_u64)
        .filter(|&s| s <= MAX_STATE)
        .map(|s| s as usize);
    let object = |p: Value| if p.is_object() { p } else { json!({}) };
    Ok(match event {
        "setTitle" => Command::SetTitle {
            context: context(),
            title: payload
                .get("title")
                .and_then(Value::as_str)
                .map(|t| t.chars().take(MAX_TITLE_CHARS).collect()),
            state,
        },
        "setImage" => Command::SetImage {
            context: context(),
            image: payload
                .get("image")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            state,
        },
        "setState" => Command::SetState {
            context: context(),
            state: state.ok_or("setState without a state")?,
        },
        "setSettings" => Command::SetSettings {
            context: context(),
            settings: object(payload),
        },
        "getSettings" => Command::GetSettings { context: context() },
        "setGlobalSettings" => Command::SetGlobalSettings {
            settings: object(payload),
        },
        "getGlobalSettings" => Command::GetGlobalSettings,
        "openUrl" => Command::OpenUrl {
            url: str_field(&payload, "url"),
        },
        "logMessage" => Command::LogMessage {
            message: str_field(&payload, "message"),
        },
        "showAlert" => Command::ShowAlert { context: context() },
        "showOk" => Command::ShowOk { context: context() },
        "sendToPropertyInspector" => Command::SendToPropertyInspector {
            context: context(),
            payload,
        },
        "sendToPlugin" => Command::SendToPlugin {
            context: context(),
            payload,
        },
        "switchToProfile" => Command::SwitchToProfile {
            device: str_field(&v, "device"),
            profile: payload
                .get("profile")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            page: payload
                .get("page")
                .and_then(Value::as_u64)
                .map(|p| p as usize),
        },
        other => Command::Other(other.chars().take(64).collect()),
    })
}

/// Reads the registration message: `{"event": ..., "uuid": ...}`.
pub fn parse_registration(text: &str) -> Option<(String, String)> {
    if text.len() > 4096 {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let event = v.get("event")?.as_str()?.to_owned();
    let uuid = v.get("uuid")?.as_str()?.to_owned();
    Some((event, uuid))
}

fn str_field(v: &Value, field: &str) -> String {
    v.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// What the registration info says about the host.
#[derive(Clone, Debug)]
pub struct AppInfo {
    pub language: String,
    pub platform_version: String,
    pub font: String,
}

/// A connected device as plugins see it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub columns: u8,
    pub rows: u8,
    /// The SDK's device type number.
    pub kind: u8,
}

/// The protocol level Deckly follows; the SDK library refuses features when
/// the host reports a lower version.
pub const APP_VERSION: &str = "7.6.0";

/// The `-info` argument for a plugin, and the `info` argument of its
/// property inspector.
pub fn registration_info(
    app: &AppInfo,
    platform: &str,
    plugin: &str,
    version: &str,
    devices: &[DeviceInfo],
) -> String {
    json!({
        "application": {
            "font": app.font,
            "language": app.language,
            "platform": platform,
            "platformVersion": app.platform_version,
            "version": APP_VERSION,
        },
        "colors": {
            "buttonMouseOverBackgroundColor": "#464646FF",
            "buttonPressedBackgroundColor": "#303030FF",
            "buttonPressedBorderColor": "#646464FF",
            "buttonPressedTextColor": "#969696FF",
            "highlightColor": "#0078FFFF",
        },
        "devicePixelRatio": 2,
        "devices": devices.iter().map(device_json).collect::<Vec<_>>(),
        "plugin": { "uuid": plugin, "version": version },
    })
    .to_string()
}

fn device_json(d: &DeviceInfo) -> Value {
    json!({
        "id": d.id,
        "name": d.name,
        "size": { "columns": d.columns, "rows": d.rows },
        "type": d.kind,
    })
}

/// One placed action as events describe it.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub action: String,
    pub context: String,
    pub device: String,
    pub settings: Value,
    pub column: u8,
    pub row: u8,
    /// Only for actions with more than one state.
    pub state: Option<usize>,
    /// Inside a Multi Action, where an action has no place on the device.
    pub in_multi_action: bool,
    /// The state the user picked for this action in its Multi Action.
    pub desired_state: Option<usize>,
}

impl Instance {
    fn payload(&self) -> Value {
        let mut p = json!({
            "settings": if self.settings.is_object() { self.settings.clone() } else { json!({}) },
            "controller": "Keypad",
            "isInMultiAction": self.in_multi_action,
        });
        if let Some(map) = p.as_object_mut() {
            if !self.in_multi_action {
                map.insert(
                    "coordinates".into(),
                    json!({ "column": self.column, "row": self.row }),
                );
            }
            if let Some(state) = self.state {
                map.insert("state".into(), state.into());
            }
            if let Some(state) = self.desired_state {
                map.insert("userDesiredState".into(), state.into());
            }
        }
        p
    }

    /// `keyDown`, `keyUp`, `willAppear`, `willDisappear`,
    /// `didReceiveSettings`.
    pub fn event(&self, event: &str) -> String {
        json!({
            "event": event,
            "action": self.action,
            "context": self.context,
            "device": self.device,
            "payload": self.payload(),
        })
        .to_string()
    }

    pub fn title_parameters(&self, title: &str, show: bool, size: u8, align: &str) -> String {
        let mut payload = self.payload();
        if let Some(map) = payload.as_object_mut() {
            map.insert("title".into(), title.into());
            map.insert(
                "titleParameters".into(),
                json!({
                    "fontFamily": "",
                    "fontSize": size,
                    "fontStyle": "",
                    "fontUnderline": false,
                    "showTitle": show,
                    "titleAlignment": align,
                    "titleColor": "#ffffff",
                }),
            );
        }
        json!({
            "event": "titleParametersDidChange",
            "action": self.action,
            "context": self.context,
            "device": self.device,
            "payload": payload,
        })
        .to_string()
    }

    /// `propertyInspectorDidAppear` / `propertyInspectorDidDisappear`.
    pub fn inspector_event(&self, event: &str) -> String {
        json!({
            "event": event,
            "action": self.action,
            "context": self.context,
            "device": self.device,
        })
        .to_string()
    }

    /// The `actionInfo` argument of a property inspector.
    pub fn action_info(&self) -> String {
        json!({
            "action": self.action,
            "context": self.context,
            "device": self.device,
            "payload": self.payload(),
        })
        .to_string()
    }
}

pub fn did_receive_global_settings(settings: &Value) -> String {
    json!({ "event": "didReceiveGlobalSettings", "payload": { "settings": settings } }).to_string()
}

pub fn device_did_connect(d: &DeviceInfo) -> String {
    json!({
        "event": "deviceDidConnect",
        "device": d.id,
        "deviceInfo": {
            "name": d.name,
            "size": { "columns": d.columns, "rows": d.rows },
            "type": d.kind,
        },
    })
    .to_string()
}

pub fn device_did_disconnect(id: &str) -> String {
    json!({ "event": "deviceDidDisconnect", "device": id }).to_string()
}

pub fn system_did_wake_up() -> String {
    json!({ "event": "systemDidWakeUp" }).to_string()
}

/// `sendToPlugin` (to a plugin) or `sendToPropertyInspector` (to its
/// inspector).
pub fn forward(event: &str, action: &str, context: &str, payload: &Value) -> String {
    json!({ "event": event, "action": action, "context": context, "payload": payload }).to_string()
}

/// A stable id for a device: 32 hex digits derived from its serial, so the
/// serial itself is not handed to plugins.
pub fn device_id(serial: &str) -> String {
    // FNV-1a with two offsets; stable across builds, unlike DefaultHasher.
    let hash = |mut h: u64| {
        for b in serial.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    };
    format!(
        "{:016x}{:016x}",
        hash(0xcbf2_9ce4_8422_2325),
        hash(0x6c62_272e_07bb_0142)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let c = parse_command(
            r#"{"event":"setTitle","context":"c1","payload":{"title":"Hi","state":1,"target":0}}"#,
        );
        assert_eq!(
            c,
            Ok(Command::SetTitle {
                context: "c1".into(),
                title: Some("Hi".into()),
                state: Some(1)
            })
        );
        let c = parse_command(r#"{"event":"setImage","context":"c","payload":{"image":""}}"#);
        assert_eq!(
            c,
            Ok(Command::SetImage {
                context: "c".into(),
                image: None,
                state: None
            })
        );
        let c = parse_command(r#"{"event":"setSettings","context":"c","payload":[1]}"#);
        assert_eq!(
            c,
            Ok(Command::SetSettings {
                context: "c".into(),
                settings: json!({})
            })
        );
        assert_eq!(
            parse_command(r#"{"event":"setFeedback","context":"c"}"#),
            Ok(Command::Other("setFeedback".into()))
        );
        assert!(
            parse_command(r#"{"event":"setState","context":"c","payload":{"state":99}}"#).is_err()
        );
        assert_eq!(
            parse_command(
                r#"{"event":"switchToProfile","device":"d","payload":{"profile":"profiles/A","page":2}}"#
            ),
            Ok(Command::SwitchToProfile {
                device: "d".into(),
                profile: Some("profiles/A".into()),
                page: Some(2)
            })
        );
        // Going back: the SDK leaves out an undefined profile.
        assert_eq!(
            parse_command(r#"{"event":"switchToProfile","device":"d","payload":{}}"#),
            Ok(Command::SwitchToProfile {
                device: "d".into(),
                profile: None,
                page: None
            })
        );
        assert!(parse_command(r#"{"context":"c"}"#).is_err());
        assert!(parse_command("garbage").is_err());
        let long = format!(
            r#"{{"event":"setTitle","payload":{{"title":"{}"}}}}"#,
            "x".repeat(500)
        );
        match parse_command(&long) {
            Ok(Command::SetTitle { title: Some(t), .. }) => assert_eq!(t.len(), 200),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn registration_messages() {
        assert_eq!(
            parse_registration(r#"{"event":"registerPlugin","uuid":"abc"}"#),
            Some(("registerPlugin".into(), "abc".into()))
        );
        assert_eq!(parse_registration(r#"{"event":"registerPlugin"}"#), None);
    }

    #[test]
    fn events_carry_the_instance() {
        let i = Instance {
            action: "com.a.b.act".into(),
            context: "ctx".into(),
            device: "dev".into(),
            settings: json!({"x": 1}),
            column: 2,
            row: 1,
            state: Some(1),
            in_multi_action: false,
            desired_state: None,
        };
        let v: Value = serde_json::from_str(&i.event("keyDown")).unwrap();
        assert_eq!(v["event"], "keyDown");
        assert_eq!(v["payload"]["settings"]["x"], 1);
        assert_eq!(v["payload"]["coordinates"]["column"], 2);
        assert_eq!(v["payload"]["state"], 1);
        assert_eq!(v["payload"]["isInMultiAction"], false);
        assert!(v["payload"].get("userDesiredState").is_none());
        let member = Instance {
            in_multi_action: true,
            desired_state: Some(0),
            ..i.clone()
        };
        let v: Value = serde_json::from_str(&member.event("keyDown")).unwrap();
        assert_eq!(v["payload"]["isInMultiAction"], true);
        assert_eq!(v["payload"]["userDesiredState"], 0);
        assert!(v["payload"].get("coordinates").is_none());
        let single = Instance {
            state: None,
            settings: Value::Null,
            ..i
        };
        let v: Value = serde_json::from_str(&single.event("willAppear")).unwrap();
        assert!(v["payload"].get("state").is_none());
        assert_eq!(v["payload"]["settings"], json!({}));
    }

    #[test]
    fn info_has_the_documented_shape() {
        let app = AppInfo {
            language: "en".into(),
            platform_version: "10".into(),
            font: "".into(),
        };
        let d = DeviceInfo {
            id: device_id("SERIAL"),
            name: "Deck".into(),
            columns: 5,
            rows: 3,
            kind: 0,
        };
        let v: Value =
            serde_json::from_str(&registration_info(&app, "linux", "com.a.b", "1.0", &[d]))
                .unwrap();
        assert_eq!(v["application"]["version"], APP_VERSION);
        assert_eq!(v["application"]["platform"], "linux");
        assert_eq!(v["devices"][0]["size"]["columns"], 5);
        assert_eq!(v["plugin"]["uuid"], "com.a.b");
    }

    #[test]
    fn device_ids_are_stable_and_hide_the_serial() {
        let id = device_id("CL12345");
        assert_eq!(id.len(), 32);
        assert_eq!(id, device_id("CL12345"));
        assert_ne!(id, device_id("CL12346"));
        assert!(!id.contains("12345"));
    }
}

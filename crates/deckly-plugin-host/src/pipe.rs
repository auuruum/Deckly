//! Deckly plugins: their manifest (`deckly-plugin.json`) and the pipe
//! transport (docs/plugin-sdk.md).
//!
//! The plugin runs with pipes on its standard input and output and speaks
//! the frames of `deckly-plugin-protocol`. A writer thread owns its input
//! and a reader thread its output, so a plugin that stops reading or
//! writing never blocks the host: messages to it wait in a bounded queue,
//! and a full queue is answered with a restart. Its messages are translated
//! into the same [`Command`]s Stream Deck plugins produce, and the events
//! the app builds for Stream Deck plugins are translated into protocol
//! messages, so the app has one code path for both kinds.

use crate::manifest::{Action, Manifest, State};
use crate::protocol::Command;
use deckly_plugin_protocol::{
    self as proto, Device, HostMessage, KeyInfo, MANIFEST_FILE, PluginMessage, Reader, Received,
    encode_message,
};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{ChildStdin, ChildStdout};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

/// Messages waiting for a plugin that does not read them; more is a stuck
/// plugin.
const QUEUE_FRAMES: usize = 1024;

/// Whether a folder holds a Deckly plugin.
pub fn is_plugin_dir(dir: &Path) -> bool {
    dir.join(MANIFEST_FILE).is_file()
}

/// Reads `deckly-plugin.json` and describes the plugin the way the rest of
/// the host and the app know plugins.
pub fn load(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST_FILE);
    let len = std::fs::metadata(&path)
        .map_err(|e| format!("cannot read {MANIFEST_FILE}: {e}"))?
        .len();
    if len > proto::limits::MAX_MANIFEST_BYTES as u64 {
        return Err(format!("{MANIFEST_FILE} is too large ({len} bytes)"));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {MANIFEST_FILE}: {e}"))?;
    let m = proto::Manifest::parse(&bytes)?;
    Ok(describe(m))
}

/// The host's view of a Deckly manifest.
pub fn describe(m: proto::Manifest) -> Manifest {
    let actions = m
        .actions
        .iter()
        .map(|a| Action {
            uuid: a.id.clone(),
            name: if a.name.trim().is_empty() {
                a.id.clone()
            } else {
                a.name.clone()
            },
            icon: a.icon.clone(),
            tooltip: a.tooltip.clone(),
            states: a
                .states
                .iter()
                .map(|s| State {
                    image: s.image.clone(),
                    title: s.title.clone(),
                    show_title: true,
                })
                .collect(),
            property_inspector: String::new(),
            controllers: vec!["Keypad".into()],
            // The plugin moves its keys between states itself.
            automatic_states: false,
            visible: true,
            user_title: true,
            in_multi_actions: true,
            in_key_logic: true,
        })
        .collect();
    Manifest {
        uuid: m.id.clone(),
        name: m.name.clone(),
        version: m.version.clone(),
        author: m.author.clone(),
        description: m.description.clone(),
        icon: m.icon.clone(),
        category: m.category.clone(),
        category_icon: m.icon.clone(),
        code_path: String::new(),
        code_path_win: String::new(),
        code_path_mac: String::new(),
        code_path_lin: String::new(),
        property_inspector: String::new(),
        sdk_version: 0,
        node_version: None,
        node_debug: String::new(),
        platforms: Vec::new(),
        profiles: Vec::new(),
        actions,
        deckly: Some(Arc::new(m)),
    }
}

/// The program to run here, or why there is none.
pub fn program(dir: &Path, m: &proto::Manifest) -> Result<std::path::PathBuf, String> {
    if !m.supported() {
        return Err(format!(
            "needs a newer Deckly (plugin protocol {}, this Deckly speaks {})",
            m.protocol,
            proto::PROTOCOL
        ));
    }
    let target = proto::current_target();
    let rel = m
        .program_for(&target)
        .ok_or_else(|| format!("has no program for {target}"))?;
    crate::manifest::find_file(dir, rel).ok_or_else(|| format!("its program {rel} is missing"))
}

/// The host's end of a running plugin's input: a bounded queue of encoded
/// frames, written by the writer thread.
#[derive(Clone)]
pub struct Pipe {
    tx: SyncSender<Vec<u8>>,
}

pub enum SendError {
    /// The plugin does not read its input.
    Full,
    /// The plugin is gone.
    Closed,
}

impl Pipe {
    /// Starts the writer thread for a plugin's standard input. The input
    /// closes, which tells the plugin to exit, once every `Pipe` clone is
    /// dropped and the queue is written out.
    pub fn start(stdin: ChildStdin) -> std::io::Result<Self> {
        let (tx, rx) = sync_channel(QUEUE_FRAMES);
        std::thread::Builder::new()
            .name("deckly-plugin-write".into())
            .spawn(move || write_frames(stdin, rx))?;
        Ok(Self { tx })
    }

    pub fn send_frame(&self, frame: Vec<u8>) -> Result<(), SendError> {
        self.tx.try_send(frame).map_err(|e| match e {
            TrySendError::Full(_) => SendError::Full,
            TrySendError::Disconnected(_) => SendError::Closed,
        })
    }

    pub fn send(&self, message: &HostMessage) -> Result<(), SendError> {
        match encode_message(message) {
            Ok(frame) => self.send_frame(frame),
            // Host messages are built from bounded data; one that is not
            // is dropped rather than sent.
            Err(_) => Ok(()),
        }
    }
}

fn write_frames(mut stdin: ChildStdin, rx: Receiver<Vec<u8>>) {
    while let Ok(frame) = rx.recv() {
        if stdin
            .write_all(&frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

/// What the reader thread reports.
pub enum Incoming {
    Message(Received<PluginMessage>),
    /// The plugin broke the protocol; it is restarted.
    Error(String),
    /// The plugin closed its output, normally by exiting.
    Closed,
}

/// Starts the reader thread for a plugin's standard output.
pub fn read(
    stdout: ChildStdout,
    report: impl Fn(Incoming) -> bool + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("deckly-plugin-read".into())
        .spawn(move || read_frames(stdout, report))
        .map(drop)
}

fn read_frames(stdout: impl Read, report: impl Fn(Incoming) -> bool) {
    let mut reader = Reader::new(stdout);
    loop {
        match reader.read::<PluginMessage>() {
            Ok(Some(m)) => {
                if !report(Incoming::Message(m)) {
                    return;
                }
            }
            Ok(None) => {
                report(Incoming::Closed);
                return;
            }
            Err(proto::Error::Io(_)) => {
                report(Incoming::Closed);
                return;
            }
            Err(e) => {
                report(Incoming::Error(e.to_string()));
                return;
            }
        }
    }
}

/// The opening message.
pub fn hello(language: &str, devices: &[crate::DeviceInfo]) -> HostMessage {
    HostMessage::Hello(proto::Hello {
        protocol: proto::PROTOCOL,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: crate::manifest::current_platform().to_owned(),
        session: session().to_owned(),
        language: proto::truncate(language, 32).to_owned(),
        devices: devices
            .iter()
            .take(proto::limits::MAX_DEVICES)
            .map(device)
            .collect(),
        services: Vec::new(),
    })
}

fn device(d: &crate::DeviceInfo) -> Device {
    Device {
        id: d.id.clone(),
        name: proto::truncate(&d.name, proto::limits::MAX_NAME_CHARS).to_owned(),
        columns: d.columns,
        rows: d.rows,
    }
}

/// The Linux desktop session: `wayland`, `x11` or empty.
fn session() -> &'static str {
    if !cfg!(target_os = "linux") {
        return "";
    }
    let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if set("WAYLAND_DISPLAY") {
        "wayland"
    } else if set("DISPLAY") {
        "x11"
    } else {
        ""
    }
}

/// Translates an event the app built for Stream Deck plugins (see
/// [`crate::protocol`]) into a protocol message; `None` for events Deckly
/// plugins do not get. The text comes from the app, not from a plugin.
pub fn translate(text: &str) -> Option<HostMessage> {
    let v: Value = serde_json::from_str(text).ok()?;
    let str_of = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let message = match v.get("event")?.as_str()? {
        "keyDown" => HostMessage::KeyDown { key: key_info(&v)? },
        "keyUp" => HostMessage::KeyUp { key: key_info(&v)? },
        "willAppear" => HostMessage::KeyAppear { key: key_info(&v)? },
        "willDisappear" => HostMessage::KeyDisappear { key: key_info(&v)? },
        "didReceiveSettings" => HostMessage::Settings { key: key_info(&v)? },
        "didReceiveGlobalSettings" => HostMessage::GlobalSettings {
            settings: v
                .pointer("/payload/settings")
                .filter(|s| s.is_object())
                .cloned()
                .unwrap_or_else(|| json!({})),
        },
        "deviceDidConnect" => {
            let info = v.get("deviceInfo")?;
            let size = |k: &str| {
                info.pointer(&format!("/size/{k}"))
                    .and_then(Value::as_u64)
                    .and_then(|n| u8::try_from(n).ok())
                    .unwrap_or(0)
            };
            HostMessage::DeviceConnected {
                device: Device {
                    id: str_of(&v, "device"),
                    name: str_of(info, "name"),
                    columns: size("columns"),
                    rows: size("rows"),
                },
            }
        }
        "deviceDidDisconnect" => HostMessage::DeviceDisconnected {
            device: str_of(&v, "device"),
        },
        "systemDidWakeUp" => HostMessage::SystemWake,
        _ => return None,
    };
    Some(message)
}

/// A placed key as a Deckly plugin sees it.
pub fn key_of(instance: &crate::protocol::Instance) -> Option<KeyInfo> {
    let v: Value = serde_json::from_str(&instance.event("keyDown")).ok()?;
    key_info(&v)
}

/// Lets `filter` see and change the settings in a message before it goes
/// to the plugin (the app puts secrets back in).
pub fn filter_settings(message: &mut HostMessage, filter: &dyn Fn(&mut Value)) {
    match message {
        HostMessage::KeyAppear { key }
        | HostMessage::KeyDisappear { key }
        | HostMessage::KeyDown { key }
        | HostMessage::KeyUp { key }
        | HostMessage::Settings { key }
        | HostMessage::GetChoices { key: Some(key), .. }
        | HostMessage::Button { key: Some(key), .. } => filter(&mut key.settings),
        HostMessage::GlobalSettings { settings } => filter(settings),
        _ => {}
    }
}

fn key_info(v: &Value) -> Option<KeyInfo> {
    let payload = v.get("payload")?;
    let small = |p: &str| {
        payload
            .pointer(p)
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok())
    };
    Some(KeyInfo {
        context: v.get("context")?.as_str()?.to_owned(),
        action: v.get("action")?.as_str()?.to_owned(),
        device: v
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        column: small("/coordinates/column").unwrap_or(0),
        row: small("/coordinates/row").unwrap_or(0),
        state: small("/state"),
        settings: payload
            .get("settings")
            .filter(|s| s.is_object())
            .cloned()
            .unwrap_or_else(|| json!({})),
        in_multi_action: payload
            .get("isInMultiAction")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Turns a plugin message into a host command. `Err` is a protocol error;
/// `Ok(None)` is a message that needs nothing from the app.
pub fn command(r: Received<PluginMessage>) -> Result<Option<Command>, String> {
    let state = |s: Option<u8>| s.map(usize::from);
    Ok(Some(match r.message {
        PluginMessage::SetTitle {
            context,
            title,
            state: s,
        } => Command::SetTitle {
            context,
            title,
            state: state(s),
        },
        PluginMessage::SetState { context, state: s } => Command::SetState {
            context,
            state: usize::from(s),
        },
        PluginMessage::SetFace { context, face } => Command::SetFace { context, face },
        PluginMessage::SetImage {
            context,
            format: None,
            state: s,
        } => Command::SetImage {
            context,
            image: None,
            state: state(s),
        },
        PluginMessage::SetImage {
            context,
            format: Some(format),
            state: s,
        } => {
            let data = r
                .blob
                .ok_or("set_image with a format but no blob before it")?;
            if format.raw_len().is_some_and(|n| n != data.len()) {
                return Err("a raw image whose size does not match its pixels".into());
            }
            Command::SetImageData {
                context,
                format,
                data,
                state: state(s),
            }
        }
        PluginMessage::SetSettings { context, settings } => {
            Command::SetSettings { context, settings }
        }
        PluginMessage::SetGlobalSettings { settings } => Command::SetGlobalSettings { settings },
        PluginMessage::GetGlobalSettings => Command::GetGlobalSettings,
        PluginMessage::ShowOk { context } => Command::ShowOk { context },
        PluginMessage::ShowAlert { context } => Command::ShowAlert { context },
        PluginMessage::Choices {
            request,
            options,
            error,
        } => Command::Choices {
            request,
            options,
            error,
        },
        PluginMessage::Ready { .. } | PluginMessage::Unknown => return Ok(None),
    }))
}

/// Checks the plugin's answer to `hello`.
pub fn check_ready(message: &PluginMessage) -> Result<(), String> {
    match message {
        PluginMessage::Ready { protocol, .. }
            if (proto::MIN_PROTOCOL..=proto::PROTOCOL).contains(protocol) =>
        {
            Ok(())
        }
        PluginMessage::Ready { protocol, .. } => Err(format!(
            "it speaks plugin protocol {protocol}; this Deckly speaks {} to {}",
            proto::MIN_PROTOCOL,
            proto::PROTOCOL
        )),
        _ => Err("it sent something else before answering hello".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Instance;
    use deckly_plugin_protocol::{Face, ImageFormat, Validate};

    fn instance() -> Instance {
        Instance {
            action: "com.example.counter.count".into(),
            context: "page.3".into(),
            device: "dev".into(),
            settings: json!({"step": 2}),
            column: 3,
            row: 0,
            state: Some(1),
            in_multi_action: false,
            desired_state: None,
        }
    }

    #[test]
    fn app_events_become_protocol_messages() {
        let i = instance();
        let Some(HostMessage::KeyDown { key }) = translate(&i.event("keyDown")) else {
            panic!("not a key down");
        };
        assert_eq!(key.context, "page.3");
        assert_eq!(key.column, 3);
        assert_eq!(key.state, Some(1));
        assert_eq!(key.settings, json!({"step": 2}));
        assert!(key.validate().is_ok());
        assert!(matches!(
            translate(&i.event("willAppear")),
            Some(HostMessage::KeyAppear { .. })
        ));
        assert!(matches!(
            translate(&i.event("didReceiveSettings")),
            Some(HostMessage::Settings { .. })
        ));
        let member = Instance {
            in_multi_action: true,
            settings: Value::Null,
            ..i.clone()
        };
        let Some(HostMessage::KeyUp { key }) = translate(&member.event("keyUp")) else {
            panic!("not a key up");
        };
        assert!(key.in_multi_action);
        assert_eq!(key.settings, json!({}));
        let d = crate::DeviceInfo {
            id: "d1".into(),
            name: "Deck".into(),
            columns: 5,
            rows: 3,
            kind: 0,
        };
        assert_eq!(
            translate(&crate::protocol::device_did_connect(&d)),
            Some(HostMessage::DeviceConnected {
                device: Device {
                    id: "d1".into(),
                    name: "Deck".into(),
                    columns: 5,
                    rows: 3
                }
            })
        );
        assert_eq!(
            translate(&crate::protocol::did_receive_global_settings(
                &json!({"a": 1})
            )),
            Some(HostMessage::GlobalSettings {
                settings: json!({"a": 1})
            })
        );
        // Stream Deck only events are not passed on.
        assert_eq!(
            translate(&i.inspector_event("propertyInspectorDidAppear")),
            None
        );
        assert_eq!(translate("not json"), None);
    }

    #[test]
    fn plugin_messages_become_commands() {
        let msg = |message| Received {
            message,
            blob: None,
        };
        assert_eq!(
            command(msg(PluginMessage::SetFace {
                context: "c".into(),
                face: Some(Face::new().value("1")),
            })),
            Ok(Some(Command::SetFace {
                context: "c".into(),
                face: Some(Face::new().value("1"))
            }))
        );
        let image = |format| PluginMessage::SetImage {
            context: "c".into(),
            format,
            state: Some(1),
        };
        assert!(command(msg(image(Some(ImageFormat::Png)))).is_err());
        let with_blob = Received {
            message: image(Some(ImageFormat::Png)),
            blob: Some(vec![1, 2]),
        };
        assert!(matches!(
            command(with_blob),
            Ok(Some(Command::SetImageData { state: Some(1), .. }))
        ));
        let wrong_size = Received {
            message: image(Some(ImageFormat::Rgba {
                width: 2,
                height: 2,
            })),
            blob: Some(vec![0; 15]),
        };
        assert!(command(wrong_size).is_err());
        assert!(matches!(
            command(msg(image(None))),
            Ok(Some(Command::SetImage { image: None, .. }))
        ));
        assert_eq!(command(msg(PluginMessage::Unknown)), Ok(None));
    }

    #[test]
    fn choices_answers_and_settings_filters() {
        let answer = Received {
            message: PluginMessage::Choices {
                request: 9,
                options: vec![deckly_plugin_protocol::Choice::new("a", "A")],
                error: None,
            },
            blob: None,
        };
        assert!(matches!(
            command(answer),
            Ok(Some(Command::Choices { request: 9, ref options, error: None })) if options.len() == 1
        ));
        let key = key_of(&instance()).unwrap();
        assert_eq!(key.context, "page.3");
        assert_eq!(key.settings, json!({"step": 2}));
        let reveal = |v: &mut Value| {
            if let Some(o) = v.as_object_mut() {
                o.insert("secret".into(), json!("shh"));
            }
        };
        let mut down = HostMessage::KeyDown { key: key.clone() };
        filter_settings(&mut down, &reveal);
        let HostMessage::KeyDown { key: shown } = down else {
            panic!("changed type");
        };
        assert_eq!(shown.settings["secret"], "shh");
        let mut global = HostMessage::GlobalSettings {
            settings: json!({}),
        };
        filter_settings(&mut global, &reveal);
        assert_eq!(
            global,
            HostMessage::GlobalSettings {
                settings: json!({"secret": "shh"})
            }
        );
        let mut ask = HostMessage::GetChoices {
            request: 1,
            field: "f".into(),
            key: Some(key),
        };
        filter_settings(&mut ask, &reveal);
        assert!(
            matches!(ask, HostMessage::GetChoices { key: Some(k), .. } if k.settings["secret"] == "shh")
        );
    }

    #[test]
    fn ready_must_come_first_with_a_known_protocol() {
        let ready = |protocol| PluginMessage::Ready {
            protocol,
            sdk: String::new(),
        };
        assert!(check_ready(&ready(proto::PROTOCOL)).is_ok());
        assert!(check_ready(&ready(proto::PROTOCOL + 1)).is_err());
        assert!(check_ready(&ready(0)).is_err());
        assert!(check_ready(&PluginMessage::GetGlobalSettings).is_err());
    }

    #[test]
    fn manifests_describe_actions_for_the_app() {
        let dir = std::env::temp_dir().join(format!("deckly-pipe-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_plugin_dir(&dir));
        let text = json!({
            "id": "com.example.counter", "name": "Counter", "version": "1.0", "protocol": 1,
            "programs": {proto::current_target(): "counter"},
            "actions": [{"id": "com.example.counter.count", "name": "Count",
                         "states": [{"title": "A"}, {"title": "B", "image": "b.png"}]}]
        });
        std::fs::write(dir.join(MANIFEST_FILE), text.to_string()).unwrap();
        assert!(is_plugin_dir(&dir));
        let m = load(&dir).unwrap();
        assert_eq!(m.uuid, "com.example.counter");
        assert!(m.deckly.is_some());
        let a = m.action("com.example.counter.count").unwrap();
        assert!(a.on_keys() && !a.automatic_states);
        assert_eq!(a.states[1].image, "b.png");
        let deckly = m.deckly.as_ref().unwrap();
        assert!(program(&dir, deckly).is_err_and(|e| e.contains("missing")));
        std::fs::write(dir.join("counter"), "").unwrap();
        assert_eq!(program(&dir, deckly).unwrap(), dir.join("counter"));
        let mut newer = (**deckly).clone();
        newer.protocol = proto::PROTOCOL + 1;
        assert!(program(&dir, &newer).is_err_and(|e| e.contains("newer Deckly")));
        newer.protocol = 1;
        newer.programs.clear();
        assert!(program(&dir, &newer).is_err_and(|e| e.contains("no program")));
        std::fs::write(dir.join(MANIFEST_FILE), "{\"id\": 5}").unwrap();
        assert!(load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

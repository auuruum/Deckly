use super::*;
use crate::frame::{KIND_BLOB, KIND_MESSAGE};
use crate::limits::*;
use serde_json::json;
use std::io::Cursor;

fn key(context: &str) -> KeyInfo {
    KeyInfo {
        context: context.into(),
        action: "com.example.counter.count".into(),
        device: "0123".into(),
        column: 2,
        row: 1,
        state: Some(1),
        settings: json!({"step": 2}),
        in_multi_action: false,
    }
}

fn hello() -> HostMessage {
    HostMessage::Hello(Hello {
        protocol: PROTOCOL,
        app_version: "0.0.4".into(),
        platform: "linux".into(),
        session: "wayland".into(),
        language: "en".into(),
        devices: vec![Device {
            id: "abc".into(),
            name: "Deck".into(),
            columns: 5,
            rows: 3,
        }],
        services: vec![],
    })
}

fn stream(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.concat()
}

fn raw(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(body);
    out
}

#[test]
fn host_messages_round_trip() {
    let messages = vec![
        hello(),
        HostMessage::DeviceConnected {
            device: Device {
                id: "d".into(),
                name: "n".into(),
                columns: 8,
                rows: 4,
            },
        },
        HostMessage::DeviceDisconnected { device: "d".into() },
        HostMessage::KeyAppear { key: key("p.1") },
        HostMessage::KeyDisappear { key: key("p.1") },
        HostMessage::KeyDown { key: key("p.1") },
        HostMessage::KeyUp { key: key("p.1") },
        HostMessage::Settings { key: key("p.1") },
        HostMessage::GlobalSettings {
            settings: json!({"a": [1, 2]}),
        },
        HostMessage::SystemWake,
    ];
    let bytes: Vec<u8> = messages
        .iter()
        .flat_map(|m| encode_message(m).unwrap())
        .collect();
    let mut r = Reader::new(Cursor::new(bytes));
    for m in &messages {
        let got = r.read::<HostMessage>().unwrap().unwrap();
        assert_eq!(&got.message, m);
        assert_eq!(got.blob, None);
    }
    assert!(r.read::<HostMessage>().unwrap().is_none());
}

#[test]
fn plugin_messages_round_trip_with_blobs() {
    let face = Face {
        label: Some("Count".into()),
        value: Some("12".into()),
        detail: None,
        progress: Some(0.5),
        color: Some(Color::rgb(255, 128, 0)),
        background: None,
        accent: Some(Color::rgb(0, 0, 0)),
        icon: Some("icons/count.png".into()),
    };
    let image = PluginMessage::SetImage {
        context: "p.1".into(),
        format: Some(ImageFormat::Png),
        state: None,
    };
    let mut w = Writer::new(Vec::new());
    w.write_message(&PluginMessage::Ready {
        protocol: 1,
        sdk: "0.1.0".into(),
    })
    .unwrap();
    w.write_message(&PluginMessage::SetFace {
        context: "p.1".into(),
        face: Some(face.clone()),
    })
    .unwrap();
    w.write_blob(b"\x89PNG...").unwrap();
    w.write_message(&image).unwrap();
    w.write_message(&PluginMessage::GetGlobalSettings).unwrap();
    let mut r = Reader::new(Cursor::new(w.into_inner()));
    let ready = r.read::<PluginMessage>().unwrap().unwrap();
    assert!(matches!(
        ready.message,
        PluginMessage::Ready { protocol: 1, .. }
    ));
    match r.read::<PluginMessage>().unwrap().unwrap().message {
        PluginMessage::SetFace { face: Some(f), .. } => assert_eq!(f, face),
        other => panic!("{other:?}"),
    }
    let got = r.read::<PluginMessage>().unwrap().unwrap();
    assert_eq!(got.message, image);
    assert_eq!(got.blob.as_deref(), Some(&b"\x89PNG..."[..]));
    let got = r.read::<PluginMessage>().unwrap().unwrap();
    assert_eq!(got.message, PluginMessage::GetGlobalSettings);
    assert_eq!(got.blob, None);
}

#[test]
fn the_wire_format_is_as_documented() {
    let frame = encode_message(&PluginMessage::ShowOk {
        context: "c".into(),
    })
    .unwrap();
    let body = br#"{"type":"show_ok","context":"c"}"#;
    assert_eq!(&frame[..4], &(body.len() as u32).to_le_bytes());
    assert_eq!(frame[4], KIND_MESSAGE);
    assert_eq!(&frame[5..], body);
    let blob = encode_blob(&[1, 2, 3]).unwrap();
    assert_eq!(blob, [3, 0, 0, 0, KIND_BLOB, 1, 2, 3]);
    let c: String = Color::rgb(1, 0xab, 255).into();
    assert_eq!(c, "#01abff");
}

#[test]
fn unknown_messages_fields_and_frame_kinds_are_ignored() {
    let bytes = stream(&[
        raw(KIND_MESSAGE, br#"{"type":"teleport","to":"mars"}"#),
        raw(7, b"future frame kind"),
        raw(
            KIND_MESSAGE,
            br#"{"type":"show_alert","context":"c","new_field":{"x":1}}"#,
        ),
    ]);
    let mut r = Reader::new(Cursor::new(bytes));
    assert_eq!(
        r.read::<PluginMessage>().unwrap().unwrap().message,
        PluginMessage::Unknown
    );
    assert_eq!(
        r.read::<PluginMessage>().unwrap().unwrap().message,
        PluginMessage::ShowAlert {
            context: "c".into()
        }
    );
    let wake = br#"{"type":"system_wake","extra":true}"#;
    assert_eq!(
        decode::<HostMessage>(wake).unwrap(),
        HostMessage::SystemWake
    );
    assert_eq!(
        decode::<HostMessage>(br#"{"type":"dial_rotate","ticks":3}"#).unwrap(),
        HostMessage::Unknown
    );
}

#[test]
fn a_clean_end_is_none_and_a_cut_frame_is_an_error() {
    let mut r = Reader::new(Cursor::new(Vec::new()));
    assert!(r.read_frame().unwrap().is_none());
    let full = encode_message(&PluginMessage::GetGlobalSettings).unwrap();
    for cut in [2, 5, full.len() - 1] {
        let mut r = Reader::new(Cursor::new(full[..cut].to_vec()));
        assert!(
            matches!(r.read_frame(), Err(Error::Truncated)),
            "cut at {cut}"
        );
    }
    // A blob with no message after it.
    let mut r = Reader::new(Cursor::new(encode_blob(b"x").unwrap()));
    assert!(matches!(r.read::<PluginMessage>(), Err(Error::Truncated)));
}

#[test]
fn oversized_frames_are_refused_before_allocating() {
    let mut header = (u32::MAX).to_le_bytes().to_vec();
    header.push(KIND_BLOB);
    let mut r = Reader::new(Cursor::new(header));
    assert!(matches!(r.read_frame(), Err(Error::TooLarge { .. })));
    let mut header = ((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes().to_vec();
    header.push(KIND_MESSAGE);
    let mut r = Reader::new(Cursor::new(header));
    assert!(matches!(r.read_frame(), Err(Error::TooLarge { .. })));
    // A blob may be larger than a message.
    let big = vec![0u8; MAX_MESSAGE_BYTES + 1];
    let mut r = Reader::new(Cursor::new(encode_blob(&big).unwrap()));
    assert_eq!(r.read_frame().unwrap().unwrap().body.len(), big.len());
    assert!(encode_blob(&vec![0u8; MAX_BLOB_BYTES + 1]).is_err());
    let huge = PluginMessage::SetSettings {
        context: "c".into(),
        settings: json!({"x": "y".repeat(MAX_MESSAGE_BYTES)}),
    };
    assert!(matches!(encode_message(&huge), Err(Error::TooLarge { .. })));
}

#[test]
fn printed_text_is_reported_as_text() {
    let mut r = Reader::new(Cursor::new(b"hello from println!\n".to_vec()));
    match r.read_frame() {
        Err(Error::Text(t)) => assert_eq!(t, "hello from println!"),
        other => panic!("{other:?}"),
    }
    let mut r = Reader::new(Cursor::new(b"ok\n".to_vec()));
    assert!(matches!(r.read_frame(), Err(Error::Text(t)) if t == "ok"));
    let e = Error::Text("hi".into()).to_string();
    assert!(e.contains("standard error"), "{e}");
}

#[test]
fn hostile_messages_are_refused() {
    let bad: &[&[u8]] = &[
        b"not json",
        b"[1,2,3]",
        br#"{"no_type":1}"#,
        // Known type, missing or wrong fields.
        br#"{"type":"set_state","context":"c"}"#,
        br#"{"type":"set_state","context":"c","state":-1}"#,
        br#"{"type":"set_state","context":"c","state":300}"#,
        br#"{"type":"set_state","context":"c","state":16}"#,
        br#"{"type":"set_title","context":"","title":"x"}"#,
        br#"{"type":"set_settings","context":"c","settings":[1]}"#,
        br#"{"type":"set_face","context":"c","face":{"progress":1.5}}"#,
        br#"{"type":"set_face","context":"c","face":{"color":"red"}}"#,
        br##"{"type":"set_face","context":"c","face":{"color":"#12345"}}"##,
        br#"{"type":"set_image","context":"c","format":{"kind":"rgba","width":0,"height":5}}"#,
        br#"{"type":"set_image","context":"c","format":{"kind":"rgba","width":5000,"height":5}}"#,
        br#"{"type":"set_image","context":"c","format":{"kind":"bmp"}}"#,
    ];
    for body in bad {
        assert!(
            decode::<PluginMessage>(body).is_err(),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    let long_title =
        json!({"type":"set_title","context":"c","title":"é".repeat(MAX_TITLE_CHARS + 1)});
    assert!(decode::<PluginMessage>(long_title.to_string().as_bytes()).is_err());
    let ok_title = json!({"type":"set_title","context":"c","title":"é".repeat(MAX_TITLE_CHARS)});
    assert!(decode::<PluginMessage>(ok_title.to_string().as_bytes()).is_ok());
    let long_context = json!({"type":"show_ok","context":"c".repeat(MAX_CONTEXT_BYTES + 1)});
    assert!(decode::<PluginMessage>(long_context.to_string().as_bytes()).is_err());
    let big = json!({"type":"set_global_settings","settings":{"x":"y".repeat(MAX_SETTINGS_BYTES)}});
    assert!(decode::<PluginMessage>(big.to_string().as_bytes()).is_err());
    // Deep nesting stops at serde_json's recursion limit instead of the stack.
    let deep = format!(
        r#"{{"type":"set_global_settings","settings":{}{}}}"#,
        "[".repeat(10_000),
        "]".repeat(10_000)
    );
    assert!(decode::<PluginMessage>(deep.as_bytes()).is_err());
    // Two blobs in a row.
    let bytes = stream(&[encode_blob(b"a").unwrap(), encode_blob(b"b").unwrap()]);
    let mut r = Reader::new(Cursor::new(bytes));
    assert!(matches!(r.read::<PluginMessage>(), Err(Error::Invalid(_))));
}

#[test]
fn hostile_host_messages_are_refused_too() {
    let mut many = match hello() {
        HostMessage::Hello(h) => h,
        _ => unreachable!(),
    };
    many.devices = vec![many.devices[0].clone(); MAX_DEVICES + 1];
    let bytes = encode_message(&HostMessage::Hello(many)).unwrap();
    assert!(decode::<HostMessage>(&bytes[5..]).is_err());
    let bad_action = json!({"type":"key_down","key":{"context":"c","action":"../x"}});
    assert!(decode::<HostMessage>(bad_action.to_string().as_bytes()).is_err());
    // Missing optional key fields take their defaults.
    let minimal = json!({"type":"key_down","key":{"context":"c","action":"a.b"}});
    match decode::<HostMessage>(minimal.to_string().as_bytes()).unwrap() {
        HostMessage::KeyDown { key } => {
            assert_eq!(key.settings, json!({}));
            assert_eq!(key.state, None);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn truncate_counts_characters() {
    assert_eq!(truncate("héllo", 2), "hé");
    assert_eq!(truncate("hi", 5), "hi");
    assert_eq!(truncate("", 0), "");
}

const MANIFEST: &str = r#"{
    "id": "com.example.counter", "name": "Counter", "version": "0.1.0",
    "protocol": 1, "sdk": "0.1.0", "future": {"anything": true},
    "programs": {"x86_64-unknown-linux-gnu": "bin/counter",
                 "aarch64-apple-darwin": "bin/counter-mac",
                 "x86_64-pc-windows-msvc": "bin\\counter.exe"},
    "actions": [{"id": "com.example.counter.count", "name": "Count",
                 "states": [{"title": "0"}], "form": {"fields": []}}]
}"#;

#[test]
fn manifests_parse_and_pick_a_program() {
    let m = Manifest::parse(MANIFEST.as_bytes()).unwrap();
    assert_eq!(m.id, "com.example.counter");
    assert!(m.supported());
    assert_eq!(
        m.action("com.example.counter.count").unwrap().states.len(),
        1
    );
    assert_eq!(
        m.program_for("x86_64-unknown-linux-gnu"),
        Some("bin/counter")
    );
    // Same processor and system, other C library.
    assert_eq!(
        m.program_for("x86_64-unknown-linux-musl"),
        Some("bin/counter")
    );
    assert_eq!(m.program_for("aarch64-unknown-linux-gnu"), None);
    assert_eq!(
        m.program_for("x86_64-pc-windows-gnu"),
        Some("bin\\counter.exe")
    );
    assert_eq!(m.program_for("garbage"), None);
    // What --describe writes reads back the same.
    let text = serde_json::to_string(&m).unwrap();
    assert_eq!(Manifest::parse(text.as_bytes()).unwrap(), m);
    let t = current_target();
    assert!(t.starts_with(std::env::consts::ARCH), "{t}");
}

#[test]
fn bad_manifests_are_refused() {
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        edit(&mut v);
        Manifest::parse(v.to_string().as_bytes())
    };
    assert!(with(&|_| {}).is_ok());
    assert!(with(&|v| v["id"] = json!("../evil")).is_err());
    assert!(with(&|v| v["name"] = json!("  ")).is_err());
    assert!(with(&|v| v["protocol"] = json!("one")).is_err());
    assert!(with(&|v| v["programs"]["x"] = json!("../../bin/sh")).is_err());
    assert!(with(&|v| v["programs"]["x"] = json!("/bin/sh")).is_err());
    assert!(with(&|v| v["programs"]["x"] = json!("C:\\x.exe")).is_err());
    assert!(with(&|v| v["icon"] = json!("..\\x.png")).is_err());
    assert!(with(&|v| v["actions"] = json!([v["actions"][0], v["actions"][0]])).is_err());
    assert!(with(&|v| v["actions"][0]["id"] = json!("")).is_err());
    assert!(with(&|v| v["actions"][0]["states"] = json!(vec![json!({}); MAX_STATES + 1])).is_err());
    assert!(with(&|v| v["actions"] = json!(vec![json!({}); MAX_ACTIONS + 1])).is_err());
    assert!(with(&|v| v["form"] = big_form()).is_err());
    assert!(with(&|v| v["actions"][0]["form"] = big_form()).is_err());
    assert!(with(&|v| v["form"] = json!({"fields": "none"})).is_err());
    let newer = with(&|v| v["protocol"] = json!(PROTOCOL + 1)).unwrap();
    assert!(!newer.supported());
    assert!(Manifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
    assert!(Manifest::parse(b"\xEF\xBB\xBF{}").is_err());
    let bom = format!("\u{FEFF}{MANIFEST}");
    assert!(Manifest::parse(bom.as_bytes()).is_ok());
}

#[test]
fn built_faces_always_pass_the_checks() {
    let face = Face::new()
        .label("x".repeat(100))
        .value("12")
        .detail("é".repeat(MAX_FACE_TEXT_CHARS + 5))
        .progress(7.0)
        .accent(Color::rgb(1, 2, 3));
    assert!(face.validate().is_ok());
    assert_eq!(face.progress, Some(1.0));
    assert_eq!(
        face.label.as_deref().map(str::len),
        Some(MAX_FACE_TEXT_CHARS)
    );
    assert_eq!(Face::new().progress(f32::NAN).progress, None);
    assert_eq!(Face::new().progress(-1.0).progress, Some(0.0));
}

// ----- settings forms ------------------------------------------------------

/// Valid fields that add up to more than a form may hold.
fn big_form() -> serde_json::Value {
    let options: Vec<_> = (0..200)
        .map(|i| json!({"value": format!("option-{i}")}))
        .collect();
    let fields: Vec<_> = (0..MAX_FORM_FIELDS)
        .map(|i| json!({"kind": "choice", "key": format!("c{i}"), "options": options}))
        .collect();
    json!({ "fields": fields })
}

fn sample_form() -> Form {
    serde_json::from_value(json!({"fields": [
        {"kind": "section", "label": "Counting"},
        {"kind": "number", "key": "step", "label": "Step", "min": 1, "max": 100,
         "integer": true, "default": 1, "help": "Added on each press"},
        {"kind": "text", "key": "name", "label": "Name", "min_length": 2, "max_length": 5},
        {"kind": "secret", "key": "token", "label": "Token", "required": true},
        {"kind": "checkbox", "key": "advanced", "label": "Advanced", "default": false},
        {"kind": "slider", "key": "level", "label": "Level", "min": 0, "max": 10, "step": 0.5,
         "visible_when": {"field": "advanced", "equals": true}},
        {"kind": "choice", "key": "mode", "label": "Mode", "default": "up",
         "options": [{"value": "up", "label": "Up"}, {"value": "down"}],
         "visible_when": {"field": "level", "equals": 5}},
        {"kind": "choice", "key": "scene", "label": "Scene", "dynamic": true},
        {"kind": "file", "key": "sound", "label": "Sound", "extensions": ["wav", "ogg"]},
        {"kind": "folder", "key": "dir", "label": "Folder"},
        {"kind": "color", "key": "tint", "label": "Tint", "default": "#ff8800"},
        {"kind": "hotkey", "key": "keys", "label": "Keys"},
        {"kind": "button", "key": "reset", "label": "Reset"},
        {"kind": "note", "label": "A note under everything."}
    ]}))
    .unwrap()
}

#[test]
fn forms_round_trip_in_manifests() {
    let form = sample_form();
    assert!(form.validate().is_ok(), "{:?}", form.validate());
    let mut m = Manifest {
        id: "com.example.counter".into(),
        name: "Counter".into(),
        version: "1".into(),
        protocol: 1,
        form: Some(form.clone()),
        ..Manifest::default()
    };
    m.actions.push(ManifestAction {
        id: "com.example.counter.count".into(),
        name: "Count".into(),
        form: Some(form.clone()),
        ..ManifestAction::default()
    });
    let text = serde_json::to_vec(&m).unwrap();
    let back = Manifest::parse(&text).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.actions[0].form.as_ref(), Some(&form));
    // Properties that are not set stay out of the file.
    let note = serde_json::to_value(&form.fields[13]).unwrap();
    assert_eq!(
        note,
        json!({"kind": "note", "label": "A note under everything."})
    );
}

#[test]
fn unknown_field_kinds_and_properties_are_ignored() {
    let form: Form = serde_json::from_value(json!({"fields": [
        {"kind": "map_picker", "key": "where", "zoom": 3, "label": "Where"},
        {"kind": "text", "key": "name", "label": "Name", "glow": true},
        {"kind": "map_picker", "key": "name"}
    ], "layout": "columns"}))
    .unwrap();
    assert!(form.validate().is_ok());
    assert_eq!(form.fields[0].kind, FieldKind::Unknown);
    assert!(!form.is_visible(0, &json!({})));
    assert!(form.is_visible(1, &json!({})));
    assert_eq!(form.field("name").map(|f| f.kind), Some(FieldKind::Text));
    assert_eq!(form.field("where"), None);
}

#[test]
fn hostile_forms_are_refused() {
    let field = |extra: serde_json::Value| {
        let mut f = json!({"kind": "text", "key": "k", "label": "L"});
        if let (Some(f), Some(extra)) = (f.as_object_mut(), extra.as_object()) {
            f.extend(extra.clone());
        }
        f
    };
    let bad = [
        field(json!({"key": ""})),
        field(json!({"key": "has space"})),
        field(json!({"key": "k".repeat(MAX_FIELD_KEY_BYTES + 1)})),
        field(json!({"label": "x".repeat(MAX_LABEL_CHARS + 1)})),
        field(json!({"help": "x".repeat(MAX_HELP_CHARS + 1)})),
        field(json!({"kind": "number", "min": 5, "max": 1})),
        field(json!({"kind": "number", "step": 0})),
        field(json!({"kind": "slider", "min": 0})),
        field(json!({"min_length": 9, "max_length": 3})),
        field(json!({"max_length": MAX_TEXT_VALUE_CHARS + 1})),
        field(
            json!({"kind": "choice", "options": [{"value": "v".repeat(MAX_CHOICE_VALUE_BYTES + 1)}]}),
        ),
        field(json!({"kind": "file", "extensions": ["../x"]})),
        field(json!({"kind": "number", "default": "five"})),
        field(json!({"kind": "choice", "options": [{"value": "a"}], "default": "b"})),
        field(json!({"default": "x".repeat(MAX_DEFAULT_BYTES)})),
        field(json!({"visible_when": {"field": "", "equals": 1}})),
        field(json!({"kind": "number", "min": "low"})),
    ];
    for f in bad {
        let form = json!({"fields": [f]});
        let parsed = serde_json::from_value::<Form>(form.clone());
        assert!(
            parsed.map_or(true, |p| p.validate().is_err()),
            "accepted {form}"
        );
    }
    let many = (0..=MAX_FORM_FIELDS)
        .map(|i| json!({"kind": "text", "key": format!("k{i}")}))
        .collect::<Vec<_>>();
    let form: Form = serde_json::from_value(json!({ "fields": many })).unwrap();
    assert!(form.validate().is_err());
    let options = (0..=MAX_CHOICES)
        .map(|i| json!({"value": i.to_string()}))
        .collect::<Vec<_>>();
    let form: Form = serde_json::from_value(
        json!({"fields": [{"kind": "choice", "key": "c", "options": options}]}),
    )
    .unwrap();
    assert!(form.validate().is_err());
    let twice: Form = serde_json::from_value(json!({"fields": [
        {"kind": "text", "key": "a"}, {"kind": "button", "key": "a"}
    ]}))
    .unwrap();
    assert!(twice.validate().is_err());
    // Within every other limit, but larger than a form may be.
    let huge: Form = serde_json::from_value(big_form()).unwrap();
    assert!(huge.fields.iter().all(|f| f.validate().is_ok()));
    assert!(huge.validate().is_err_and(|e| e.contains("larger")));
}

#[test]
fn field_values_are_checked_with_plain_messages() {
    let form = sample_form();
    let f = |k: &str| form.field(k).unwrap();
    assert_eq!(f("step").check(Some(&json!(5))), Ok(()));
    assert_eq!(
        f("step").check(Some(&json!(0))),
        Err("Enter a number from 1 to 100.".into())
    );
    assert_eq!(
        f("step").check(Some(&json!(2.5))),
        Err("Enter a whole number.".into())
    );
    assert_eq!(
        f("step").check(Some(&json!("2"))),
        Err("Enter a number.".into())
    );
    assert_eq!(f("step").check(None), Ok(()));
    assert_eq!(
        f("name").check(Some(&json!("a"))),
        Err("Use at least 2 characters.".into())
    );
    assert_eq!(
        f("name").check(Some(&json!("abcdef"))),
        Err("Use at most 5 characters.".into())
    );
    assert_eq!(f("name").check(Some(&json!("ééé"))), Ok(()));
    assert_eq!(f("name").check(Some(&json!(""))), Ok(()));
    assert_eq!(
        f("token").check(Some(&json!(""))),
        Err("This field is required.".into())
    );
    assert_eq!(f("mode").check(Some(&json!("down"))), Ok(()));
    assert!(f("mode").check(Some(&json!("sideways"))).is_err());
    // Dynamic choices accept what the plugin offered.
    assert_eq!(f("scene").check(Some(&json!("anything"))), Ok(()));
    assert_eq!(f("tint").check(Some(&json!("#00ff00"))), Ok(()));
    assert!(f("tint").check(Some(&json!("green"))).is_err());
    assert_eq!(
        f("keys").check(Some(&json!({"key": "K", "ctrl": true}))),
        Ok(())
    );
    assert!(
        f("keys")
            .check(Some(&json!({"key": "K", "ctrl": "yes"})))
            .is_err()
    );
    assert_eq!(f("advanced").check(Some(&json!(true))), Ok(()));
    assert!(f("advanced").check(Some(&json!(1))).is_err());
    let problems = form.problems(&json!({"step": 500, "token": "t"}));
    assert_eq!(
        problems,
        vec![(
            "step".to_owned(),
            "Enter a number from 1 to 100.".to_owned()
        )]
    );
    assert_eq!(
        form.problems(&json!({})),
        vec![("token".to_owned(), "This field is required.".to_owned())]
    );
    assert_eq!(form.secrets().collect::<Vec<_>>(), ["token"]);
}

#[test]
fn conditions_show_and_hide_fields() {
    let form = sample_form();
    let index = |k: &str| form.fields.iter().position(|f| f.key == k).unwrap();
    let (level, mode) = (index("level"), index("mode"));
    // Defaults count: "advanced" is false by default.
    assert!(!form.is_visible(level, &json!({})));
    assert!(form.is_visible(level, &json!({"advanced": true})));
    // "mode" needs level 5, and level itself must be shown.
    assert!(!form.is_visible(mode, &json!({"level": 5})));
    assert!(form.is_visible(mode, &json!({"level": 5, "advanced": true})));
    assert!(!form.is_visible(mode, &json!({"level": 4, "advanced": true})));
    // Hidden fields are not checked.
    assert!(
        form.problems(&json!({"token": "t", "level": 99}))
            .is_empty()
    );
    assert!(
        !form
            .problems(&json!({"token": "t", "level": 99, "advanced": true}))
            .is_empty()
    );
    // A loop of conditions hides its fields instead of hanging.
    let looped: Form = serde_json::from_value(json!({"fields": [
        {"kind": "checkbox", "key": "a", "default": true, "visible_when": {"field": "b", "equals": true}},
        {"kind": "checkbox", "key": "b", "default": true, "visible_when": {"field": "a", "equals": true}}
    ]}))
    .unwrap();
    assert!(looped.validate().is_ok());
    assert!(!looped.is_visible(0, &json!({})));
    assert!(!looped.is_visible(1, &json!({})));
    // A condition on a property no field shows uses the setting alone.
    let plain: Form = serde_json::from_value(json!({"fields": [
        {"kind": "text", "key": "a", "visible_when": {"field": "mode", "equals": "x"}}
    ]}))
    .unwrap();
    assert!(plain.is_visible(0, &json!({"mode": "x"})));
    assert!(!plain.is_visible(0, &json!({})));
}

#[test]
fn choice_and_button_messages_round_trip_and_are_bounded() {
    let messages = [
        HostMessage::GetChoices {
            request: 7,
            field: "scene".into(),
            key: Some(key("p.1")),
        },
        HostMessage::GetChoices {
            request: u64::MAX,
            field: "scene".into(),
            key: None,
        },
        HostMessage::Button {
            field: "reset".into(),
            key: None,
        },
    ];
    for m in &messages {
        let frame = encode_message(m).unwrap();
        assert_eq!(&decode::<HostMessage>(&frame[5..]).unwrap(), m);
    }
    let answer = PluginMessage::Choices {
        request: 7,
        options: vec![Choice::new("a", "A"), Choice::new("b", "")],
        error: None,
    };
    let frame = encode_message(&answer).unwrap();
    assert_eq!(decode::<PluginMessage>(&frame[5..]).unwrap(), answer);
    let failed = br#"{"type":"choices","request":3,"error":"OBS is not running"}"#;
    assert!(matches!(
        decode::<PluginMessage>(failed).unwrap(),
        PluginMessage::Choices { request: 3, ref options, error: Some(_) } if options.is_empty()
    ));
    let too_many = json!({"type": "choices", "request": 1,
        "options": (0..=MAX_CHOICES).map(|i| json!({"value": i.to_string()})).collect::<Vec<_>>()});
    assert!(decode::<PluginMessage>(too_many.to_string().as_bytes()).is_err());
    let long_error =
        json!({"type": "choices", "request": 1, "error": "x".repeat(MAX_ERROR_CHARS + 1)});
    assert!(decode::<PluginMessage>(long_error.to_string().as_bytes()).is_err());
    let no_request = br#"{"type":"choices","options":[]}"#;
    assert!(decode::<PluginMessage>(no_request).is_err());
    let bad_field = br#"{"type":"button","field":"has space"}"#;
    assert!(decode::<HostMessage>(bad_field).is_err());
    // Choices built in code always pass.
    let long = Choice::new("é".repeat(MAX_CHOICE_VALUE_BYTES), "x".repeat(500));
    assert!(long.validate().is_ok());
    assert_eq!(long.text(), "x".repeat(MAX_LABEL_CHARS));
}

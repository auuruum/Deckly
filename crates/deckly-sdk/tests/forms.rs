//! Settings forms, dynamic choices, buttons and global settings, tested
//! through the harness.
#![allow(clippy::panic)] // One handler panics on purpose.

use deckly_sdk::prelude::*;
use deckly_sdk::protocol::limits::{
    MAX_CHOICES, MAX_ERROR_CHARS, MAX_FORM_FIELDS, MAX_LABEL_CHARS,
};
use deckly_sdk::protocol::{FieldKind, Manifest, PluginMessage, Validate};
use deckly_sdk::testing::Harness;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Light {
    room: String,
    level: u8,
    presses: u32,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Bridge {
    address: String,
    token: String,
}

/// How often a light's globals hook ran.
static GLOBALS_SEEN: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Lamp;

impl Action for Lamp {
    const ID: &'static str = "com.example.lights.lamp";
    const NAME: &'static str = "Lamp";
    type Settings = Light;

    fn form(form: &mut Form) {
        form.section("Lamp");
        form.choice("room", "Room").dynamic().required();
        form.slider("level", "Level", 0..=100).default(50);
        form.button("blink", "Blink");
        form.button("count", "Count");
    }

    fn choices(field: &str, settings: &Light, globals: &Globals) -> Result<Vec<Choice>, String> {
        match field {
            "room" => {
                let bridge = globals.get::<Bridge>();
                if bridge.address.is_empty() {
                    return Err("Set the bridge address first.".into());
                }
                Ok(vec![
                    Choice::new("kitchen", format!("Kitchen at {}", bridge.address)),
                    Choice::new("hall", format!("Hall, level {}", settings.level)),
                ])
            }
            "slow" => {
                std::thread::sleep(Duration::from_millis(500));
                Ok(vec![Choice::new("late", "")])
            }
            "many" => Ok((0..MAX_CHOICES + 10)
                .map(|i| Choice::new(i.to_string(), ""))
                .collect()),
            "broken" => panic!("the handler broke"),
            "wordy" => Err("x".repeat(MAX_ERROR_CHARS * 2)),
            _ => Err(format!("no options for {field}")),
        }
    }

    fn key_down(&mut self, key: &Key<Self>) {
        key.set_title(key.settings().room);
    }

    fn button(&mut self, key: &Key<Self>, button: &str) {
        match button {
            "blink" => key.show_ok(),
            "count" => key.update_settings(|s| s.presses += 1),
            _ => {}
        }
    }

    fn global_settings_changed(&mut self, key: &Key<Self>) {
        GLOBALS_SEEN.fetch_add(1, Ordering::Relaxed);
        key.set_title(key.globals().get::<Bridge>().address);
    }
}

fn plugin() -> Plugin {
    Plugin::new("com.example.lights", "Lights")
        .action::<Lamp>()
        .form(|form| {
            form.text("address", "Bridge address")
                .placeholder("192.168.1.2")
                .required();
            form.secret("token", "Token");
            form.choice("bridge", "Bridge").dynamic();
            form.button("pair", "Pair");
        })
        .on_choices(|field, _| match field {
            "bridge" => Ok(vec![Choice::new("b1", "Bridge 1")]),
            _ => Err("unknown".into()),
        })
        .on_button(|button, globals| {
            if button == "pair" {
                globals.update::<Bridge>(|b| b.token = "paired".into());
            }
        })
}

#[test]
fn forms_go_into_the_manifest() {
    let p = plugin();
    assert!(p.check().is_ok());
    let m = p.manifest();
    let text = serde_json::to_string(&m).unwrap();
    let back = Manifest::parse(text.as_bytes()).unwrap();
    let form = back.actions[0].form.as_ref().unwrap();
    let kinds: Vec<_> = form.fields.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        [
            FieldKind::Section,
            FieldKind::Choice,
            FieldKind::Slider,
            FieldKind::Button,
            FieldKind::Button
        ]
    );
    let level = form.field("level").unwrap();
    assert_eq!(
        (level.min, level.max, level.integer),
        (Some(0.0), Some(100.0), true)
    );
    assert_eq!(level.default, Some(json!(50)));
    let global = back.form.as_ref().unwrap();
    assert_eq!(global.secrets().collect::<Vec<_>>(), ["token"]);
    assert!(global.field("address").unwrap().required);
    // An action without a form has none in the manifest.
    #[derive(Default)]
    struct Plain;
    impl Action for Plain {
        const ID: &'static str = "com.example.lights.plain";
        const NAME: &'static str = "Plain";
        type Settings = serde_json::Value;
    }
    let m = Plugin::new("com.example.lights", "Lights")
        .action::<Plain>()
        .manifest();
    assert_eq!(m.actions[0].form, None);
    assert_eq!(m.form, None);
}

#[test]
fn the_builder_keeps_forms_within_the_limits() {
    let mut form = Form::default();
    for i in 0..MAX_FORM_FIELDS + 5 {
        form.text(&format!("f{i}"), &"é".repeat(MAX_LABEL_CHARS * 2))
            .help("help");
    }
    form.number("n", "N").range(0.5..=2.5).step(0.5);
    let d = form.description();
    assert_eq!(d.fields.len(), MAX_FORM_FIELDS);
    assert!(d.validate().is_ok());
    assert_eq!(d.fields[0].label.chars().count(), MAX_LABEL_CHARS);
    // A default the field refuses is caught before Deckly sees it.
    let bad = Plugin::new("com.example.bad", "Bad").form(|f| {
        f.number("n", "N").range(1..=10).default(99);
    });
    assert!(
        bad.check()
            .is_err_and(|e| e.to_string().contains("default"))
    );
    let mut floats = Form::default();
    floats.number("x", "X").range(0.5..=2.5);
    assert!(!floats.description().fields[0].integer);
}

#[test]
fn dynamic_choices_are_answered_from_the_settings_and_globals() {
    let mut deck = Harness::start(plugin());
    // A key in the editor only, never shown on a device.
    deck.place("k1", Lamp::ID, json!({"level": 7}));
    assert_eq!(
        deck.choices("k1", "room", WAIT),
        Err("Set the bridge address first.".into())
    );
    deck.set_globals(json!({"address": "10.0.0.2"}));
    let rooms = deck.choices("k1", "room", WAIT).unwrap();
    assert_eq!(rooms[0].text(), "Kitchen at 10.0.0.2");
    assert_eq!(rooms[1].text(), "Hall, level 7");
    assert_eq!(
        deck.plugin_choices("bridge", WAIT),
        Ok(vec![Choice::new("b1", "Bridge 1")])
    );
    assert!(deck.plugin_choices("other", WAIT).is_err());
    deck.finish().unwrap();
}

#[test]
fn choice_answers_are_bounded_and_survive_a_panicking_handler() {
    let mut deck = Harness::start(plugin());
    deck.place("k1", Lamp::ID, json!({}));
    assert_eq!(deck.choices("k1", "many", WAIT).unwrap().len(), MAX_CHOICES);
    let wordy = deck.choices("k1", "wordy", WAIT).unwrap_err();
    assert_eq!(wordy.chars().count(), MAX_ERROR_CHARS);
    assert!(
        deck.choices("k1", "broken", WAIT)
            .is_err_and(|e| e.contains("failed"))
    );
    // The answering thread lives on after the panic.
    deck.set_globals(json!({"address": "a"}));
    assert!(deck.choices("k1", "room", WAIT).is_ok());
    deck.finish().unwrap();
}

#[test]
fn a_slow_choice_does_not_hold_up_key_presses() {
    let mut deck = Harness::start(plugin());
    deck.appear("k1", Lamp::ID, json!({"room": "hall"}));
    deck.send(&deckly_sdk::protocol::HostMessage::GetChoices {
        request: 99,
        field: "slow".into(),
        key: Some(deckly_sdk::protocol::KeyInfo {
            context: "k1".into(),
            action: Lamp::ID.into(),
            device: String::new(),
            column: 0,
            row: 0,
            state: None,
            settings: json!({}),
            in_multi_action: false,
        }),
    });
    deck.key_down("k1");
    // The title comes before the slow answer.
    let first = deck
        .wait_for(WAIT, |m| match &m.message {
            PluginMessage::SetTitle { title, .. } if title.as_deref() == Some("hall") => {
                Some("title")
            }
            PluginMessage::Choices { .. } => Some("choices"),
            _ => None,
        })
        .unwrap();
    assert_eq!(first, "title");
    deck.finish().unwrap();
}

#[test]
fn buttons_reach_shown_keys_editor_keys_and_the_plugin() {
    let mut deck = Harness::start(plugin());
    deck.appear("shown", Lamp::ID, json!({}));
    deck.button("shown", "blink");
    assert!(
        deck.wait_for(WAIT, |m| {
            matches!(&m.message, PluginMessage::ShowOk { context } if context == "shown")
                .then_some(())
        })
        .is_some()
    );
    // A key in the editor only: its settings are updated all the same.
    deck.place("editor", Lamp::ID, json!({"room": "hall", "presses": 4}));
    deck.button("editor", "count");
    assert_eq!(
        deck.settings("editor", WAIT),
        Some(json!({"room": "hall", "level": 0, "presses": 5}))
    );
    deck.plugin_button("pair");
    assert_eq!(
        deck.stored_globals(WAIT),
        Some(json!({"address": "", "token": "paired"}))
    );
    assert_eq!(deck.globals()["token"], "paired");
    deck.finish().unwrap();
}

#[test]
fn keys_hear_of_new_global_settings() {
    let mut deck = Harness::start(plugin());
    deck.appear("k1", Lamp::ID, json!({}));
    let before = GLOBALS_SEEN.load(Ordering::Relaxed);
    deck.set_globals(json!({"address": "bridge.local"}));
    assert_eq!(deck.title("k1", WAIT).as_deref(), Some("bridge.local"));
    assert!(GLOBALS_SEEN.load(Ordering::Relaxed) > before);
    deck.finish().unwrap();
}

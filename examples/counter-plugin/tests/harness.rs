//! The counter plugin tested with the SDK's harness, without Deckly.

use counter_plugin::{Counter, CounterSettings, plugin};
use deckly_sdk::Action;
use deckly_sdk::protocol::{FieldKind, Validate};
use deckly_sdk::testing::Harness;
use serde_json::{Value, json};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

#[allow(clippy::unwrap_used)] // A test helper.
fn counter(settings: &Value) -> CounterSettings {
    serde_json::from_value(settings.clone()).unwrap()
}

#[test]
fn presses_count_by_the_step_and_are_saved() {
    let mut deck = Harness::start(plugin());
    deck.appear("k1", Counter::ID, json!({"step": 5}));
    let first = deck.face("k1", WAIT).unwrap();
    assert_eq!(first.value.as_deref(), Some("0"));
    assert_eq!(first.detail.as_deref(), Some("step 5"));
    deck.press("k1");
    let saved = counter(&deck.settings("k1", WAIT).unwrap());
    assert_eq!((saved.step, saved.count), (5, 5));
    let face = deck.face("k1", WAIT).unwrap();
    assert_eq!(face.value.as_deref(), Some("5"));
    deck.change_settings("k1", json!({"step": 1, "count": 100}));
    let face = deck.face("k1", WAIT).unwrap();
    assert_eq!(face.value.as_deref(), Some("100"));
    deck.finish().unwrap();
}

#[test]
fn the_timer_redraws_after_a_press() {
    let mut deck = Harness::start(plugin());
    deck.appear("k1", Counter::ID, json!({}));
    deck.face("k1", WAIT).unwrap();
    deck.press("k1");
    let pressed = deck.face("k1", WAIT).unwrap();
    assert_eq!(pressed.detail.as_deref(), Some("0s ago"));
    // Only the timer can change the face now.
    let later = deck.face("k1", Duration::from_secs(5)).unwrap();
    assert_ne!(later, pressed);
    deck.finish().unwrap();
}

#[test]
fn bad_settings_fall_back_to_defaults() {
    let mut deck = Harness::start(plugin());
    deck.appear("k1", Counter::ID, json!({"step": "lots"}));
    let face = deck.face("k1", WAIT).unwrap();
    assert_eq!(face.detail.as_deref(), Some("step 1"));
    deck.finish().unwrap();
}

#[test]
fn the_forms_describe_every_setting_with_its_default() {
    let deck = Harness::start(plugin());
    let form = deck.form(Counter::ID).unwrap();
    assert!(form.validate().is_ok());
    let defaults = serde_json::to_value(CounterSettings::default()).unwrap();
    for field in form.fields.iter().filter(|f| f.kind.has_value()) {
        // Every field names a setting, and its default is the type's.
        assert_eq!(
            field.default.as_ref(),
            defaults.get(&field.key),
            "field {}",
            field.key
        );
    }
    // The defaults pass the form's own checks.
    assert!(
        form.problems(&json!({})).is_empty(),
        "{:?}",
        form.problems(&json!({}))
    );
    let fade = form.fields.iter().position(|f| f.key == "fade").unwrap();
    assert!(form.is_visible(fade, &json!({})));
    assert!(!form.is_visible(fade, &json!({"show_age": false})));
    assert_eq!(
        form.problems(&json!({"label": "", "step": 0})),
        vec![
            (
                "step".to_owned(),
                "Enter a number from 1 to 100.".to_owned()
            ),
            ("label".to_owned(), "This field is required.".to_owned()),
        ]
    );
    let global = deck.plugin_form().unwrap();
    assert!(global.validate().is_ok());
    let kinds: Vec<FieldKind> = global.fields.iter().map(|f| f.kind).collect();
    for kind in [
        FieldKind::Secret,
        FieldKind::File,
        FieldKind::Folder,
        FieldKind::Hotkey,
        FieldKind::Button,
        FieldKind::Note,
    ] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
    deck.finish().unwrap();
}

#[test]
fn start_values_come_from_the_plugin_and_reset_uses_them() {
    let mut deck = Harness::start(plugin());
    // In the editor only: no device shows the key.
    deck.place("k1", Counter::ID, json!({"count": 7, "start": "100"}));
    let starts = deck.choices("k1", "start", WAIT).unwrap();
    let values: Vec<&str> = starts.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, ["0", "10", "100"]);
    deck.set_globals(json!({"default_start": 42}));
    let starts = deck.choices("k1", "start", WAIT).unwrap();
    assert_eq!(starts.last().map(|c| c.text()), Some("42 (plugin setting)"));
    assert!(deck.choices("k1", "label", WAIT).is_err());
    deck.button("k1", "reset");
    let saved = counter(&deck.settings("k1", WAIT).unwrap());
    assert_eq!(saved.count, 100);
    deck.finish().unwrap();
}

#[test]
fn counting_down_stops_at_zero() {
    let mut deck = Harness::start(plugin());
    deck.appear(
        "k1",
        Counter::ID,
        json!({"count": 3, "step": 2, "direction": "down"}),
    );
    deck.press("k1");
    assert_eq!(counter(&deck.settings("k1", WAIT).unwrap()).count, 1);
    deck.press("k1");
    assert_eq!(counter(&deck.settings("k1", WAIT).unwrap()).count, 0);
    deck.finish().unwrap();
}

#[test]
fn forgetting_the_token_keeps_the_other_global_settings() {
    let mut deck = Harness::start(plugin());
    deck.set_globals(json!({"token": "abc", "default_start": 5, "sound": "/tmp/a.wav"}));
    deck.plugin_button("forget");
    assert_eq!(
        deck.stored_globals(WAIT),
        Some(json!({"token": "", "default_start": 5, "sound": "/tmp/a.wav"}))
    );
    deck.finish().unwrap();
}

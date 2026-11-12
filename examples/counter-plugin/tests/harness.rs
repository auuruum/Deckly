//! The counter plugin tested with the SDK's harness, without Deckly.

use counter_plugin::{Counter, plugin};
use deckly_sdk::Action;
use deckly_sdk::testing::Harness;
use serde_json::json;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn presses_count_by_the_step_and_are_saved() {
    let mut deck = Harness::start(plugin());
    deck.appear("k1", Counter::ID, json!({"step": 5}));
    let first = deck.face("k1", WAIT).unwrap();
    assert_eq!(first.value.as_deref(), Some("0"));
    assert_eq!(first.detail.as_deref(), Some("step 5"));
    deck.press("k1");
    assert_eq!(
        deck.settings("k1", WAIT),
        Some(json!({"step": 5, "count": 5}))
    );
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

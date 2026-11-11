//! An example Deckly plugin: a key that counts its presses.
//!
//! It shows the parts of the SDK most plugins need: a typed setting (the
//! step), state kept in the settings so it survives restarts (the count), a
//! face drawn by Deckly, and a timer (the time since the last press).

use deckly_sdk::prelude::*;
use std::time::{Duration, Instant};

pub const PLUGIN_ID: &str = "com.deckly.example.counter";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CounterSettings {
    /// Added on each press.
    pub step: u32,
    pub count: u64,
}

impl Default for CounterSettings {
    fn default() -> Self {
        Self { step: 1, count: 0 }
    }
}

#[derive(Default)]
pub struct Counter {
    last_press: Option<Instant>,
}

impl Counter {
    fn draw(&self, key: &Key<Self>) {
        let settings = key.settings();
        let detail = match self.last_press.map(|t| t.elapsed().as_secs()) {
            None => format!("step {}", settings.step.max(1)),
            Some(s) if s < 60 => format!("{s}s ago"),
            Some(s) => format!("{}m ago", s / 60),
        };
        // The bar empties over the ten seconds after a press.
        let fresh = self
            .last_press
            .map_or(0.0, |t| 1.0 - t.elapsed().as_secs_f32() / 10.0);
        key.set_face(
            Face::new()
                .label("Count")
                .value(settings.count.to_string())
                .detail(detail)
                .progress(fresh)
                .accent(Color::rgb(0x4c, 0xc2, 0xff))
                .icon("assets/counter.svg"),
        );
    }
}

impl Action for Counter {
    const ID: &'static str = "com.deckly.example.counter.count";
    const NAME: &'static str = "Count presses";
    const TOOLTIP: &'static str = "Counts how often the key is pressed";
    const ICON: &'static str = "assets/counter.svg";
    type Settings = CounterSettings;

    fn key_appear(&mut self, key: &Key<Self>) {
        self.draw(key);
        key.every(Duration::from_secs(1));
    }

    fn key_down(&mut self, key: &Key<Self>) {
        key.update_settings(|s| s.count = s.count.saturating_add(u64::from(s.step.max(1))));
        self.last_press = Some(Instant::now());
        self.draw(key);
    }

    fn settings_changed(&mut self, key: &Key<Self>) {
        self.draw(key);
    }

    fn tick(&mut self, key: &Key<Self>) {
        self.draw(key);
    }
}

pub fn plugin() -> Plugin {
    Plugin::new(PLUGIN_ID, "Counter example")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Deckly contributors")
        .license("MIT")
        .description("Counts key presses. An example of the Deckly plugin SDK.")
        .icon("assets/counter.svg")
        .action::<Counter>()
}

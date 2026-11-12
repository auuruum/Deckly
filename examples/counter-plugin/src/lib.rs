//! An example Deckly plugin: a key that counts its presses.
//!
//! It shows the parts of the SDK most plugins need: typed settings with a
//! settings form (the step, the direction, the look), state kept in the
//! settings so it survives restarts (the count), a face drawn by Deckly, a
//! timer (the time since the last press), a choice whose options the plugin
//! lists when the form opens, a button, and plugin-wide settings with a
//! form of their own and a secret.

use deckly_sdk::prelude::*;
use std::time::{Duration, Instant};

pub const PLUGIN_ID: &str = "com.deckly.example.counter";

/// Start values the "Start at" choice always offers.
const STARTS: [u64; 3] = [0, 10, 100];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CounterSettings {
    /// Added (or taken away) on each press.
    pub step: u32,
    pub count: u64,
    /// `up` or `down`.
    pub direction: String,
    /// Where "Reset" puts the count, as the choice's value.
    pub start: String,
    /// The face's label.
    pub label: String,
    /// The progress bar's colour, `#rrggbb`.
    pub accent: String,
    pub show_age: bool,
    /// Seconds the bar takes to empty after a press.
    pub fade: u32,
}

impl Default for CounterSettings {
    fn default() -> Self {
        Self {
            step: 1,
            count: 0,
            direction: "up".into(),
            start: "0".into(),
            label: "Count".into(),
            accent: "#4cc2ff".into(),
            show_age: true,
            fade: 10,
        }
    }
}

impl CounterSettings {
    fn start_value(&self) -> u64 {
        self.start.parse().unwrap_or(0)
    }
}

/// Settings shared by every counter key, edited in the plugin's own form.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CounterGlobals {
    /// Offered as one more "Start at" value.
    pub default_start: u64,
    /// Not used by the counter: it shows how secrets are kept.
    pub token: String,
    /// The form's other fields, which the counter does not read. Kept, so
    /// storing the settings from here does not drop them.
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Default)]
pub struct Counter {
    last_press: Option<Instant>,
}

impl Counter {
    fn draw(&self, key: &Key<Self>) {
        let settings = key.settings();
        let step = settings.step.max(1);
        let detail = match self.last_press.map(|t| t.elapsed().as_secs()) {
            _ if !settings.show_age => format!("step {step}"),
            None => format!("step {step}"),
            Some(s) if s < 60 => format!("{s}s ago"),
            Some(s) => format!("{}m ago", s / 60),
        };
        // The bar empties over the seconds after a press.
        let fade = settings.fade.max(1) as f32;
        let fresh = self
            .last_press
            .filter(|_| settings.show_age)
            .map_or(0.0, |t| 1.0 - t.elapsed().as_secs_f32() / fade);
        let accent =
            Color::try_from(settings.accent.clone()).unwrap_or(Color::rgb(0x4c, 0xc2, 0xff));
        key.set_face(
            Face::new()
                .label(&settings.label)
                .value(settings.count.to_string())
                .detail(detail)
                .progress(fresh)
                .accent(accent)
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

    fn form(form: &mut Form) {
        form.section("Counting");
        form.number("step", "Step")
            .range(1..=100)
            .default(1)
            .help("Added on each press.");
        form.choice("direction", "Direction")
            .option("up", "Count up")
            .option("down", "Count down")
            .default("up");
        form.choice("start", "Start at")
            .dynamic()
            .default("0")
            .help("Where Reset puts the count.");
        form.button("reset", "Reset");
        form.section("Look");
        form.text("label", "Label")
            .length(1..=12)
            .default("Count")
            .required();
        form.color("accent", "Bar colour").default("#4cc2ff");
        form.checkbox("show_age", "Show the time since the last press")
            .default(true);
        form.slider("fade", "Bar empties after", 1..=60)
            .default(10)
            .help("Seconds.")
            .visible_when("show_age", true);
    }

    fn choices(
        field: &str,
        _settings: &CounterSettings,
        globals: &Globals,
    ) -> Result<Vec<Choice>, String> {
        if field != "start" {
            return Err(format!("no options for {field}"));
        }
        let shared = globals.get::<CounterGlobals>().default_start;
        let mut options: Vec<Choice> = STARTS
            .iter()
            .map(|n| Choice::new(n.to_string(), n.to_string()))
            .collect();
        if !STARTS.contains(&shared) {
            options.push(Choice::new(
                shared.to_string(),
                format!("{shared} (plugin setting)"),
            ));
        }
        Ok(options)
    }

    fn key_appear(&mut self, key: &Key<Self>) {
        self.draw(key);
        key.every(Duration::from_secs(1));
    }

    fn key_down(&mut self, key: &Key<Self>) {
        key.update_settings(|s| {
            let step = u64::from(s.step.max(1));
            s.count = if s.direction == "down" {
                s.count.saturating_sub(step)
            } else {
                s.count.saturating_add(step)
            };
        });
        self.last_press = Some(Instant::now());
        self.draw(key);
    }

    fn button(&mut self, key: &Key<Self>, button: &str) {
        if button == "reset" {
            key.update_settings(|s| s.count = s.start_value());
            self.last_press = None;
            self.draw(key);
        }
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
        .form(|form| {
            form.note("Settings shared by every counter key.");
            form.number("default_start", "Extra start value")
                .range(0..=1_000_000u32)
                .default(0)
                .help("Offered under Start at on every key.");
            form.secret("token", "Token")
                .placeholder("Not set")
                .help("The counter does not use it; it shows how Deckly keeps secrets.");
            form.button("forget", "Forget the token");
            form.section("Fields the counter does not use");
            form.file("sound", "Sound").extensions(&["wav", "ogg"]);
            form.folder("folder", "Folder");
            form.hotkey("shortcut", "Hotkey");
        })
        .on_button(|button, globals| {
            if button == "forget" {
                globals.update::<CounterGlobals>(|g| g.token.clear());
            }
        })
}

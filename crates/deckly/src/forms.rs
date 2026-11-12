//! Settings forms of Deckly plugins, drawn by Deckly in egui: one per
//! action, in the key editor where a plugin's settings belong, and one per
//! plugin, for its global settings.
//!
//! The form edits a settings object in place. Input that does not pass the
//! field's checks stays in the field with a plain message under it and is
//! not saved. Secrets are stored in the secret store and the settings keep a
//! reference (see `crate::secrets`). Dynamic choices are asked from the
//! plugin when they are first shown; a plugin that is slow or gone shows a
//! loading line, then a failure with a retry, and never holds the form up.

use crate::actions;
use crate::secrets;
use crate::theme::{TEXT_DIM, TEXT_FAINT, WARNING};
use deckly_plugin_host::proto::{Choice, Color, Field, FieldKind, Form};
use deckly_plugin_host::{Host, Instance};
use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a plugin has to list a choice's options.
pub const CHOICES_TIMEOUT: Duration = Duration::from_secs(5);
/// Answers kept for the editor before old ones are dropped.
const MAX_ANSWERS: usize = 64;
/// Width of text fields, as in the built-in actions' settings.
const FIELD_WIDTH: f32 = 320.0;
const CHOICE_WIDTH: f32 = 220.0;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Plugins' answers to choice requests, by request number; the plugin
/// thread fills it and the editor takes from it.
#[derive(Default)]
pub struct Answers {
    map: HashMap<u64, Result<Vec<Choice>, String>>,
}

impl Answers {
    pub fn answer(&mut self, request: u64, answer: Result<Vec<Choice>, String>) {
        // Answers nobody waits for any more must not pile up.
        if self.map.len() >= MAX_ANSWERS {
            self.map.clear();
        }
        self.map.insert(request, answer);
    }

    pub fn take(&mut self, request: u64) -> Option<Result<Vec<Choice>, String>> {
        self.map.remove(&request)
    }
}

/// Where a dynamic choice's options stand.
#[derive(Clone, Debug, PartialEq)]
pub enum Options {
    Loading { request: u64, since: Instant },
    Ready(Vec<Choice>),
    Failed(String),
}

/// What a form shows besides the settings: input not saved yet, messages,
/// options, the hotkey being recorded. Reset when the form shows something
/// else.
#[derive(Default)]
pub struct FormState {
    owner: String,
    edits: HashMap<String, String>,
    errors: HashMap<String, String>,
    secrets: HashMap<String, String>,
    options: HashMap<String, Options>,
    recording: Option<String>,
}

/// What the form needs from around it.
pub struct Place<'a> {
    pub plugin: &'a str,
    pub host: Option<&'a Host>,
    /// The key the form belongs to; none for the plugin-wide form.
    pub instance: Option<&'a Instance>,
    pub state: &'a crate::runtime::Shared,
}

/// What happened in one frame.
#[derive(Default)]
pub struct Outcome {
    /// A secret changed: the settings look the same, but the plugin must
    /// hear of it.
    pub secret_changed: bool,
}

impl FormState {
    /// Starts over when the form now shows `owner` (a key's context, or a
    /// plugin). True when it did.
    pub fn follow(&mut self, owner: &str) -> bool {
        if self.owner == owner {
            return false;
        }
        *self = Self {
            owner: owner.to_owned(),
            ..Self::default()
        };
        true
    }

    /// Whether a hotkey field is listening for keys.
    pub fn recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Asks the plugin for a field's options.
    fn ask(&mut self, key: &str, place: &Place<'_>) {
        let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let sent = place
            .host
            .is_some_and(|h| h.ask_choices(place.plugin, request, key, place.instance));
        let options = if sent {
            Options::Loading {
                request,
                since: Instant::now(),
            }
        } else {
            Options::Failed("The plugin is not running.".into())
        };
        self.options.insert(key.to_owned(), options);
    }

    /// Takes the plugin's answer, or gives up after the timeout.
    fn poll(&mut self, key: &str, answers: &mut Answers, now: Instant) {
        let Some(Options::Loading { request, since }) = self.options.get(key).cloned() else {
            return;
        };
        let next = match answers.take(request) {
            Some(Ok(options)) => Options::Ready(options),
            Some(Err(why)) => Options::Failed(why),
            None if now.duration_since(since) >= CHOICES_TIMEOUT => {
                Options::Failed("The plugin did not answer in time.".into())
            }
            None => return,
        };
        self.options.insert(key.to_owned(), next);
    }

    /// Draws the form's rows into a two-column grid (label, field) and
    /// edits `settings`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        form: &Form,
        settings: &mut Value,
        place: &Place<'_>,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        if !settings.is_object() {
            *settings = Value::Object(Map::new());
        }
        for (index, field) in form.fields.iter().enumerate() {
            if !form.is_visible(index, settings) {
                continue;
            }
            match field.kind {
                FieldKind::Unknown => continue,
                FieldKind::Section => {
                    ui.label(RichText::new(&field.label).strong());
                    ui.label("");
                    ui.end_row();
                    continue;
                }
                FieldKind::Note => {
                    ui.label("");
                    ui.add(egui::Label::new(RichText::new(&field.label).color(TEXT_DIM)).wrap());
                    ui.end_row();
                    continue;
                }
                FieldKind::Checkbox | FieldKind::Button => {
                    ui.label("");
                }
                _ => {
                    ui.label(RichText::new(&field.label).color(TEXT_DIM));
                }
            }
            ui.vertical(|ui| {
                self.field(ui, form, field, settings, place, &mut outcome);
                self.below(ui, form, field, settings, place);
            });
            ui.end_row();
        }
        if self
            .options
            .values()
            .any(|o| matches!(o, Options::Loading { .. }))
        {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        outcome
    }

    /// The help line, the message of input that was not saved, and the
    /// note about where secrets are kept.
    fn below(&self, ui: &mut Ui, form: &Form, field: &Field, settings: &Value, place: &Place<'_>) {
        let small = |text: &str, color| RichText::new(text.to_owned()).size(11.0).color(color);
        if !field.help.is_empty() {
            ui.add(egui::Label::new(small(&field.help, TEXT_FAINT)).wrap());
        }
        let shown_value = match field.kind {
            // A secret's stored value is checked through its buffer.
            FieldKind::Secret => self
                .secrets
                .get(&field.key)
                .cloned()
                .map(Value::String)
                .or_else(|| {
                    secrets::typed(secrets::vault(), place.plugin, settings, &field.key)
                        .ok()
                        .map(Value::String)
                }),
            _ => form.value(settings, &field.key).cloned(),
        };
        let problem = self
            .errors
            .get(&field.key)
            .cloned()
            .or_else(|| field.check(shown_value.as_ref()).err());
        if let Some(problem) = problem {
            ui.label(small(&problem, WARNING));
        }
        if field.kind == FieldKind::Secret
            && let Some(why) = secrets::vault().fallback()
        {
            ui.add(
                egui::Label::new(small(
                    "Kept in a file on this computer, without the system's protection.",
                    TEXT_FAINT,
                ))
                .wrap(),
            )
            .on_hover_text(format!("No system secret store is used: {why}"));
        }
    }

    fn field(
        &mut self,
        ui: &mut Ui,
        form: &Form,
        field: &Field,
        settings: &mut Value,
        place: &Place<'_>,
        outcome: &mut Outcome,
    ) {
        let key = field.key.as_str();
        let current = form.value(settings, key).cloned();
        match field.kind {
            FieldKind::Text => {
                let mut text = self.edits.get(key).cloned().unwrap_or_else(|| {
                    current
                        .as_ref()
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                });
                let edit = if field.multiline {
                    egui::TextEdit::multiline(&mut text).desired_rows(3)
                } else {
                    egui::TextEdit::singleline(&mut text)
                };
                let edit = edit
                    .hint_text(RichText::new(&field.placeholder).color(TEXT_FAINT))
                    .desired_width(FIELD_WIDTH);
                if ui.add(edit).changed() {
                    self.typed(field, settings, text);
                }
            }
            FieldKind::File | FieldKind::Folder => {
                ui.horizontal(|ui| {
                    let mut text = self.edits.get(key).cloned().unwrap_or_else(|| {
                        current
                            .as_ref()
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    });
                    let edit = egui::TextEdit::singleline(&mut text)
                        .hint_text(RichText::new(&field.placeholder).color(TEXT_FAINT))
                        .desired_width(FIELD_WIDTH - 90.0);
                    let mut changed = ui.add(edit).changed();
                    if ui.button("Choose…").clicked() {
                        let dialog = rfd::FileDialog::new().set_title(&field.label);
                        let picked = if field.kind == FieldKind::Folder {
                            dialog.pick_folder()
                        } else if field.extensions.is_empty() {
                            dialog.pick_file()
                        } else {
                            dialog
                                .add_filter(&field.label, &field.extensions)
                                .pick_file()
                        };
                        if let Some(p) = picked {
                            text = p.to_string_lossy().into_owned();
                            changed = true;
                        }
                    }
                    if changed {
                        self.typed(field, settings, text);
                    }
                });
            }
            FieldKind::Secret => self.secret(ui, field, settings, place, outcome),
            FieldKind::Number => {
                let mut n = current.as_ref().and_then(Value::as_f64).unwrap_or(0.0);
                let mut drag =
                    egui::DragValue::new(&mut n).speed(field.step.unwrap_or(1.0).max(0.01) / 4.0);
                drag = match (field.min, field.max) {
                    (Some(a), Some(b)) => drag.range(a..=b),
                    (Some(a), None) => drag.range(a..=f64::MAX),
                    (None, Some(b)) => drag.range(f64::MIN..=b),
                    (None, None) => drag,
                };
                if field.integer {
                    drag = drag.max_decimals(0);
                }
                if ui.add(drag).changed() {
                    set(settings, key, number(field, n));
                }
            }
            FieldKind::Slider => {
                let (min, max) = (field.min.unwrap_or(0.0), field.max.unwrap_or(100.0));
                let mut n = current.as_ref().and_then(Value::as_f64).unwrap_or(min);
                let mut slider = egui::Slider::new(&mut n, min..=max);
                if let Some(step) = field.step {
                    slider = slider.step_by(step);
                }
                if field.integer {
                    slider = slider.integer();
                }
                if ui.add(slider).changed() {
                    set(settings, key, number(field, n));
                }
            }
            FieldKind::Checkbox => {
                let mut on = current.as_ref().and_then(Value::as_bool).unwrap_or(false);
                if ui.checkbox(&mut on, &field.label).changed() {
                    set(settings, key, json!(on));
                }
            }
            FieldKind::Choice => self.choice(ui, field, settings, current.as_ref(), place),
            FieldKind::Color => {
                let mut rgb = current
                    .as_ref()
                    .and_then(Value::as_str)
                    .and_then(|s| Color::try_from(s.to_owned()).ok())
                    .map_or([255, 255, 255], |c| c.0);
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    set(settings, key, json!(Color(rgb).to_string()));
                }
            }
            FieldKind::Hotkey => {
                ui.horizontal(|ui| {
                    let value = current.clone().unwrap_or_else(|| json!({}));
                    let hotkey = actions::hotkey_setting(&value);
                    let recording = self.recording.as_deref() == Some(key);
                    let text = if recording {
                        "Press a key combination… (Esc cancels)".to_owned()
                    } else if hotkey.key.is_empty() {
                        "Not set".to_owned()
                    } else {
                        actions::hotkey_label(&hotkey)
                    };
                    ui.add(egui::Label::new(RichText::new(text).monospace()));
                    let button = if recording { "Cancel" } else { "Record" };
                    if ui.button(button).clicked() {
                        self.recording = (!recording).then(|| key.to_owned());
                    }
                    if recording && let Some(got) = crate::app::capture_hotkey(ui.ctx()) {
                        if let Some(hk) = got {
                            let mut value = json!({});
                            actions::set_hotkey(&mut value, &hk);
                            set(settings, key, value);
                        }
                        self.recording = None;
                    }
                });
            }
            FieldKind::Button => {
                if ui.button(&field.label).clicked() {
                    let pressed = place
                        .host
                        .is_some_and(|h| h.press_button(place.plugin, key, place.instance));
                    if pressed {
                        self.errors.remove(key);
                    } else {
                        self.errors
                            .insert(key.to_owned(), "The plugin is not running.".into());
                    }
                }
            }
            FieldKind::Section | FieldKind::Note | FieldKind::Unknown => {}
        }
    }

    /// Text typed into a field: saved when it passes, kept with a message
    /// when it does not.
    fn typed(&mut self, field: &Field, settings: &mut Value, text: String) {
        let value = Value::String(text.clone());
        match field.check(Some(&value)) {
            Ok(()) => {
                set(settings, &field.key, value);
                self.edits.remove(&field.key);
                self.errors.remove(&field.key);
            }
            Err(why) => {
                self.edits.insert(field.key.clone(), text);
                self.errors.insert(field.key.clone(), why);
            }
        }
    }

    /// A secret is stored when the field loses focus or Enter is pressed,
    /// not on every key, so the secret store is not asked each keystroke.
    fn secret(
        &mut self,
        ui: &mut Ui,
        field: &Field,
        settings: &mut Value,
        place: &Place<'_>,
        outcome: &mut Outcome,
    ) {
        let key = field.key.as_str();
        let stored = secrets::typed(secrets::vault(), place.plugin, settings, key);
        let mut text = match (self.secrets.get(key), &stored) {
            (Some(t), _) => t.clone(),
            (None, Ok(t)) => t.clone(),
            (None, Err(_)) => String::new(),
        };
        let resp = ui.add(
            egui::TextEdit::singleline(&mut text)
                .password(true)
                .hint_text(RichText::new(&field.placeholder).color(TEXT_FAINT))
                .desired_width(FIELD_WIDTH),
        );
        if resp.changed() {
            self.secrets.insert(key.to_owned(), text.clone());
            self.errors.remove(key);
        }
        if let Err(why) = &stored
            && !self.errors.contains_key(key)
        {
            self.errors
                .insert(key.to_owned(), format!("The secret cannot be read: {why}"));
        }
        if !resp.lost_focus() {
            return;
        }
        let Some(text) = self.secrets.get(key).cloned() else {
            return;
        };
        if let Err(why) = field.check(Some(&Value::String(text.clone()))) {
            self.errors.insert(key.to_owned(), why);
            return;
        }
        let id = |k: &str| {
            if place.instance.is_some() {
                secrets::new_id()
            } else {
                secrets::global_id(k)
            }
        };
        match secrets::store_typed(secrets::vault(), place.plugin, settings, key, &text, &id) {
            Ok(()) => {
                self.secrets.remove(key);
                self.errors.remove(key);
                outcome.secret_changed = true;
            }
            Err(why) => {
                self.errors
                    .insert(key.to_owned(), format!("The secret was not stored: {why}"));
            }
        }
    }

    fn choice(
        &mut self,
        ui: &mut Ui,
        field: &Field,
        settings: &mut Value,
        current: Option<&Value>,
        place: &Place<'_>,
    ) {
        let key = field.key.as_str();
        let selected = current
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let options = if field.dynamic {
            if !self.options.contains_key(key) {
                self.ask(key, place);
            }
            {
                let mut s = crate::runtime::lock(place.state);
                self.poll(key, &mut s.choices, Instant::now());
            }
            match self.options.get(key).cloned() {
                Some(Options::Ready(options)) => options,
                Some(Options::Loading { .. }) | None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Loading options…").color(TEXT_DIM));
                    });
                    return;
                }
                Some(Options::Failed(why)) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::WARNING).color(WARNING));
                        ui.add(egui::Label::new(RichText::new(&why).color(WARNING)).wrap());
                        if ui
                            .button(format!("{}  Retry", icon::ARROW_CLOCKWISE))
                            .clicked()
                        {
                            self.ask(key, place);
                        }
                    });
                    return;
                }
            }
        } else {
            field.options.clone()
        };
        let text = options
            .iter()
            .find(|o| o.value == selected)
            .map(|o| o.text().to_owned())
            .unwrap_or_else(|| {
                if selected.is_empty() {
                    "Choose…".to_owned()
                } else {
                    selected.clone()
                }
            });
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("plugin-form-choice", &self.owner, key))
                .selected_text(text)
                .width(CHOICE_WIDTH)
                .show_ui(ui, |ui| {
                    for o in &options {
                        if ui.selectable_label(o.value == selected, o.text()).clicked() {
                            set(settings, key, json!(o.value));
                            self.errors.remove(key);
                        }
                    }
                });
            if field.dynamic
                && ui
                    .button(icon::ARROW_CLOCKWISE)
                    .on_hover_text("Ask the plugin for the options again")
                    .clicked()
            {
                self.ask(key, place);
            }
        });
    }
}

/// A number as the field keeps it: whole numbers without a fraction.
fn number(field: &Field, n: f64) -> Value {
    if field.integer && n.is_finite() && n.abs() < 9e15 {
        json!(n.round() as i64)
    } else {
        json!(n)
    }
}

fn set(settings: &mut Value, key: &str, value: Value) {
    if let Some(map) = settings.as_object_mut() {
        map.insert(key.to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        serde_json::from_value(json!({"fields": [
            {"kind": "text", "key": "name", "label": "Name", "max_length": 3, "required": true},
            {"kind": "number", "key": "step", "label": "Step", "integer": true}
        ]}))
        .unwrap()
    }

    #[test]
    fn typed_text_is_saved_only_when_it_passes() {
        let form = form();
        let field = form.field("name").unwrap();
        let mut state = FormState::default();
        let mut settings = json!({"name": "ok"});
        state.typed(field, &mut settings, "toolong".into());
        assert_eq!(settings, json!({"name": "ok"}));
        assert_eq!(state.edits["name"], "toolong");
        assert_eq!(state.errors["name"], "Use at most 3 characters.");
        state.typed(field, &mut settings, String::new());
        assert_eq!(settings["name"], "ok");
        assert_eq!(state.errors["name"], "This field is required.");
        state.typed(field, &mut settings, "abc".into());
        assert_eq!(settings["name"], "abc");
        assert!(state.edits.is_empty() && state.errors.is_empty());
    }

    #[test]
    fn numbers_keep_whole_numbers_whole() {
        let form = form();
        let step = form.field("step").unwrap();
        assert_eq!(number(step, 3.0), json!(3));
        assert_eq!(number(step, 2.6), json!(3));
        let name = form.field("name").unwrap();
        assert_eq!(number(name, 2.5), json!(2.5));
    }

    #[test]
    fn choices_load_answer_fail_and_time_out() {
        let mut state = FormState::default();
        let mut answers = Answers::default();
        let start = Instant::now();
        let loading = |request| Options::Loading {
            request,
            since: start,
        };
        state.options.insert("a".into(), loading(1));
        state.options.insert("b".into(), loading(2));
        state.options.insert("c".into(), loading(3));
        answers.answer(1, Ok(vec![Choice::new("x", "X")]));
        answers.answer(2, Err("OBS is not running".into()));
        // An answer for a request nobody waits for is ignored.
        answers.answer(99, Ok(Vec::new()));
        for key in ["a", "b", "c"] {
            state.poll(key, &mut answers, start + Duration::from_secs(1));
        }
        assert_eq!(
            state.options["a"],
            Options::Ready(vec![Choice::new("x", "X")])
        );
        assert_eq!(
            state.options["b"],
            Options::Failed("OBS is not running".into())
        );
        assert_eq!(state.options["c"], loading(3));
        state.poll("c", &mut answers, start + CHOICES_TIMEOUT);
        assert!(matches!(&state.options["c"], Options::Failed(why) if why.contains("in time")));
        // A late answer changes nothing.
        answers.answer(3, Ok(Vec::new()));
        state.poll("c", &mut answers, start + CHOICES_TIMEOUT * 2);
        assert!(matches!(state.options["c"], Options::Failed(_)));
    }

    #[test]
    fn asking_a_plugin_that_is_not_running_fails_at_once() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::runtime::State::new(
            Vec::new(),
            deckly_core::Settings::default(),
        )));
        let place = Place {
            plugin: "com.example.x",
            host: None,
            instance: None,
            state: &state,
        };
        let mut form = FormState::default();
        form.ask("scene", &place);
        assert!(
            matches!(&form.options["scene"], Options::Failed(why) if why.contains("not running"))
        );
    }

    #[test]
    fn answers_do_not_pile_up() {
        let mut answers = Answers::default();
        for i in 0..(MAX_ANSWERS as u64 * 3) {
            answers.answer(i, Ok(Vec::new()));
        }
        assert!(answers.map.len() <= MAX_ANSWERS);
    }

    #[test]
    fn following_another_owner_starts_over() {
        let mut state = FormState::default();
        assert!(state.follow("page.1"));
        state.errors.insert("a".into(), "x".into());
        state.recording = Some("k".into());
        assert!(!state.follow("page.1"));
        assert!(state.recording());
        assert!(state.follow("page.2"));
        assert!(state.errors.is_empty() && !state.recording());
    }
}

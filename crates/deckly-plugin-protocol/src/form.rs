//! Settings forms: what a plugin asks the user, described in its manifest
//! and drawn by Deckly. One form per action (stored with each key) and one
//! for the whole plugin (its global settings).
//!
//! A field sets one property of the settings object, named by its key.
//! Unknown field kinds read as [`FieldKind::Unknown`] and unknown properties
//! are ignored, so an older Deckly shows what it knows of a newer plugin's
//! form. Everything else is checked against the [`limits`](crate::limits).

use crate::limits::*;
use crate::message::{check_bytes, check_chars, check_count, json_len, truncate};
use crate::{Color, Validate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Form {
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// What a field is. The value each kind keeps in the settings:
///
/// | Kind | Value |
/// | --- | --- |
/// | `text`, `secret`, `choice`, `file`, `folder` | a string |
/// | `number`, `slider` | a number |
/// | `checkbox` | `true` or `false` |
/// | `color` | `"#rrggbb"` |
/// | `hotkey` | `{"key": "K", "ctrl": true, "shift": false, "alt": false, "meta": false}` |
/// | `button`, `section`, `note` | none |
///
/// A secret's value never reaches the profile: Deckly keeps it in the
/// system's secret store and hands it to the plugin as a plain string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    #[default]
    Text,
    Secret,
    Number,
    Slider,
    Checkbox,
    Choice,
    File,
    Folder,
    Color,
    Hotkey,
    /// Sends a `button` message to the plugin when pressed.
    Button,
    /// A heading over the fields after it.
    Section,
    /// A line of text, its label.
    Note,
    /// A kind this side does not know; not shown.
    #[serde(other)]
    Unknown,
}

impl FieldKind {
    /// Whether the field keeps a value in the settings.
    pub fn has_value(self) -> bool {
        !matches!(
            self,
            FieldKind::Button | FieldKind::Section | FieldKind::Note | FieldKind::Unknown
        )
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One field. Properties that do not apply to its kind are ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub kind: FieldKind,
    /// The settings property the field sets; the button id for a button.
    /// Empty for sections and notes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The label; the heading of a section; the text of a note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// A line under the field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub help: String,
    /// Shown in an empty text field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    /// The value shown while the settings have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// An empty value is refused.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// Number and slider bounds and step; a slider needs both bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Whole numbers only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub integer: bool,
    /// Text and secret length, in characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    /// A text field of several lines.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiline: bool,
    /// The options of a choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Choice>,
    /// The options of a choice come from the plugin when the form opens
    /// (`get_choices`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub dynamic: bool,
    /// File name extensions a file field offers, without the dot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Shown only while another field has a given value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<Condition>,
}

/// One option of a choice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Choice {
    /// What the settings keep.
    pub value: String,
    /// What the user sees; the value when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl Choice {
    /// An option; text is cut to the limits, so a choice built here always
    /// passes the host's checks.
    pub fn new(value: impl AsRef<str>, label: impl AsRef<str>) -> Self {
        Self {
            value: cut_bytes(value.as_ref(), MAX_CHOICE_VALUE_BYTES).to_owned(),
            label: truncate(label.as_ref(), MAX_LABEL_CHARS).to_owned(),
        }
    }

    /// The label, or the value when there is none.
    pub fn text(&self) -> &str {
        if self.label.is_empty() {
            &self.value
        } else {
            &self.label
        }
    }
}

/// The longest start of `s` within `max` bytes that ends on a character.
pub fn cut_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The field is shown while `field` holds `equals`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub field: String,
    pub equals: Value,
}

/// Field keys: letters, digits, `_`, `-` and `.`.
pub fn valid_field_key(key: &str) -> bool {
    (1..=MAX_FIELD_KEY_BYTES).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Whether a value counts as not filled in.
fn is_empty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        // A hotkey without a key.
        Some(Value::Object(o)) => o
            .get("key")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty),
        _ => false,
    }
}

/// A number in plain words: `5` rather than `5.0`.
fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

impl Field {
    /// Checks a value the user entered, with a plain message for the user
    /// when it does not fit. `None` is a value not filled in.
    pub fn check(&self, value: Option<&Value>) -> Result<(), String> {
        if !self.kind.has_value() {
            return Ok(());
        }
        if is_empty(value) {
            return if self.required {
                Err("This field is required.".into())
            } else {
                Ok(())
            };
        }
        let Some(value) = value else {
            return Ok(());
        };
        match self.kind {
            FieldKind::Text | FieldKind::Secret | FieldKind::File | FieldKind::Folder => {
                let text = value.as_str().ok_or("Enter text.")?;
                let n = text.chars().count();
                let max = self
                    .max_length
                    .map_or(MAX_TEXT_VALUE_CHARS, |m| m as usize)
                    .min(MAX_TEXT_VALUE_CHARS);
                if let Some(min) = self.min_length
                    && n < min as usize
                {
                    return Err(format!("Use at least {min} characters."));
                }
                if n > max {
                    return Err(format!("Use at most {max} characters."));
                }
                Ok(())
            }
            FieldKind::Number | FieldKind::Slider => {
                let n = value
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .ok_or("Enter a number.")?;
                if self.integer && n.fract() != 0.0 {
                    return Err("Enter a whole number.".into());
                }
                let low = self.min.is_some_and(|m| n < m);
                let high = self.max.is_some_and(|m| n > m);
                if low || high {
                    return Err(match (self.min, self.max) {
                        (Some(a), Some(b)) => {
                            format!(
                                "Enter a number from {} to {}.",
                                number_text(a),
                                number_text(b)
                            )
                        }
                        (Some(a), None) => format!("Enter {} or more.", number_text(a)),
                        (None, Some(b)) => format!("Enter {} or less.", number_text(b)),
                        (None, None) => "Enter a number.".into(),
                    });
                }
                Ok(())
            }
            FieldKind::Checkbox => value
                .as_bool()
                .map(drop)
                .ok_or("Expected on or off.".into()),
            FieldKind::Choice => {
                let v = value.as_str().ok_or("Choose an option.")?;
                if !self.dynamic && !self.options.iter().any(|o| o.value == v) {
                    return Err("Choose one of the options.".into());
                }
                Ok(())
            }
            FieldKind::Color => {
                let s = value.as_str().ok_or("Choose a colour.")?;
                Color::try_from(s.to_owned())
                    .map(drop)
                    .map_err(|_| "Choose a colour.".into())
            }
            FieldKind::Hotkey => {
                let o = value.as_object().ok_or("Record a key combination.")?;
                let key = o.get("key").and_then(Value::as_str).unwrap_or_default();
                let flags_ok = ["ctrl", "shift", "alt", "meta"]
                    .iter()
                    .all(|f| o.get(*f).is_none_or(Value::is_boolean));
                if key.len() > MAX_SHORT_TEXT_BYTES || !flags_ok {
                    return Err("Record a key combination.".into());
                }
                Ok(())
            }
            FieldKind::Button | FieldKind::Section | FieldKind::Note | FieldKind::Unknown => Ok(()),
        }
    }
}

impl Form {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn field(&self, key: &str) -> Option<&Field> {
        self.fields
            .iter()
            .find(|f| f.kind != FieldKind::Unknown && !f.key.is_empty() && f.key == key)
    }

    /// A field's value: what the settings hold, else its default.
    pub fn value<'a>(&'a self, settings: &'a Value, key: &str) -> Option<&'a Value> {
        settings
            .get(key)
            .filter(|v| !v.is_null())
            .or_else(|| self.field(key).and_then(|f| f.default.as_ref()))
    }

    /// Whether the field at `index` is shown with these settings. A field
    /// whose condition names a hidden field is hidden too; a chain longer
    /// than the form (a loop) is hidden.
    pub fn is_visible(&self, index: usize, settings: &Value) -> bool {
        let mut at = index;
        for _ in 0..=self.fields.len() {
            let Some(field) = self.fields.get(at) else {
                return false;
            };
            if field.kind == FieldKind::Unknown {
                return false;
            }
            let Some(cond) = &field.visible_when else {
                return true;
            };
            if self.value(settings, &cond.field) != Some(&cond.equals) {
                return false;
            }
            match self
                .fields
                .iter()
                .position(|f| f.kind.has_value() && f.key == cond.field)
            {
                Some(next) => at = next,
                // A property no field shows: its value alone decides.
                None => return true,
            }
        }
        false
    }

    /// The fields shown with these settings that hold a value the user
    /// must change, with the message for each.
    pub fn problems(&self, settings: &Value) -> Vec<(String, String)> {
        self.fields
            .iter()
            .enumerate()
            .filter(|(i, f)| f.kind.has_value() && self.is_visible(*i, settings))
            .filter_map(|(_, f)| {
                f.check(self.value(settings, &f.key))
                    .err()
                    .map(|e| (f.key.clone(), e))
            })
            .collect()
    }

    /// The keys of the secret fields.
    pub fn secrets(&self) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .filter(|f| f.kind == FieldKind::Secret)
            .map(|f| f.key.as_str())
    }
}

fn check_value_bytes(what: &str, v: &Value) -> Result<(), String> {
    if json_len(v) > MAX_DEFAULT_BYTES {
        return Err(format!("{what} is larger than {MAX_DEFAULT_BYTES} bytes"));
    }
    Ok(())
}

impl Validate for Field {
    fn validate(&self) -> Result<(), String> {
        let named = |what: &str| format!("{what} of field {:?}", truncate(&self.key, 32));
        let help_max = if self.kind == FieldKind::Note {
            MAX_HELP_CHARS
        } else {
            MAX_LABEL_CHARS
        };
        check_chars(&named("the label"), &self.label, help_max)?;
        check_chars(&named("the help line"), &self.help, MAX_HELP_CHARS)?;
        check_chars(
            &named("the placeholder"),
            &self.placeholder,
            MAX_LABEL_CHARS,
        )?;
        if let Some(c) = &self.visible_when {
            if !valid_field_key(&c.field) {
                return Err(named("the condition"));
            }
            check_value_bytes(&named("the condition value"), &c.equals)?;
        }
        if self.kind == FieldKind::Unknown {
            // Nothing else is known about it; its size counts in the form.
            return Ok(());
        }
        let needs_key = self.kind.has_value() || self.kind == FieldKind::Button;
        if needs_key && !valid_field_key(&self.key) {
            return Err(format!("bad field key {:?}", truncate(&self.key, 32)));
        }
        if !needs_key && !self.key.is_empty() {
            check_bytes("a field key", &self.key, MAX_FIELD_KEY_BYTES)?;
        }
        for (what, n) in [("min", self.min), ("max", self.max), ("step", self.step)] {
            if n.is_some_and(|n| !n.is_finite()) {
                return Err(format!("{} is not a number", named(&format!("the {what}"))));
            }
        }
        if let (Some(a), Some(b)) = (self.min, self.max)
            && a > b
        {
            return Err(format!("{} is above its max", named("the min")));
        }
        if self.step.is_some_and(|s| s <= 0.0) {
            return Err(format!("{} must be above 0", named("the step")));
        }
        if self.kind == FieldKind::Slider && (self.min.is_none() || self.max.is_none()) {
            return Err(format!("{} needs a min and a max", named("the slider")));
        }
        let limit = MAX_TEXT_VALUE_CHARS as u32;
        if self.min_length.is_some_and(|n| n > limit) || self.max_length.is_some_and(|n| n > limit)
        {
            return Err(format!("{} is longer than {limit}", named("a length")));
        }
        if let (Some(a), Some(b)) = (self.min_length, self.max_length)
            && a > b
        {
            return Err(format!("{} is above its max", named("the min length")));
        }
        check_count("choice options", self.options.len(), MAX_CHOICES)?;
        self.options.iter().try_for_each(Validate::validate)?;
        check_count("extensions", self.extensions.len(), MAX_EXTENSIONS)?;
        for e in &self.extensions {
            if e.is_empty()
                || e.len() > MAX_EXTENSION_BYTES
                || !e.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(format!("bad extension {:?}", truncate(e, 16)));
            }
        }
        if let Some(d) = &self.default {
            check_value_bytes(&named("the default"), d)?;
            if !d.is_null() {
                // A default must be a value the field could hold, required or not.
                let optional = Field {
                    required: false,
                    ..self.clone()
                };
                optional
                    .check(Some(d))
                    .map_err(|e| format!("{}: {e}", named("the default")))?;
            }
        }
        Ok(())
    }
}

impl Validate for Choice {
    fn validate(&self) -> Result<(), String> {
        check_bytes("a choice value", &self.value, MAX_CHOICE_VALUE_BYTES)?;
        check_chars("a choice label", &self.label, MAX_LABEL_CHARS)
    }
}

impl Validate for Form {
    fn validate(&self) -> Result<(), String> {
        if json_len_of(self) > MAX_FORM_BYTES {
            return Err(format!("a form is larger than {MAX_FORM_BYTES} bytes"));
        }
        check_count("form fields", self.fields.len(), MAX_FORM_FIELDS)?;
        for (i, f) in self.fields.iter().enumerate() {
            f.validate()?;
            let keyed = f.kind != FieldKind::Unknown && !f.key.is_empty();
            if keyed
                && self.fields[..i]
                    .iter()
                    .any(|g| g.kind != FieldKind::Unknown && g.key == f.key)
            {
                return Err(format!("field {} is listed twice", f.key));
            }
        }
        Ok(())
    }
}

fn json_len_of(form: &Form) -> usize {
    serde_json::to_value(form).map_or(usize::MAX, |v| json_len(&v))
}

/// Answers to a choice request are bounded like fixed options.
pub fn check_choices(options: &[Choice], error: Option<&str>) -> Result<(), String> {
    check_count("choice options", options.len(), MAX_CHOICES)?;
    options.iter().try_for_each(Validate::validate)?;
    match error {
        Some(e) => check_chars("a choice error", e, MAX_ERROR_CHARS),
        None => Ok(()),
    }
}

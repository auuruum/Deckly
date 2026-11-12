//! Settings forms, built in code and drawn by Deckly.
//!
//! ```
//! use deckly_sdk::Form;
//!
//! let mut form = Form::default();
//! form.number("step", "Step").range(1..=100).default(1);
//! form.choice("mode", "Mode").option("up", "Count up").option("down", "Count down");
//! form.checkbox("sound", "Play a sound");
//! form.file("sound_file", "Sound")
//!     .extensions(&["wav", "ogg"])
//!     .visible_when("sound", true);
//! assert_eq!(form.description().fields.len(), 4);
//! ```
//!
//! Each field sets the settings property named by its key, so the keys
//! match the fields of the action's `Settings` type. Text is cut to the
//! protocol's limits; a form with more fields than the limit keeps the
//! first ones and says so on standard error.

use deckly_plugin_protocol::form::cut_bytes;
use deckly_plugin_protocol::limits::{
    MAX_CHOICES, MAX_EXTENSIONS, MAX_FIELD_KEY_BYTES, MAX_FORM_FIELDS, MAX_HELP_CHARS,
    MAX_LABEL_CHARS, MAX_TEXT_VALUE_CHARS,
};
use deckly_plugin_protocol::{Choice, Condition, Field, FieldKind, truncate};
use serde::Serialize;
use std::ops::RangeInclusive;

/// A form being described. Each method adds a field and returns a builder
/// for its details.
#[derive(Default)]
pub struct Form {
    form: deckly_plugin_protocol::Form,
    /// Where the fields past the limit go, so the builder still has one.
    overflow: Field,
}

/// Numbers a number field or slider can be described with.
pub trait Number: Copy {
    /// Whether the type holds whole numbers only.
    const INTEGER: bool;
    fn to_f64(self) -> f64;
}

macro_rules! number {
    ($integer:expr => $($t:ty),*) => {$(
        impl Number for $t {
            const INTEGER: bool = $integer;
            #[allow(clippy::cast_precision_loss, clippy::cast_lossless)]
            fn to_f64(self) -> f64 {
                self as f64
            }
        }
    )*};
}
number!(true => i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);
number!(false => f32, f64);

impl Form {
    /// What the manifest carries.
    pub fn description(&self) -> &deckly_plugin_protocol::Form {
        &self.form
    }

    pub(crate) fn into_description(self) -> Option<deckly_plugin_protocol::Form> {
        (!self.form.fields.is_empty()).then_some(self.form)
    }

    fn add(&mut self, kind: FieldKind, key: &str, label: &str) -> FieldBuilder<'_> {
        let label_max = if kind == FieldKind::Note {
            MAX_HELP_CHARS
        } else {
            MAX_LABEL_CHARS
        };
        let field = Field {
            kind,
            key: cut_bytes(key, MAX_FIELD_KEY_BYTES).to_owned(),
            label: truncate(label, label_max).to_owned(),
            ..Field::default()
        };
        if self.form.fields.len() >= MAX_FORM_FIELDS {
            eprintln!(
                "deckly-sdk: a form holds at most {MAX_FORM_FIELDS} fields; {key:?} left out"
            );
            self.overflow = field;
            return FieldBuilder {
                field: &mut self.overflow,
            };
        }
        self.form.fields.push(field);
        let at = self.form.fields.len() - 1;
        FieldBuilder {
            field: &mut self.form.fields[at],
        }
    }

    /// One line of text; see [`FieldBuilder::multiline`].
    pub fn text(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Text, key, label)
    }

    /// A password or token. Deckly keeps it in the system's secret store,
    /// never in a profile, and hands it to the plugin as a plain string.
    pub fn secret(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Secret, key, label)
    }

    /// A number; see [`FieldBuilder::range`].
    pub fn number(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Number, key, label)
    }

    /// A slider between two numbers.
    pub fn slider<T: Number>(
        &mut self,
        key: &str,
        label: &str,
        range: RangeInclusive<T>,
    ) -> FieldBuilder<'_> {
        self.add(FieldKind::Slider, key, label).range(range)
    }

    /// On or off. The label is shown next to the box.
    pub fn checkbox(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Checkbox, key, label)
    }

    /// One of a list of options: fixed ([`FieldBuilder::option`]) or asked
    /// from the plugin when the form opens ([`FieldBuilder::dynamic`]).
    pub fn choice(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Choice, key, label)
    }

    /// A file's path; see [`FieldBuilder::extensions`].
    pub fn file(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::File, key, label)
    }

    /// A folder's path.
    pub fn folder(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Folder, key, label)
    }

    /// A colour, kept as `"#rrggbb"`.
    pub fn color(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Color, key, label)
    }

    /// A key combination, kept as `{"key": "K", "ctrl": true, "shift":
    /// false, "alt": false, "meta": false}`.
    pub fn hotkey(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Hotkey, key, label)
    }

    /// A button; pressing it calls the action's `button` handler (or the
    /// plugin's, in the plugin-wide form) with `key`.
    pub fn button(&mut self, key: &str, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Button, key, label)
    }

    /// A heading over the fields after it.
    pub fn section(&mut self, label: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Section, "", label)
    }

    /// A line of text.
    pub fn note(&mut self, text: &str) -> FieldBuilder<'_> {
        self.add(FieldKind::Note, "", text)
    }
}

/// The details of one field.
pub struct FieldBuilder<'a> {
    field: &'a mut Field,
}

impl FieldBuilder<'_> {
    /// A line under the field.
    pub fn help(self, text: &str) -> Self {
        self.field.help = truncate(text, MAX_HELP_CHARS).to_owned();
        self
    }

    /// Shown in an empty text field.
    pub fn placeholder(self, text: &str) -> Self {
        self.field.placeholder = truncate(text, MAX_LABEL_CHARS).to_owned();
        self
    }

    /// What the form shows while the settings hold no value. Keep it equal
    /// to the `Default` of the settings type.
    pub fn default(self, value: impl Serialize) -> Self {
        match serde_json::to_value(value) {
            Ok(v) => self.field.default = Some(v),
            Err(e) => eprintln!("deckly-sdk: default of {:?}: {e}", self.field.key),
        }
        self
    }

    /// An empty value is refused.
    pub fn required(self) -> Self {
        self.field.required = true;
        self
    }

    /// The lowest and highest number; whole numbers only for integer types.
    pub fn range<T: Number>(self, range: RangeInclusive<T>) -> Self {
        self.field.min = Some(range.start().to_f64());
        self.field.max = Some(range.end().to_f64());
        self.field.integer = T::INTEGER;
        self
    }

    pub fn min<T: Number>(self, min: T) -> Self {
        self.field.min = Some(min.to_f64());
        self.field.integer |= T::INTEGER;
        self
    }

    pub fn max<T: Number>(self, max: T) -> Self {
        self.field.max = Some(max.to_f64());
        self.field.integer |= T::INTEGER;
        self
    }

    /// How far one step of a number field or slider moves.
    pub fn step<T: Number>(self, step: T) -> Self {
        self.field.step = Some(step.to_f64());
        self
    }

    /// Whole numbers only.
    pub fn integer(self) -> Self {
        self.field.integer = true;
        self
    }

    /// The length of a text, in characters.
    pub fn length(self, range: RangeInclusive<u32>) -> Self {
        let cap = |n: u32| n.min(MAX_TEXT_VALUE_CHARS as u32);
        self.field.min_length = Some(cap(*range.start()));
        self.field.max_length = Some(cap(*range.end()));
        self
    }

    /// Several lines of text.
    pub fn multiline(self) -> Self {
        self.field.multiline = true;
        self
    }

    /// Adds an option to a choice.
    pub fn option(self, value: &str, label: &str) -> Self {
        if self.field.options.len() < MAX_CHOICES {
            self.field.options.push(Choice::new(value, label));
        }
        self
    }

    /// The choice's options come from the action's `choices` handler (or
    /// the plugin's, in the plugin-wide form) each time the form opens.
    pub fn dynamic(self) -> Self {
        self.field.dynamic = true;
        self
    }

    /// The file name extensions a file field offers, without the dot.
    pub fn extensions(self, extensions: &[&str]) -> Self {
        self.field.extensions = extensions
            .iter()
            .take(MAX_EXTENSIONS)
            .map(|e| e.trim_start_matches('.').to_owned())
            .collect();
        self
    }

    /// Shows the field only while the field `key` holds `value`.
    pub fn visible_when(self, key: &str, value: impl Serialize) -> Self {
        match serde_json::to_value(value) {
            Ok(equals) => {
                self.field.visible_when = Some(Condition {
                    field: key.to_owned(),
                    equals,
                });
            }
            Err(e) => eprintln!("deckly-sdk: condition of {:?}: {e}", self.field.key),
        }
        self
    }
}

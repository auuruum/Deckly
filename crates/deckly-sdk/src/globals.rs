//! Plugin-wide settings, shared by every key: an address and a password
//! that all of a plugin's keys use. Deckly stores them; the plugin-wide
//! form edits them.

use crate::key::Output;
use deckly_plugin_protocol::limits::MAX_SETTINGS_BYTES;
use deckly_plugin_protocol::{PluginMessage, json_len};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The plugin's global settings. Cheap to clone and `Send`; every key and
/// handler sees the same ones. Read them as your own type with
/// [`Globals::get`].
#[derive(Clone)]
pub struct Globals {
    inner: Arc<Inner>,
}

struct Inner {
    value: Mutex<Value>,
    loaded: AtomicBool,
    out: Arc<Output>,
}

impl Globals {
    pub(crate) fn new(out: Arc<Output>) -> Self {
        Self {
            inner: Arc::new(Inner {
                value: Mutex::new(Value::Object(Default::default())),
                loaded: AtomicBool::new(false),
                out,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Value> {
        self.inner.value.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The settings as `G`. Settings that do not fit read as `Default`,
    /// with a line on standard error.
    pub fn get<G: DeserializeOwned + Default>(&self) -> G {
        let value = self.lock().clone();
        serde_json::from_value(value).unwrap_or_else(|e| {
            eprintln!("deckly-sdk: global settings do not fit their type: {e}; using defaults");
            G::default()
        })
    }

    /// The settings as Deckly sent them.
    pub fn value(&self) -> Value {
        self.lock().clone()
    }

    /// Whether Deckly has sent the settings yet. Until then they are empty.
    pub fn is_loaded(&self) -> bool {
        self.inner.loaded.load(Ordering::Relaxed)
    }

    /// Stores new settings; Deckly saves them and they reach every key.
    pub fn set<G: Serialize>(&self, settings: &G) {
        let value = match serde_json::to_value(settings) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                eprintln!("deckly-sdk: global settings must be a JSON object; not saved");
                return;
            }
            Err(e) => {
                eprintln!("deckly-sdk: cannot serialize global settings: {e}");
                return;
            }
        };
        if json_len(&value) > MAX_SETTINGS_BYTES {
            eprintln!("deckly-sdk: global settings are larger than {MAX_SETTINGS_BYTES} bytes");
            return;
        }
        *self.lock() = value.clone();
        self.inner
            .out
            .send(&PluginMessage::SetGlobalSettings { settings: value }, None);
    }

    /// Changes the settings in place and stores them.
    pub fn update<G: Serialize + DeserializeOwned + Default>(&self, change: impl FnOnce(&mut G)) {
        let mut g = self.get::<G>();
        change(&mut g);
        self.set(&g);
    }

    /// What Deckly sent.
    pub(crate) fn received(&self, value: Value) {
        *self.lock() = value;
        self.inner.loaded.store(true, Ordering::Relaxed);
    }
}

//! A test harness that plays Deckly, so a plugin can be tested without it.
//!
//! ```
//! use deckly_sdk::prelude::*;
//! use deckly_sdk::testing::Harness;
//! use std::time::Duration;
//!
//! #[derive(Default)]
//! struct Hello;
//!
//! impl Action for Hello {
//!     const ID: &'static str = "com.example.hello.wave";
//!     const NAME: &'static str = "Wave";
//!     type Settings = serde_json::Value;
//!
//!     fn key_down(&mut self, key: &Key<Self>) {
//!         key.set_title("Hi");
//!     }
//! }
//!
//! let mut deck = Harness::start(Plugin::new("com.example.hello", "Hello").action::<Hello>());
//! deck.appear("k1", Hello::ID, serde_json::json!({}));
//! deck.press("k1");
//! assert_eq!(deck.title("k1", Duration::from_secs(5)).as_deref(), Some("Hi"));
//! deck.finish().unwrap();
//! ```

use crate::{Choice, Error, Face, Plugin, Result};
use deckly_plugin_protocol::{
    Device, Form, Hello, HostMessage, KeyInfo, Manifest, PROTOCOL, PluginMessage, Reader, Received,
    Writer,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::PipeWriter;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type Input = Arc<Mutex<Option<Writer<PipeWriter>>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A message from the plugin, with the image bytes sent with it.
pub type Sent = Received<PluginMessage>;

/// Runs a plugin on a thread and talks to it as Deckly would.
///
/// Like Deckly, the harness keeps the plugin's global settings: it answers
/// `get_global_settings` by itself and remembers what the plugin stores.
pub struct Harness {
    input: Input,
    sent: Receiver<Sent>,
    plugin: Option<JoinHandle<Result<()>>>,
    keys: HashMap<String, KeyInfo>,
    globals: Arc<Mutex<Value>>,
    manifest: Manifest,
    request: u64,
    /// The `ready` message the plugin answered `hello` with.
    pub ready: Option<PluginMessage>,
}

impl Harness {
    /// Starts the plugin and completes the handshake. Panics when the
    /// plugin does not answer: this is for tests.
    #[allow(clippy::expect_used)]
    pub fn start(plugin: Plugin) -> Self {
        let manifest = plugin.manifest();
        let (plugin_in, to_plugin) = std::io::pipe().expect("a pipe");
        let (from_plugin, plugin_out) = std::io::pipe().expect("a pipe");
        let handle = std::thread::spawn(move || plugin.run_with(plugin_in, plugin_out));
        let (tx, sent) = channel();
        let input: Input = Arc::new(Mutex::new(Some(Writer::new(to_plugin))));
        let globals = Arc::new(Mutex::new(Value::Object(Default::default())));
        std::thread::spawn({
            let (input, globals) = (input.clone(), globals.clone());
            move || {
                let mut reader = Reader::new(from_plugin);
                while let Ok(Some(m)) = reader.read::<PluginMessage>() {
                    match &m.message {
                        PluginMessage::GetGlobalSettings => {
                            let settings = lock(&globals).clone();
                            if let Some(w) = lock(&input).as_mut() {
                                let _ = w.write_message(&HostMessage::GlobalSettings { settings });
                            }
                            continue;
                        }
                        PluginMessage::SetGlobalSettings { settings } => {
                            lock(&globals).clone_from(settings);
                        }
                        _ => {}
                    }
                    if tx.send(m).is_err() {
                        break;
                    }
                }
            }
        });
        let mut harness = Self {
            input,
            sent,
            plugin: Some(handle),
            keys: HashMap::new(),
            globals,
            manifest,
            request: 0,
            ready: None,
        };
        harness.send(&HostMessage::Hello(Hello {
            protocol: PROTOCOL,
            app_version: "test".into(),
            platform: "test".into(),
            session: String::new(),
            language: "en".into(),
            devices: vec![Device {
                id: "test-device".into(),
                name: "Test deck".into(),
                columns: 5,
                rows: 3,
            }],
            services: Vec::new(),
        }));
        let ready = harness.recv(Duration::from_secs(10)).expect("ready");
        assert!(
            matches!(ready.message, PluginMessage::Ready { .. }),
            "the plugin answered hello with {:?}",
            ready.message
        );
        harness.ready = Some(ready.message);
        harness
    }

    /// Sends any message to the plugin.
    pub fn send(&mut self, message: &HostMessage) {
        if let Some(w) = lock(&self.input).as_mut() {
            let _ = w.write_message(message);
        }
    }

    /// The manifest the plugin describes itself with (`--describe`).
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The settings form of an action.
    pub fn form(&self, action: &str) -> Option<&Form> {
        self.manifest.action(action)?.form.as_ref()
    }

    /// The plugin-wide settings form.
    pub fn plugin_form(&self) -> Option<&Form> {
        self.manifest.form.as_ref()
    }

    /// Places a key without showing it, as the editor does with no device
    /// connected: forms can ask about it, but it never appears.
    pub fn place(&mut self, context: &str, action: &str, settings: Value) {
        let key = KeyInfo {
            context: context.into(),
            action: action.into(),
            device: String::new(),
            column: 0,
            row: 0,
            state: None,
            settings,
            in_multi_action: false,
        };
        self.keys.insert(context.into(), key);
    }

    /// Asks for the options of a dynamic choice in a key's form, as Deckly
    /// does when the form opens. `Err` holds the plugin's reason, or says
    /// that no answer came in time.
    pub fn choices(
        &mut self,
        context: &str,
        field: &str,
        timeout: Duration,
    ) -> std::result::Result<Vec<Choice>, String> {
        let key = Some(self.key(context));
        self.ask(field, key, timeout)
    }

    /// Asks for the options of a dynamic choice in the plugin-wide form.
    pub fn plugin_choices(
        &mut self,
        field: &str,
        timeout: Duration,
    ) -> std::result::Result<Vec<Choice>, String> {
        self.ask(field, None, timeout)
    }

    fn ask(
        &mut self,
        field: &str,
        key: Option<KeyInfo>,
        timeout: Duration,
    ) -> std::result::Result<Vec<Choice>, String> {
        self.request += 1;
        let request = self.request;
        self.send(&HostMessage::GetChoices {
            request,
            field: field.into(),
            key,
        });
        self.wait_for(timeout, |m| match &m.message {
            PluginMessage::Choices {
                request: r,
                options,
                error,
            } if *r == request => Some(match error {
                Some(e) => Err(e.clone()),
                None => Ok(options.clone()),
            }),
            _ => None,
        })
        .unwrap_or_else(|| Err("the plugin did not answer in time".into()))
    }

    /// Presses a button in a key's form.
    pub fn button(&mut self, context: &str, field: &str) {
        let key = Some(self.key(context));
        self.send(&HostMessage::Button {
            field: field.into(),
            key,
        });
    }

    /// Presses a button in the plugin-wide form.
    pub fn plugin_button(&mut self, field: &str) {
        self.send(&HostMessage::Button {
            field: field.into(),
            key: None,
        });
    }

    /// Changes the global settings as the user would in the plugin-wide
    /// form.
    pub fn set_globals(&mut self, settings: Value) {
        lock(&self.globals).clone_from(&settings);
        self.send(&HostMessage::GlobalSettings { settings });
    }

    /// The global settings as the plugin last stored them (or as
    /// [`Harness::set_globals`] set them).
    pub fn globals(&self) -> Value {
        lock(&self.globals).clone()
    }

    /// The next global settings the plugin stores.
    pub fn stored_globals(&self, timeout: Duration) -> Option<Value> {
        self.wait_for(timeout, |m| match &m.message {
            PluginMessage::SetGlobalSettings { settings } => Some(settings.clone()),
            _ => None,
        })
    }

    fn key(&self, context: &str) -> KeyInfo {
        self.keys.get(context).cloned().unwrap_or_else(|| KeyInfo {
            context: context.into(),
            action: String::new(),
            device: String::new(),
            column: 0,
            row: 0,
            state: None,
            settings: Value::Object(Default::default()),
            in_multi_action: false,
        })
    }

    /// Shows a key with `action` and its settings (a JSON object).
    pub fn appear(&mut self, context: &str, action: &str, settings: Value) {
        let index = self.keys.len() as u8;
        let key = KeyInfo {
            context: context.into(),
            action: action.into(),
            device: "test-device".into(),
            column: index % 5,
            row: index / 5,
            state: None,
            settings,
            in_multi_action: false,
        };
        self.keys.insert(context.into(), key.clone());
        self.send(&HostMessage::KeyAppear { key });
    }

    pub fn disappear(&mut self, context: &str) {
        let key = self.key(context);
        self.keys.remove(context);
        self.send(&HostMessage::KeyDisappear { key });
    }

    pub fn key_down(&mut self, context: &str) {
        let key = self.key(context);
        self.send(&HostMessage::KeyDown { key });
    }

    pub fn key_up(&mut self, context: &str) {
        let key = self.key(context);
        self.send(&HostMessage::KeyUp { key });
    }

    /// Key down, then key up.
    pub fn press(&mut self, context: &str) {
        self.key_down(context);
        self.key_up(context);
    }

    /// Changes a key's settings as the user would in Deckly.
    pub fn change_settings(&mut self, context: &str, settings: Value) {
        let mut key = self.key(context);
        key.settings = settings;
        self.keys.insert(context.into(), key.clone());
        self.send(&HostMessage::Settings { key });
    }

    /// The next message from the plugin, waiting at most `timeout`.
    pub fn recv(&self, timeout: Duration) -> Option<Sent> {
        self.sent.recv_timeout(timeout).ok()
    }

    /// Waits for the first message `pick` accepts, dropping the others.
    pub fn wait_for<T>(
        &self,
        timeout: Duration,
        mut pick: impl FnMut(&Sent) -> Option<T>,
    ) -> Option<T> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.checked_duration_since(Instant::now())?;
            let m = self.recv(left)?;
            if let Some(found) = pick(&m) {
                return Some(found);
            }
        }
    }

    /// The next face the plugin shows on a key.
    pub fn face(&self, context: &str, timeout: Duration) -> Option<Face> {
        self.wait_for(timeout, |m| match &m.message {
            PluginMessage::SetFace { context: c, face } if c == context => face.clone(),
            _ => None,
        })
    }

    /// The next title the plugin sets on a key.
    pub fn title(&self, context: &str, timeout: Duration) -> Option<String> {
        self.wait_for(timeout, |m| match &m.message {
            PluginMessage::SetTitle {
                context: c, title, ..
            } if c == context => title.clone(),
            _ => None,
        })
    }

    /// The next settings the plugin stores for a key.
    pub fn settings(&self, context: &str, timeout: Duration) -> Option<Value> {
        self.wait_for(timeout, |m| match &m.message {
            PluginMessage::SetSettings {
                context: c,
                settings,
            } if c == context => Some(settings.clone()),
            _ => None,
        })
    }

    /// Closes the plugin's input, as Deckly does when it quits, and waits
    /// for the plugin to return.
    pub fn finish(mut self) -> Result<()> {
        lock(&self.input).take();
        match self.plugin.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(Error::Protocol("the plugin panicked".into())),
            None => Ok(()),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Closing the input lets the plugin thread end on its own.
        lock(&self.input).take();
    }
}

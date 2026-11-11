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

use crate::{Error, Face, Plugin, Result};
use deckly_plugin_protocol::{
    Device, Hello, HostMessage, KeyInfo, PROTOCOL, PluginMessage, Reader, Received, Writer,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::PipeWriter;
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A message from the plugin, with the image bytes sent with it.
pub type Sent = Received<PluginMessage>;

/// Runs a plugin on a thread and talks to it as Deckly would.
pub struct Harness {
    input: Option<Writer<PipeWriter>>,
    sent: Receiver<Sent>,
    plugin: Option<JoinHandle<Result<()>>>,
    keys: HashMap<String, KeyInfo>,
    /// The `ready` message the plugin answered `hello` with.
    pub ready: Option<PluginMessage>,
}

impl Harness {
    /// Starts the plugin and completes the handshake. Panics when the
    /// plugin does not answer: this is for tests.
    #[allow(clippy::expect_used)]
    pub fn start(plugin: Plugin) -> Self {
        let (plugin_in, to_plugin) = std::io::pipe().expect("a pipe");
        let (from_plugin, plugin_out) = std::io::pipe().expect("a pipe");
        let handle = std::thread::spawn(move || plugin.run_with(plugin_in, plugin_out));
        let (tx, sent) = channel();
        std::thread::spawn(move || {
            let mut reader = Reader::new(from_plugin);
            while let Ok(Some(m)) = reader.read::<PluginMessage>() {
                if tx.send(m).is_err() {
                    break;
                }
            }
        });
        let mut harness = Self {
            input: Some(Writer::new(to_plugin)),
            sent,
            plugin: Some(handle),
            keys: HashMap::new(),
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
        if let Some(w) = &mut self.input {
            let _ = w.write_message(message);
        }
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
        self.input = None;
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
        self.input = None;
    }
}

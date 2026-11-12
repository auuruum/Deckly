//! Write plugins for Deckly in Rust.
//!
//! A plugin is a program that Deckly starts and talks to over its standard
//! input and output. Each action is a type implementing [`Action`]; Deckly
//! makes one value of it per key that shows the action, so per-key state is
//! a plain field.
//!
//! ```no_run
//! use deckly_sdk::prelude::*;
//!
//! #[derive(Clone, Default, Serialize, Deserialize)]
//! struct CounterSettings {
//!     step: u32,
//! }
//!
//! #[derive(Default)]
//! struct Counter {
//!     count: u32,
//! }
//!
//! impl Action for Counter {
//!     const ID: &'static str = "com.example.counter.count";
//!     const NAME: &'static str = "Counter";
//!     type Settings = CounterSettings;
//!
//!     fn key_down(&mut self, key: &Key<Self>) {
//!         self.count += key.settings().step.max(1);
//!         key.set_face(Face::new().value(self.count.to_string()).label("Count"));
//!     }
//! }
//!
//! fn main() -> deckly_sdk::Result<()> {
//!     Plugin::new("com.example.counter", "Counter")
//!         .action::<Counter>()
//!         .run()
//! }
//! ```
//!
//! Standard output belongs to the protocol: anything printed there with
//! `println!` breaks the connection, and Deckly restarts the plugin. Write
//! logs to standard error (`eprintln!`); Deckly keeps them in the plugin's
//! log file. The plugin exits when Deckly closes its standard input.
//!
//! Inside are threads and blocking I/O; there is no async runtime. A
//! [`Key`] is cheap to clone and can be moved to any thread.

mod key;
mod run;
pub mod testing;

pub use deckly_plugin_protocol as protocol;
pub use deckly_plugin_protocol::{Color, Face, ManifestState as State};
pub use key::{Image, Key};

use deckly_plugin_protocol::{Manifest, ManifestAction, PROTOCOL};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::io::{Read, Write};

/// What a plugin author usually needs.
pub mod prelude {
    pub use crate::{Action, Color, Face, Image, Key, Plugin, State};
    pub use serde::{Deserialize, Serialize};
}

/// The SDK version, written into `ready` and the manifest.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Deckly sent something the SDK cannot use, or the plugin was started
    /// by something that is not Deckly.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Protocol(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<deckly_plugin_protocol::Error> for Error {
    fn from(e: deckly_plugin_protocol::Error) -> Self {
        match e {
            deckly_plugin_protocol::Error::Io(e) => Error::Io(e),
            other => Error::Protocol(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// One kind of key a plugin offers. Deckly creates one value per key that
/// shows the action (with [`Default`]) and drops it when the key goes away.
///
/// Every handler has a default that does nothing.
pub trait Action: Default + Send + 'static {
    /// A reverse-DNS id that starts with the plugin id.
    const ID: &'static str;
    /// The name in Deckly's action list.
    const NAME: &'static str;
    const TOOLTIP: &'static str = "";
    /// An image file inside the plugin folder, for the action list.
    const ICON: &'static str = "";

    /// The key's settings, stored by Deckly in the profile. Keys start with
    /// `Default`; settings that no longer fit the type read as `Default`.
    type Settings: Serialize + DeserializeOwned + Default + Clone + Send + 'static;

    /// The states a key can be in, with the title and image Deckly shows
    /// before the plugin draws anything. None means one plain state.
    fn states() -> Vec<State> {
        Vec::new()
    }

    /// The key became visible: on a device, or in the editor.
    fn key_appear(&mut self, _key: &Key<Self>) {}
    /// The key is no longer visible. The value is dropped right after.
    fn key_disappear(&mut self, _key: &Key<Self>) {}
    fn key_down(&mut self, _key: &Key<Self>) {}
    fn key_up(&mut self, _key: &Key<Self>) {}
    /// The user changed the key's settings in Deckly.
    fn settings_changed(&mut self, _key: &Key<Self>) {}
    /// The key's timer fired; see [`Key::every`].
    fn tick(&mut self, _key: &Key<Self>) {}
}

/// A plugin: its description and its actions.
pub struct Plugin {
    manifest: Manifest,
    makers: Vec<run::Maker>,
}

impl Plugin {
    /// `id` is a reverse-DNS name (`com.example.counter`); `name` is shown
    /// to the user.
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            manifest: Manifest {
                id: id.to_owned(),
                name: name.to_owned(),
                version: "0.1.0".to_owned(),
                protocol: PROTOCOL,
                sdk: SDK_VERSION.to_owned(),
                ..Manifest::default()
            },
            makers: Vec::new(),
        }
    }

    pub fn version(mut self, version: &str) -> Self {
        version.clone_into(&mut self.manifest.version);
        self
    }

    pub fn author(mut self, author: &str) -> Self {
        author.clone_into(&mut self.manifest.author);
        self
    }

    pub fn license(mut self, license: &str) -> Self {
        license.clone_into(&mut self.manifest.license);
        self
    }

    pub fn description(mut self, text: &str) -> Self {
        text.clone_into(&mut self.manifest.description);
        self
    }

    /// An image file inside the plugin folder.
    pub fn icon(mut self, path: &str) -> Self {
        path.clone_into(&mut self.manifest.icon);
        self
    }

    /// The group in Deckly's action list; the plugin name by default.
    pub fn category(mut self, category: &str) -> Self {
        category.clone_into(&mut self.manifest.category);
        self
    }

    pub fn action<A: Action>(mut self) -> Self {
        self.manifest.actions.push(ManifestAction {
            id: A::ID.to_owned(),
            name: A::NAME.to_owned(),
            icon: A::ICON.to_owned(),
            tooltip: A::TOOLTIP.to_owned(),
            states: A::states(),
            form: None,
        });
        self.makers.push(run::maker::<A>());
        self
    }

    /// The manifest (`deckly-plugin.json`) for this plugin, naming the
    /// running program as the one for this target.
    pub fn manifest(&self) -> Manifest {
        let mut m = self.manifest.clone();
        let program = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name()?.to_str().map(str::to_owned));
        if let Some(program) = program {
            m.programs
                .insert(deckly_plugin_protocol::current_target(), program);
        }
        m
    }

    /// Runs the plugin on standard input and output until Deckly closes
    /// standard input. With `--describe` it prints the manifest instead.
    pub fn run(self) -> Result<()> {
        if std::env::args().skip(1).any(|a| a == "--describe") {
            let text = serde_json::to_string_pretty(&self.manifest())
                .map_err(|e| Error::Protocol(e.to_string()))?;
            println!("{text}");
            return Ok(());
        }
        run::run(self, std::io::stdin(), std::io::stdout(), true)
    }

    /// Runs the plugin on any pair of streams; [`testing::Harness`] uses it.
    pub fn run_with<R, W>(self, input: R, output: W) -> Result<()>
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        run::run(self, input, output, false)
    }
}

//! The system's own secret store, for passwords and tokens that plugin
//! settings forms ask for: Credential Manager on Windows, the Keychain on
//! macOS and the Secret Service (GNOME Keyring, KWallet) on Linux.
//!
//! Each system needs its own API, so each lives in a small module of its
//! own; a system without one returns an "unsupported" error and Deckly
//! falls back to a file of its own (see `crate::secrets`).

/// One secret: the plugin it belongs to and Deckly's id for it.
#[derive(Clone, Copy, Debug)]
pub struct Name<'a> {
    pub plugin: &'a str,
    pub id: &'a str,
}

impl Name<'_> {
    /// The name other programs (and the user, in the store's own app) see.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn target(&self) -> String {
        format!("Deckly/{}/{}", self.plugin, self.id)
    }

    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub fn label(&self) -> String {
        format!("Deckly: a secret of the plugin {}", self.plugin)
    }
}

#[cfg(target_os = "linux")]
#[path = "secrets/linux.rs"]
mod system;
#[cfg(target_os = "macos")]
#[path = "secrets/macos.rs"]
mod system;
#[cfg(windows)]
#[path = "secrets/windows.rs"]
mod system;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod system {
    use super::Name;

    const UNSUPPORTED: &str = "this system has no secret store Deckly can use";

    pub fn probe() -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn read(_: Name<'_>) -> Result<Option<String>, String> {
        Err(UNSUPPORTED.into())
    }
    pub fn write(_: Name<'_>, _: &str) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn delete(_: Name<'_>) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

/// Whether the store is there and can be used; why not otherwise.
pub fn probe() -> Result<(), String> {
    system::probe()
}

/// A secret's value; `None` when the store has none by that name.
pub fn read(name: Name<'_>) -> Result<Option<String>, String> {
    system::read(name)
}

/// Stores a value, replacing an older one.
pub fn write(name: Name<'_>, value: &str) -> Result<(), String> {
    system::write(name, value)
}

/// Removes a secret; one that is not there is no error.
pub fn delete(name: Name<'_>) -> Result<(), String> {
    system::delete(name)
}

//! Systems without simulated input.

use super::{Hotkey, MediaKey};

const UNSUPPORTED: &str = "not supported on this platform yet";

pub fn send_hotkey(_hotkey: &Hotkey) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

pub fn type_text(_text: &str) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

pub fn media_key(_key: MediaKey) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

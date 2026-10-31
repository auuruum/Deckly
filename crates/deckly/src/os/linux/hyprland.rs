//! Asks Hyprland which layout is in use. It tells every status bar the name of
//! a virtual keyboard's layout as if the user had switched to it, so a layout
//! of Deckly's goes by the name of the one in use, and nothing seems to change.

use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// How long a key press may wait for the answer.
const ANSWER_TIME: Duration = Duration::from_millis(100);
const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;

/// The layout of the main keyboard, `None` outside Hyprland.
pub fn active_layout() -> Option<String> {
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?)
        .join("hypr")
        .join(instance)
        .join(".socket.sock");
    let mut socket = UnixStream::connect(path).ok()?;
    socket.set_read_timeout(Some(ANSWER_TIME)).ok()?;
    socket.set_write_timeout(Some(ANSWER_TIME)).ok()?;
    socket.write_all(b"j/devices").ok()?;
    let mut reply = Vec::new();
    socket.take(MAX_REPLY_BYTES).read_to_end(&mut reply).ok()?;
    main_layout(&reply)
}

fn main_layout(devices: &[u8]) -> Option<String> {
    let devices: Value = serde_json::from_slice(devices).ok()?;
    let main = devices
        .get("keyboards")?
        .as_array()?
        .iter()
        .find(|keyboard| keyboard.get("main").and_then(Value::as_bool) == Some(true))?;
    let name = main.get("active_keymap")?.as_str()?;
    (name != "error").then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_layout_of_the_main_keyboard() {
        let devices = br#"{"keyboards": [
            {"name": "other", "main": false, "active_keymap": "English (US)"},
            {"name": "real", "main": true, "active_keymap": "Russian"}
        ]}"#;
        assert_eq!(main_layout(devices).as_deref(), Some("Russian"));
    }

    #[test]
    fn a_layout_without_a_name_counts_as_none() {
        let devices = br#"{"keyboards": [{"main": true, "active_keymap": "error"}]}"#;
        assert_eq!(main_layout(devices), None);
    }

    #[test]
    fn odd_replies_are_harmless() {
        assert_eq!(main_layout(b"unknown request"), None);
        assert_eq!(main_layout(br#"{"keyboards": []}"#), None);
        assert_eq!(main_layout(br#"{"keyboards": [{"main": true}]}"#), None);
        assert_eq!(main_layout(br#"{"keyboards": 3}"#), None);
    }
}

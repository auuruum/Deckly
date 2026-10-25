//! Linux and macOS. Opening files and URLs uses the desktop's standard
//! opener; simulated input is not implemented yet.

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

pub fn open(target: &str) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("nothing to open".into());
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(target)
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot open \"{target}\": {e}"))
}

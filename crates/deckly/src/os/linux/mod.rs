//! Linux: simulated keys and text, and media control.
//!
//! No single way of pressing keys works everywhere, so each session has its
//! own list of ways, tried in order:
//!
//! - Wayland: the virtual keyboard protocol (wlroots compositors, Hyprland),
//!   then the RemoteDesktop portal (GNOME, KDE; asks the user once), then
//!   uinput. XTEST is left out: under XWayland it reaches X11 windows only
//!   and would report success for keys nothing received.
//! - X11: XTEST, then uinput.
//! - No graphical session in the environment: uinput.
//!
//! `DECKLY_INPUT=wayland|portal|x11|uinput` forces one way, for debugging.

mod keys;
mod media;
mod portal;
mod uinput;
mod wayland;
mod x11;

use self::keys::{Key, Stroke};
use super::{Hotkey, MediaKey};

/// Why a way of pressing keys did not deliver them.
#[derive(Debug, PartialEq)]
enum Failure {
    /// This session does not offer it; try the next one.
    Unavailable(String),
    /// It started and then failed: some keys may have been pressed already,
    /// so the next way would repeat them.
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Wayland,
    Portal,
    X11,
    Uinput,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Backend::Wayland => "virtual keyboard",
            Backend::Portal => "remote desktop portal",
            Backend::X11 => "XTEST",
            Backend::Uinput => "uinput",
        }
    }

    fn send(self, strokes: &[Stroke]) -> Result<(), Failure> {
        match self {
            Backend::Wayland => wayland::send(strokes),
            Backend::Portal => portal::send(strokes),
            Backend::X11 => x11::send(strokes),
            Backend::Uinput => uinput::send(strokes),
        }
    }
}

/// The ways to try for a session. `forced` is `DECKLY_INPUT`.
fn order(forced: Option<&str>, wayland: bool, x11: bool) -> Vec<Backend> {
    match forced {
        Some("wayland") => vec![Backend::Wayland],
        Some("portal") => vec![Backend::Portal],
        Some("x11") => vec![Backend::X11],
        Some("uinput") => vec![Backend::Uinput],
        _ if wayland => vec![Backend::Wayland, Backend::Portal, Backend::Uinput],
        _ if x11 => vec![Backend::X11, Backend::Uinput],
        _ => vec![Backend::Uinput],
    }
}

fn session_order() -> Vec<Backend> {
    let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let forced = std::env::var("DECKLY_INPUT").ok();
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    order(
        forced.as_deref(),
        set("WAYLAND_DISPLAY") || session == "wayland",
        set("DISPLAY"),
    )
}

fn deliver(strokes: &[Stroke]) -> Result<(), String> {
    if strokes.is_empty() {
        return Ok(());
    }
    let mut reasons = Vec::new();
    for backend in session_order() {
        match backend.send(strokes) {
            Ok(()) => return Ok(()),
            Err(Failure::Failed(e)) => return Err(format!("{}: {e}", backend.name())),
            Err(Failure::Unavailable(why)) => reasons.push(format!("{}: {why}", backend.name())),
        }
    }
    Err(format!("cannot press keys here ({})", reasons.join("; ")))
}

pub fn send_hotkey(hotkey: &Hotkey) -> Result<(), String> {
    let stroke = keys::hotkey(hotkey).ok_or_else(|| format!("unknown key \"{}\"", hotkey.key))?;
    deliver(&[stroke])
}

pub fn type_text(text: &str) -> Result<(), String> {
    deliver(&keys::text(text))
}

pub fn media_key(key: MediaKey) -> Result<(), String> {
    if media::control(key)? {
        return Ok(());
    }
    // Nothing answered directly; the desktop may still bind the key.
    deliver(&[Stroke::single(Key::media(key))])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types into the focused window of the real session. Run it by hand:
    /// `DECKLY_INPUT=x11 cargo test -p deckly -- --ignored --nocapture typing`.
    #[test]
    #[ignore = "presses real keys"]
    fn typing_reaches_the_focused_window() {
        type_text("Deckly 123 Abc !@# \u{20ac} \u{44f}\n").unwrap();
        let select_all = Hotkey {
            ctrl: true,
            key: "A".into(),
            ..Hotkey::default()
        };
        send_hotkey(&select_all).unwrap();
    }

    #[test]
    fn each_session_tries_what_works_there() {
        use Backend::*;
        assert_eq!(order(None, true, true), [Wayland, Portal, Uinput]);
        assert_eq!(order(None, false, true), [X11, Uinput]);
        assert_eq!(order(None, false, false), [Uinput]);
    }

    #[test]
    fn xtest_is_never_used_under_wayland_unless_forced() {
        assert!(!order(None, true, true).contains(&Backend::X11));
        assert_eq!(order(Some("x11"), true, true), [Backend::X11]);
        assert_eq!(order(Some("nonsense"), false, true)[0], Backend::X11);
    }
}

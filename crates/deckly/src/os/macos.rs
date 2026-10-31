//! macOS: Quartz events for keys, text and media keys. macOS drops these
//! events without an error unless the app has the Accessibility permission,
//! so that is checked first and reported with where to grant it.

use super::{Hotkey, MediaKey};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};
use objc2_foundation::NSPoint;
use std::time::Duration;

const SHIFT: u16 = 0x38;
const CONTROL: u16 = 0x3B;
const OPTION: u16 = 0x3A;
const COMMAND: u16 = 0x37;
const RETURN: u16 = 0x24;
const TAB: u16 = 0x30;
/// Between characters, so busy apps keep them in order.
const KEY_GAP: Duration = Duration::from_millis(2);
/// Characters one Quartz keyboard event carries reliably, in UTF-16 units.
const CHUNK_UNITS: usize = 16;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

fn check_permission() -> Result<(), String> {
    // SAFETY: takes no arguments and only reads the process's trust state.
    if unsafe { AXIsProcessTrusted() } {
        Ok(())
    } else {
        Err("allow Deckly under System Settings > Privacy & Security > Accessibility".into())
    }
}

/// Virtual key codes of an ANSI keyboard, from the system's HIToolbox
/// header. They name places on the keyboard, like Windows scan codes.
fn key_code(name: &str) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D, 0x1F,
        0x23, 0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
    ];
    const DIGITS: [u16; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
    const FUNCTIONS: [u16; 20] = [
        0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71,
        0x6A, 0x40, 0x4F, 0x50, 0x5A,
    ];
    let code = match name {
        "Backspace" => 0x33,
        "Tab" => TAB,
        "Enter" => RETURN,
        "Escape" => 0x35,
        "Space" => 0x31,
        "PageUp" => 0x74,
        "PageDown" => 0x79,
        "End" => 0x77,
        "Home" => 0x73,
        "Left" => 0x7B,
        "Up" => 0x7E,
        "Right" => 0x7C,
        "Down" => 0x7D,
        "Insert" => 0x72,
        "Delete" => 0x75,
        "Semicolon" | "Colon" => 0x29,
        "Equals" | "Plus" => 0x18,
        "Comma" => 0x2B,
        "Minus" => 0x1B,
        "Period" => 0x2F,
        "Slash" | "Questionmark" => 0x2C,
        "Backtick" => 0x32,
        "OpenBracket" | "OpenCurlyBracket" => 0x21,
        "Backslash" | "Pipe" => 0x2A,
        "CloseBracket" | "CloseCurlyBracket" => 0x1E,
        "Quote" => 0x27,
        _ => {
            let mut chars = name.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_uppercase() => {
                    LETTERS.get(c as usize - 'A' as usize).copied()
                }
                (Some(c), None) if c.is_ascii_digit() => {
                    DIGITS.get(c as usize - '0' as usize).copied()
                }
                (Some('F'), Some(_)) => {
                    let n: usize = name.get(1..)?.parse().ok()?;
                    FUNCTIONS.get(n.checked_sub(1)?).copied()
                }
                _ => None,
            };
        }
    };
    Some(code)
}

fn post(code: u16, down: bool, flags: CGEventFlags) -> Result<(), String> {
    let event =
        CGEvent::new_keyboard_event(None, code, down).ok_or("macOS would not make a key event")?;
    CGEvent::set_flags(Some(&event), flags);
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

pub fn send_hotkey(hotkey: &Hotkey) -> Result<(), String> {
    let code = key_code(&hotkey.key).ok_or_else(|| format!("unknown key \"{}\"", hotkey.key))?;
    check_permission()?;
    let modifiers = [
        (hotkey.ctrl, CONTROL, CGEventFlags::MaskControl),
        (hotkey.shift, SHIFT, CGEventFlags::MaskShift),
        (hotkey.alt, OPTION, CGEventFlags::MaskAlternate),
        (hotkey.meta, COMMAND, CGEventFlags::MaskCommand),
    ];
    let held: Vec<(u16, CGEventFlags)> = modifiers
        .into_iter()
        .filter_map(|(on, code, flag)| on.then_some((code, flag)))
        .collect();
    let mut flags = CGEventFlags(0);
    for &(code, flag) in &held {
        flags |= flag;
        post(code, true, flags)?;
    }
    post(code, true, flags)?;
    post(code, false, flags)?;
    for &(code, flag) in held.iter().rev() {
        flags &= !flag;
        post(code, false, flags)?;
    }
    Ok(())
}

/// Posts text carried on a key event; the key's own code does not matter.
fn post_units(units: &[u16], down: bool) -> Result<(), String> {
    let event =
        CGEvent::new_keyboard_event(None, 0, down).ok_or("macOS would not make a key event")?;
    // SAFETY: the pointer and length describe `units`, which outlives the
    // call; Quartz copies the string.
    unsafe {
        CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr());
    }
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

fn post_text(text: &str) -> Result<(), String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    for part in units.chunks(CHUNK_UNITS) {
        post_units(part, true)?;
        post_units(part, false)?;
        std::thread::sleep(KEY_GAP);
    }
    Ok(())
}

pub fn type_text(text: &str) -> Result<(), String> {
    check_permission()?;
    let mut run = String::new();
    for c in text.chars() {
        let key = match c {
            '\n' => RETURN,
            '\t' => TAB,
            c if c.is_control() => continue,
            c => {
                run.push(c);
                continue;
            }
        };
        post_text(&run)?;
        run.clear();
        post(key, true, CGEventFlags(0))?;
        post(key, false, CGEventFlags(0))?;
    }
    post_text(&run)
}

/// Media keys are system events, not key codes; numbers from the system's
/// `ev_keymap.h`. `None` for what the keyboard has no key for.
fn media_code(key: MediaKey) -> Option<isize> {
    match key {
        MediaKey::VolumeUp => Some(0),
        MediaKey::VolumeDown => Some(1),
        MediaKey::Mute => Some(7),
        MediaKey::PlayPause => Some(16),
        MediaKey::Next => Some(17),
        MediaKey::Previous => Some(18),
        MediaKey::Stop => None,
    }
}

/// The `data1` of a media key event: the key, then the state in the bits
/// the system reads for press (0xA) and release (0xB).
fn media_data(code: isize, down: bool) -> isize {
    (code << 16) | (if down { 0xA } else { 0xB } << 8)
}

fn post_media(code: isize, down: bool) -> Result<(), String> {
    /// `NX_SUBTYPE_AUX_CONTROL_BUTTONS`.
    const AUX_CONTROL_BUTTONS: i16 = 8;
    let state: usize = if down { 0xA00 } else { 0xB00 };
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::SystemDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags(state),
        0.0,
        0,
        None,
        AUX_CONTROL_BUTTONS,
        media_data(code, down),
        -1,
    )
    .ok_or("macOS would not make a media event")?;
    let event = event
        .CGEvent()
        .ok_or("macOS would not post a media event")?;
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

pub fn media_key(key: MediaKey) -> Result<(), String> {
    let code = media_code(key).ok_or("macOS has no Stop media key")?;
    check_permission()?;
    post_media(code, true)?;
    post_media(code, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_key_names() {
        assert_eq!(key_code("A"), Some(0x00));
        assert_eq!(key_code("Z"), Some(0x06));
        assert_eq!(key_code("0"), Some(0x1D));
        assert_eq!(key_code("9"), Some(0x19));
        assert_eq!(key_code("F1"), Some(0x7A));
        assert_eq!(key_code("F20"), Some(0x5A));
        assert_eq!(key_code("F21"), None);
        assert_eq!(key_code("F0"), None);
        assert_eq!(key_code("Fx"), None);
        assert_eq!(key_code("a"), None);
        assert_eq!(key_code(""), None);
    }

    #[test]
    fn media_events_carry_key_and_state() {
        assert_eq!(media_data(16, true), 0x10_0A00);
        assert_eq!(media_data(16, false), 0x10_0B00);
    }
}

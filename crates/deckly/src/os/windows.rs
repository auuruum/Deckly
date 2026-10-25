//! Windows: SendInput for keys and text, ShellExecuteW for opening targets.

use super::{Hotkey, MediaKey};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_LWIN: u16 = 0x5B;
const VK_F1: u16 = 0x70;

fn virtual_key(name: &str) -> Option<u16> {
    let vk = match name {
        "Backspace" => VK_BACK,
        "Tab" => VK_TAB,
        "Enter" => VK_RETURN,
        "Escape" => VK_ESCAPE,
        "Space" => VK_SPACE,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "End" => 0x23,
        "Home" => 0x24,
        "Left" => 0x25,
        "Up" => 0x26,
        "Right" => 0x27,
        "Down" => 0x28,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Semicolon" => 0xBA,
        "Equals" | "Plus" => 0xBB,
        "Comma" => 0xBC,
        "Minus" => 0xBD,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Backtick" => 0xC0,
        "OpenBracket" => 0xDB,
        "Backslash" => 0xDC,
        "CloseBracket" => 0xDD,
        "Quote" => 0xDE,
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                // Letters and digits share their ASCII code with the VK code.
                (Some(c), None) if c.is_ascii_uppercase() || c.is_ascii_digit() => c as u16,
                (Some('F'), Some(_)) => {
                    let n: u16 = name.get(1..)?.parse().ok()?;
                    if !(1..=24).contains(&n) {
                        return None;
                    }
                    VK_F1 + n - 1
                }
                _ => return None,
            }
        }
    };
    Some(vk)
}

/// Keys that need the extended flag to be told apart from numpad keys.
fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0xAD..=0xB3)
}

fn key_input(vk: u16, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn unicode_input(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: `inputs` is a valid slice of initialized INPUT structures and
    // the size argument matches the structure the API expects.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err("Windows blocked the simulated input".into())
    }
}

pub fn send_hotkey(hotkey: &Hotkey) -> Result<(), String> {
    let vk = virtual_key(&hotkey.key).ok_or_else(|| format!("unknown key \"{}\"", hotkey.key))?;
    let modifiers: Vec<u16> = [
        (hotkey.ctrl, VK_CONTROL),
        (hotkey.shift, VK_SHIFT),
        (hotkey.alt, VK_MENU),
        (hotkey.meta, VK_LWIN),
    ]
    .into_iter()
    .filter_map(|(on, vk)| on.then_some(vk))
    .collect();
    let mut inputs: Vec<INPUT> = modifiers.iter().map(|&m| key_input(m, false)).collect();
    inputs.push(key_input(vk, false));
    inputs.push(key_input(vk, true));
    inputs.extend(modifiers.iter().rev().map(|&m| key_input(m, true)));
    send(&inputs)
}

pub fn type_text(text: &str) -> Result<(), String> {
    let mut inputs = Vec::with_capacity(text.len() * 2);
    for c in text.chars() {
        if c == '\n' {
            inputs.push(key_input(VK_RETURN, false));
            inputs.push(key_input(VK_RETURN, true));
            continue;
        }
        if c == '\r' {
            continue;
        }
        let mut units = [0u16; 2];
        for &unit in c.encode_utf16(&mut units).iter() {
            inputs.push(unicode_input(unit, false));
            inputs.push(unicode_input(unit, true));
        }
    }
    send(&inputs)
}

pub fn media_key(key: MediaKey) -> Result<(), String> {
    let vk = match key {
        MediaKey::Mute => 0xAD,
        MediaKey::VolumeDown => 0xAE,
        MediaKey::VolumeUp => 0xAF,
        MediaKey::Next => 0xB0,
        MediaKey::Previous => 0xB1,
        MediaKey::Stop => 0xB2,
        MediaKey::PlayPause => 0xB3,
    };
    send(&[key_input(vk, false), key_input(vk, true)])
}

pub fn open(target: &str) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("nothing to open".into());
    }
    // SAFETY: all string arguments are valid, NUL-terminated wide strings
    // that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW reports success with a value above 32.
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err(format!("cannot open \"{target}\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_key_names() {
        assert_eq!(virtual_key("A"), Some(0x41));
        assert_eq!(virtual_key("7"), Some(0x37));
        assert_eq!(virtual_key("F1"), Some(0x70));
        assert_eq!(virtual_key("F24"), Some(0x87));
        assert_eq!(virtual_key("F25"), None);
        assert_eq!(virtual_key("F"), Some(0x46));
        assert_eq!(virtual_key("Left"), Some(0x25));
        assert_eq!(virtual_key("a"), None);
        assert_eq!(virtual_key(""), None);
        assert_eq!(virtual_key("Fx"), None);
    }
}

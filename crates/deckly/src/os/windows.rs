//! Windows: SendInput for keys and text, ShellExecuteW for opening targets.

use super::command::split_words;
use super::{Hotkey, MediaKey};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

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

/// The Windows version, from the registry: the version calls of the system
/// answer with what the program's manifest declares.
pub fn version_text() -> String {
    const KEY: PCWSTR = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let number = |name: PCWSTR| {
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: the strings are static NUL-terminated wide strings, and
        // the output pointer and its size describe `value`, which outlives
        // the call.
        let result = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                KEY,
                name,
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut value).cast()),
                Some(&raw mut size),
            )
        };
        (result == ERROR_SUCCESS).then_some(value)
    };
    let mut build = [0u16; 16];
    let mut size = size_of_val(&build) as u32;
    // SAFETY: as above, for the `build` buffer; the call writes at most
    // `size` bytes and ends the string with a NUL.
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            KEY,
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(build.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    let end = build.iter().position(|&c| c == 0).unwrap_or(build.len());
    let build = String::from_utf16_lossy(&build[..end]);
    match (
        number(w!("CurrentMajorVersionNumber")),
        number(w!("CurrentMinorVersionNumber")),
    ) {
        (Some(major), Some(minor)) if result == ERROR_SUCCESS => format!("{major}.{minor}.{build}"),
        _ => String::new(),
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

/// Hands a target to the shell, with `parameters` for a program.
fn shell_open(target: &str, parameters: &str) -> bool {
    // SAFETY: all string arguments are valid, NUL-terminated wide strings
    // that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            &HSTRING::from(parameters),
            None,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW reports success with a value above 32.
    result.0 as usize > 32
}

/// Puts a word back into a Windows command line.
fn quoted(word: &str) -> String {
    if word.is_empty() || word.contains(char::is_whitespace) {
        format!("\"{word}\"")
    } else {
        word.to_owned()
    }
}

/// Opens a URL or file; the text is never taken for a command line, so it is
/// safe for text from plugins.
pub fn open_url(target: &str) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("nothing to open".into());
    }
    if shell_open(target, "") {
        Ok(())
    } else {
        Err(format!("cannot open \"{target}\""))
    }
}

/// Opens what the user wrote in an Open action: a URL, a path, or a command
/// line with arguments.
pub fn open(target: &str) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("nothing to open".into());
    }
    if shell_open(target, "") {
        return Ok(());
    }
    let words = split_words(target).ok_or_else(|| format!("unclosed quote in \"{target}\""))?;
    let opened = words.split_first().is_some_and(|(program, args)| {
        let args: Vec<String> = args.iter().map(|a| quoted(a)).collect();
        shell_open(program, &args.join(" "))
    });
    if opened {
        Ok(())
    } else {
        Err(format!("cannot open \"{target}\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_words_are_quoted_again() {
        assert_eq!(quoted("toggle"), "toggle");
        assert_eq!(quoted("RGB Lights"), "\"RGB Lights\"");
        assert_eq!(quoted(""), "\"\"");
    }

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

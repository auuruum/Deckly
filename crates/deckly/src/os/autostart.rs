//! "Open automatically at login".
//!
//! Each desktop has its own per-user place for programs started at login:
//! the `Run` registry key on Windows, an XDG autostart entry on Linux and a
//! launch agent on macOS. Deckly only adds or removes its own entry there.
//!
//! While "Start in tray at login" is on, the entry also passes
//! [`START_IN_TRAY_ARG`]; turning that setting on or off writes the entry
//! again. The XDG entry is read the same way by X11 and Wayland sessions.
//! Inside a Flatpak sandbox the entry would land in the sandbox's own
//! config folder, where no desktop looks, so there it is unsupported.

use crate::launch::START_IN_TRAY_ARG;
use std::path::Path;

/// Name of Deckly's entry in each place.
const ENTRY: &str = "Deckly";
#[cfg_attr(windows, allow(dead_code))]
const AGENT_LABEL: &str = "com.deckly.app";

pub fn is_enabled() -> Result<bool, String> {
    imp::is_enabled()
}

/// Adds or removes the entry. `start_in_tray` says whether the entry asks
/// for the tray; an entry already there is written again.
pub fn set_enabled(enabled: bool, start_in_tray: bool) -> Result<(), String> {
    supported()?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find Deckly's program: {e}"))?;
    imp::set_enabled(enabled, &exe, start_in_tray)
}

#[cfg(target_os = "linux")]
fn supported() -> Result<(), String> {
    if Path::new("/.flatpak-info").exists() {
        Err("not supported inside a Flatpak sandbox".into())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
fn supported() -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
mod imp {
    use super::ENTRY;
    use std::path::Path;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::core::{HSTRING, w};

    const RUN: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

    pub fn is_enabled() -> Result<bool, String> {
        let name = HSTRING::from(ENTRY);
        // SAFETY: both strings are valid, NUL-terminated wide strings that
        // outlive the call; no output buffers are passed, so the call only
        // reports whether the value exists.
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN,
                &name,
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            )
        };
        match result {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND => Ok(false),
            e => Err(format!("cannot read the login items: error {}", e.0)),
        }
    }

    pub fn set_enabled(enabled: bool, exe: &Path, start_in_tray: bool) -> Result<(), String> {
        let name = HSTRING::from(ENTRY);
        let result = if enabled {
            let command: Vec<u16> = super::command_line(exe, start_in_tray)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let bytes = u32::try_from(command.len() * 2).map_err(|_| "path is too long")?;
            // SAFETY: `command` is a NUL-terminated UTF-16 buffer of `bytes`
            // bytes that lives until the call returns; the key and value
            // names are valid wide strings.
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    RUN,
                    &name,
                    REG_SZ.0,
                    Some(command.as_ptr().cast()),
                    bytes,
                )
            }
        } else {
            // SAFETY: the key and value names are valid wide strings.
            match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, &name) } {
                ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                other => other,
            }
        };
        if result == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("cannot change the login items: error {}", result.0))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn is_enabled() -> Result<bool, String> {
        Ok(super::entry_path()?.exists())
    }

    pub fn set_enabled(enabled: bool, exe: &Path, start_in_tray: bool) -> Result<(), String> {
        let path = super::entry_path()?;
        if enabled {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, super::entry_file(exe, start_in_tray)).map_err(|e| e.to_string())
        } else {
            match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

/// The `Run` value: the quoted program, then the flag. Windows paths cannot
/// hold `"`, so quoting is enough.
#[cfg_attr(not(windows), allow(dead_code))]
fn command_line(exe: &Path, start_in_tray: bool) -> String {
    let mut line = format!("\"{}\"", exe.display());
    if start_in_tray {
        line.push(' ');
        line.push_str(START_IN_TRAY_ARG);
    }
    line
}

#[cfg(not(windows))]
fn entry_path() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    let env = |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = env("HOME");
    let path = if cfg!(target_os = "macos") {
        home.map(|h| h.join(format!("Library/LaunchAgents/{AGENT_LABEL}.plist")))
    } else {
        env("XDG_CONFIG_HOME")
            .or_else(|| home.map(|h| h.join(".config")))
            .map(|c| c.join("autostart/deckly.desktop"))
    };
    path.ok_or_else(|| "cannot find the home folder".into())
}

#[cfg(not(windows))]
fn entry_file(exe: &Path, start_in_tray: bool) -> String {
    if cfg!(target_os = "macos") {
        launch_agent(exe, start_in_tray)
    } else {
        desktop_entry(exe, start_in_tray)
    }
}

/// An XDG autostart entry. `Exec` quotes the path as the desktop entry
/// specification asks.
#[cfg_attr(windows, allow(dead_code))]
fn desktop_entry(exe: &Path, start_in_tray: bool) -> String {
    let mut quoted = String::new();
    for c in exe.display().to_string().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    // The flag holds no character the specification asks to quote.
    let flag = if start_in_tray {
        format!(" {START_IN_TRAY_ARG}")
    } else {
        String::new()
    };
    format!(
        "[Desktop Entry]\nType=Application\nName={ENTRY}\nExec=\"{quoted}\"{flag}\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// A launch agent that starts Deckly once at login.
#[cfg_attr(windows, allow(dead_code))]
fn launch_agent(exe: &Path, start_in_tray: bool) -> String {
    let mut path = String::new();
    for c in exe.display().to_string().chars() {
        match c {
            '&' => path.push_str("&amp;"),
            '<' => path.push_str("&lt;"),
            '>' => path.push_str("&gt;"),
            c => path.push(c),
        }
    }
    let flag = if start_in_tray {
        format!("\t\t<string>{START_IN_TRAY_ARG}</string>\n")
    } else {
        String::new()
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>{AGENT_LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{path}</string>\n{flag}\t</array>\n\
         \t<key>RunAtLoad</key>\n\t<true/>\n\
         </dict>\n</plist>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quotes_the_path() {
        let text = desktop_entry(Path::new("/opt/my \"apps\"/deckly"), false);
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("Exec=\"/opt/my \\\"apps\\\"/deckly\"\n"));
    }

    #[test]
    fn desktop_entry_passes_the_flag_only_for_the_tray() {
        let exe = Path::new("/usr/lib/deckly/deckly");
        let tray = desktop_entry(exe, true);
        assert!(tray.contains("\nExec=\"/usr/lib/deckly/deckly\" --start-in-tray\n"));
        let window = desktop_entry(exe, false);
        assert!(window.contains("\nExec=\"/usr/lib/deckly/deckly\"\n"));
        assert!(!window.contains(START_IN_TRAY_ARG));
        for text in [tray, window] {
            assert_eq!(text.matches("Exec=").count(), 1);
            assert!(text.contains("\nType=Application\n"));
        }
    }

    #[test]
    fn launch_agent_escapes_the_path() {
        let text = launch_agent(Path::new("/Applications/A&B <x>/Deckly"), false);
        assert!(text.contains("<string>/Applications/A&amp;B &lt;x&gt;/Deckly</string>"));
        assert!(text.contains("<string>com.deckly.app</string>"));
        assert!(text.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn launch_agent_passes_the_flag_as_its_own_argument() {
        let exe = Path::new("/Applications/Deckly.app/Contents/MacOS/deckly");
        let tray = launch_agent(exe, true);
        assert!(tray.contains(
            "<array>\n\t\t<string>/Applications/Deckly.app/Contents/MacOS/deckly</string>\n\
             \t\t<string>--start-in-tray</string>\n\t</array>"
        ));
        let window = launch_agent(exe, false);
        assert!(window.contains(
            "<array>\n\t\t<string>/Applications/Deckly.app/Contents/MacOS/deckly</string>\n\t</array>"
        ));
        assert!(!window.contains(START_IN_TRAY_ARG));
    }

    #[test]
    fn command_line_is_quoted_and_ends_with_the_flag() {
        let exe = Path::new(r"C:\Program Files\Deckly\deckly.exe");
        assert_eq!(
            command_line(exe, false),
            r#""C:\Program Files\Deckly\deckly.exe""#
        );
        assert_eq!(
            command_line(exe, true),
            r#""C:\Program Files\Deckly\deckly.exe" --start-in-tray"#
        );
    }
}

//! Opening files, applications and URLs on Linux and macOS.

use super::command::split_words;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Hands a URL or file to the desktop's default application. Nothing here
/// runs the text itself, so it is safe for text from plugins.
pub fn open_url(target: &str) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("nothing to open".into());
    }
    let mut command = Command::new(opener());
    command.arg(target);
    launch(command).map_err(|e| format!("cannot open \"{target}\": {e}"))
}

/// Runs what the user wrote in an Open action: a URL, a path to a file,
/// folder or program, or a command line with arguments.
pub fn open(target: &str) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("nothing to open".into());
    }
    if is_url(target) {
        return open_url(target);
    }
    // A whole path first: file names may hold spaces.
    let whole = home_path(target);
    if whole.exists() {
        return open_path(&whole).map_err(|e| format!("cannot open \"{target}\": {e}"));
    }
    let words = split_words(target).ok_or_else(|| format!("unclosed quote in \"{target}\""))?;
    let Some((program, args)) = words.split_first() else {
        return Err("nothing to open".into());
    };
    let mut command = Command::new(home_path(program));
    command.args(args);
    if let Some(home) = home() {
        command.current_dir(home);
    }
    launch(command).map_err(|e| format!("cannot run \"{target}\": {e}"))
}

fn open_path(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    if path.extension().is_some_and(|e| e == "desktop") {
        // Most file managers' handling; xdg-open would show the file's text.
        let mut command = Command::new("gio");
        command.arg("launch").arg(path);
        if launch(command).is_ok() {
            return Ok(());
        }
    }
    if is_program(path) {
        let mut command = Command::new(path);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            command.current_dir(dir);
        }
        match launch(command) {
            // A data file marked executable, as on some shared drives.
            Err(e) if e.raw_os_error() == Some(ENOEXEC) => {}
            result => return result,
        }
    }
    let mut command = Command::new(opener());
    command.arg(path);
    launch(command)
}

/// The error for a file the system cannot run ("exec format error").
const ENOEXEC: i32 = 8;

fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn is_url(target: &str) -> bool {
    if target.starts_with("mailto:") {
        return true;
    }
    target.split_once("://").is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn home_path(text: &str) -> PathBuf {
    match (text.strip_prefix("~/"), home()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(text),
    }
}

/// Starts a program that outlives Deckly and has its own process group, so
/// stopping Deckly from a terminal does not reach it.
fn launch(mut command: Command) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn()?;
    // Collects its exit status; without this it stays a zombie until Deckly exits.
    let _ = std::thread::Builder::new()
        .name("deckly-launched".into())
        .stack_size(64 * 1024)
        .spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deckly-open-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait_for(path: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(path)
                && !text.is_empty()
            {
                return text;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{} was not written", path.display());
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The owner's light switches: an interpreter, a script and arguments
    /// with a space in one of them.
    #[test]
    fn runs_a_command_line_with_quoted_arguments() {
        let dir = scratch("command");
        let out = dir.join("out");
        let tool = script(
            &dir,
            "tool",
            &format!("printf '%s|' \"$@\" > {}", out.display()),
        );
        open(&format!("{} toggle \"RGB Lights\"", tool.display())).unwrap();
        assert_eq!(wait_for(&out), "toggle|RGB Lights|");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runs_a_program_with_a_space_in_its_path_from_its_own_folder() {
        let dir = scratch("spaces");
        let out = dir.join("out");
        let tool = script(&dir, "my tool", &format!("pwd > {}", out.display()));
        open(tool.to_str().unwrap()).unwrap();
        assert_eq!(
            wait_for(&out).trim(),
            dir.canonicalize().unwrap().to_str().unwrap()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reports_what_cannot_start() {
        assert!(open("").is_err());
        assert!(open("/no/such/deckly/program --flag").is_err());
        assert!(open("no-such-deckly-program").is_err());
        assert!(open("sh -c \"unclosed").is_err());
    }

    #[test]
    fn urls_are_told_from_paths() {
        assert!(is_url("https://example.com/a b"));
        assert!(is_url("steam://run/440"));
        assert!(is_url("mailto:someone@example.com"));
        assert!(!is_url("/home/me/file://name"));
        assert!(!is_url("python3 script.py"));
        assert!(!is_url("1a://x"));
    }

    #[test]
    fn program_files_need_an_executable_bit() {
        let dir = scratch("modes");
        let data = dir.join("notes.txt");
        std::fs::write(&data, "text").unwrap();
        assert!(!is_program(&data));
        assert!(!is_program(&dir));
        assert!(is_program(&script(&dir, "run", "true")));
        let _ = std::fs::remove_dir_all(dir);
    }
}

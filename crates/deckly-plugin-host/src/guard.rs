//! The plugin guard: a small process between Deckly and each plugin that
//! makes sure the plugin never outlives Deckly.
//!
//! Plugins are third-party programs and many keep running when their
//! WebSocket closes. The host starts `<deckly> --plugin-guard <plugin
//! command>` with a pipe on the guard's standard input. When Deckly stops the
//! plugin, exits or crashes, the pipe closes and the guard stops the plugin
//! with all its child processes, then exits. When the plugin exits first, the
//! guard exits with its code. The same code runs on every platform; it needs
//! no platform API, only the system's own kill command.

use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;

/// The first argument that makes a Deckly process a plugin guard.
pub const GUARD_ARG: &str = "--plugin-guard";

/// Runs the plugin command in `args` until it exits or standard input
/// closes. Returns the exit code for the guard process.
pub fn run_guard(args: &[OsString]) -> i32 {
    guard(args, std::io::stdin())
}

fn guard(args: &[OsString], mut lifeline: impl Read + Send + 'static) -> i32 {
    let Some((program, rest)) = args.split_first() else {
        eprintln!("[deckly] usage: {GUARD_ARG} <program> [arguments]");
        return 2;
    };
    let mut command = Command::new(program);
    command.args(rest).stdin(Stdio::null());
    crate::hide_console(&mut command);
    // Its own process group, so the whole group can be stopped at once.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[deckly] cannot start {}: {e}", program.to_string_lossy());
            return 127;
        }
    };
    let pid = child.id();
    // `Some(code)` when the plugin exited, `None` when the host let go.
    let (tx, rx) = channel::<Option<i32>>();
    let exited = tx.clone();
    let waiter = std::thread::Builder::new().spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
        let _ = exited.send(Some(code));
    });
    let watcher = std::thread::Builder::new().spawn(move || {
        let mut buf = [0u8; 64];
        while let Ok(n) = lifeline.read(&mut buf) {
            if n == 0 {
                break;
            }
        }
        kill_tree(pid);
        let _ = tx.send(None);
    });
    if waiter.is_err() || watcher.is_err() {
        kill_tree(pid);
        return 1;
    }
    match rx.recv() {
        Ok(Some(code)) => code,
        Ok(None) | Err(_) => 0,
    }
}

/// Stops a process and everything it started.
fn kill_tree(pid: u32) {
    let mut command = if cfg!(windows) {
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut c = Command::new(
            std::path::Path::new(&system)
                .join("System32")
                .join("taskkill.exe"),
        );
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        c
    } else {
        // The plugin leads its own process group; a negative id names it.
        let mut c = Command::new("kill");
        c.args(["-KILL", "--", &format!("-{pid}")]);
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::hide_console(&mut command);
    let _ = command.status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn shell(script: &str) -> Vec<OsString> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), script.into()]
        } else {
            vec!["sh".into(), "-c".into(), script.into()]
        }
    }

    #[test]
    fn passes_the_plugin_exit_code_through() {
        let (reader, _writer) = std::io::pipe().unwrap();
        assert_eq!(guard(&shell("exit 3"), reader), 3);
    }

    #[test]
    fn stops_the_plugin_when_the_host_lets_go() {
        let (reader, writer) = std::io::pipe().unwrap();
        // A plugin that would run for a minute.
        let long = if cfg!(windows) {
            "ping -n 60 127.0.0.1 >NUL"
        } else {
            "sleep 60"
        };
        let start = Instant::now();
        let handle = std::thread::spawn(move || guard(&shell(long), reader));
        std::thread::sleep(Duration::from_millis(300));
        drop(writer);
        // The exit code is the plugin's or 0, whichever the guard saw first.
        handle.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn reports_a_missing_program() {
        let (reader, _writer) = std::io::pipe().unwrap();
        assert_eq!(guard(&["no-such-program-deckly".into()], reader), 127);
        assert_eq!(guard(&[], std::io::empty()), 2);
    }
}

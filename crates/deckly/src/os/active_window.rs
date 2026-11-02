//! Reports which application is in the foreground. Each system signals that
//! its own way; all of them wait for events and never poll. A system that
//! cannot say reports why, and Preferences shows it.
//!
//! An application comes as the names the system gives it: paths, bundle,
//! package or class names, in no special order. An empty list is one that
//! cannot be named, as a window of a process Deckly may not look into.
//!
//! [`watch`] must be called on the main thread and only once: macOS delivers
//! its notifications there.

use std::path::Path;
use std::sync::Arc;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(windows)]
use self::windows as platform;

type Report = Arc<dyn Fn(Result<Vec<String>, String>) + Send + Sync>;

/// Calls `report`, on any thread, when the foreground moves to another
/// application. Deckly's own windows do not count.
pub fn watch(report: impl Fn(Result<Vec<String>, String>) + Send + Sync + 'static) {
    let exe = std::env::current_exe().ok();
    let filtered: Report = Arc::new(move |names| {
        let own = match (&names, &exe) {
            (Ok(names), Some(exe)) => is_own(names, exe),
            _ => false,
        };
        if !own {
            report(names);
        }
    });
    start(filtered);
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn start(report: Report) {
    platform::watch(report);
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn start(report: Report) {
    report(Err(
        "Following the active window is not supported on this system.".into(),
    ));
}

/// Deckly, or the helper that shows plugin pages for it.
fn is_own(names: &[String], exe: &Path) -> bool {
    let helper = exe.with_file_name(if cfg!(windows) {
        "deckly-web.exe"
    } else {
        "deckly-web"
    });
    let same =
        |path: &Path, name: &str| path.to_str().is_some_and(|p| p.eq_ignore_ascii_case(name));
    names
        .iter()
        .any(|name| same(exe, name) || same(&helper, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deckly_and_its_helper_are_its_own_windows() {
        let exe = Path::new("/opt/deckly/deckly");
        let name = |text: &str| vec!["com.example".to_owned(), text.to_owned()];
        assert!(is_own(&name("/opt/deckly/deckly"), exe));
        assert!(is_own(&name("/OPT/deckly/DECKLY"), exe));
        let helper = exe.with_file_name(if cfg!(windows) {
            "deckly-web.exe"
        } else {
            "deckly-web"
        });
        assert!(is_own(&name(helper.to_str().unwrap()), exe));
        assert!(!is_own(&name("/usr/bin/deckly"), exe));
        assert!(!is_own(&name("/opt/deckly/deckly-web-tool"), exe));
        assert!(!is_own(&[], exe));
    }
}

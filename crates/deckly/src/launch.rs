//! How this Deckly was started, and whether it opens its window.
//!
//! Only the login entry written by "Open automatically at login" passes
//! [`START_IN_TRAY_ARG`], and only while "Start in tray at login" is on.
//! Deckly does not guess from the environment: a start from the launcher, a
//! shortcut or a terminal has no such argument and always opens the window.

use std::ffi::OsString;

/// Added to the login entry while "Start in tray at login" is on.
pub const START_IN_TRAY_ARG: &str = "--start-in-tray";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    /// From the launcher, a shortcut, a terminal or another program.
    Manual,
    /// By the login entry, which asks for the tray.
    LoginToTray,
}

impl Launch {
    /// Reads the program's arguments, without the program name.
    pub fn from_args(args: &[OsString]) -> Self {
        if args.iter().any(|a| a == START_IN_TRAY_ARG) {
            Self::LoginToTray
        } else {
            Self::Manual
        }
    }
}

/// Whether Deckly opens its window at start. It stays in the tray only when
/// the login entry asked for that, the setting still asks for it (an entry
/// left over from an older setting cannot hide the window), and a tray is
/// there to bring the window back. `tray_ready` sets up the tray and says
/// whether that worked; it is called only when everything else agrees.
pub fn opens_window(
    launch: Launch,
    start_in_tray: bool,
    tray_ready: impl FnOnce() -> bool,
) -> bool {
    !(launch == Launch::LoginToTray && start_in_tray && tray_ready())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_the_exact_flag_means_a_start_from_the_login_entry() {
        assert_eq!(Launch::from_args(&args(&[])), Launch::Manual);
        assert_eq!(
            Launch::from_args(&args(&["--start-in-tray"])),
            Launch::LoginToTray
        );
        assert_eq!(
            Launch::from_args(&args(&["/home/me/file.txt", "--start-in-tray"])),
            Launch::LoginToTray
        );
        for other in [
            "--start-in-tray=1",
            "--START-IN-TRAY",
            "start-in-tray",
            "--tray",
            "-h",
        ] {
            assert_eq!(
                Launch::from_args(&args(&[other])),
                Launch::Manual,
                "{other}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn arguments_that_are_not_text_are_ignored() {
        use std::os::unix::ffi::OsStringExt;
        let odd = OsString::from_vec(vec![0xff, 0xfe]);
        assert_eq!(Launch::from_args(&[odd]), Launch::Manual);
    }

    #[test]
    fn a_manual_start_opens_the_window_and_leaves_the_tray_alone() {
        for setting in [false, true] {
            let opens = opens_window(Launch::Manual, setting, || {
                panic!("a manual start must not wait for the tray")
            });
            assert!(opens);
        }
    }

    #[test]
    fn the_login_entry_with_the_setting_on_starts_in_the_tray() {
        assert!(!opens_window(Launch::LoginToTray, true, || true));
    }

    #[test]
    fn the_login_entry_with_the_setting_off_opens_the_window() {
        let opens = opens_window(Launch::LoginToTray, false, || {
            panic!("with the setting off the tray does not decide")
        });
        assert!(opens);
    }

    #[test]
    fn without_a_tray_the_window_opens_anyway() {
        assert!(opens_window(Launch::LoginToTray, true, || false));
    }
}

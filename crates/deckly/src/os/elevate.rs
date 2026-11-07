//! Running a package manager with administrator rights, granted by the
//! operating system itself. The system draws its own password window;
//! Deckly never asks for, sees or stores a password.
//!
//! - Linux: `pkexec` (polkit) asks through the desktop's authentication
//!   agent.
//! - Windows: winget asks Windows for UAC itself when a package needs it;
//!   most FFmpeg packages install for the user and need none.
//! - macOS: Homebrew installs as the user and refuses to run as root.
//! - Elsewhere: unsupported.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    Mac,
    Other,
}

impl Os {
    pub const CURRENT: Os = if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Other
    };
}

/// The command line that runs `argv` (program first) with the rights a
/// system-wide install needs on `os`. `pkexec` is where pkexec is, if
/// installed.
pub fn command_line(
    os: Os,
    pkexec: Option<&Path>,
    argv: Vec<OsString>,
) -> Result<Vec<OsString>, String> {
    match os {
        Os::Linux => {
            let pkexec = pkexec.ok_or(
                "pkexec is not installed, so the system cannot ask for administrator \
                 rights. Copy the command or download FFmpeg for Deckly instead.",
            )?;
            let mut line = vec![pkexec.as_os_str().to_owned()];
            line.extend(argv);
            Ok(line)
        }
        Os::Windows | Os::Mac => Ok(argv),
        Os::Other => Err("Installing through the system is not supported here.".into()),
    }
}

/// Where pkexec is installed. Only the system folders count, never a
/// program of that name elsewhere on PATH.
pub fn pkexec() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    [
        "/usr/bin/pkexec",
        "/bin/pkexec",
        "/run/current-system/sw/bin/pkexec",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(os: Os, pkexec: Option<&str>) -> Result<Vec<String>, String> {
        let argv = vec!["apt-get".into(), "install".into()];
        command_line(os, pkexec.map(Path::new), argv).map(|line| {
            line.into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        })
    }

    #[test]
    fn linux_asks_through_pkexec_and_needs_it() {
        assert_eq!(
            line(Os::Linux, Some("/usr/bin/pkexec")).unwrap(),
            ["/usr/bin/pkexec", "apt-get", "install"]
        );
        assert!(line(Os::Linux, None).unwrap_err().contains("pkexec"));
    }

    #[test]
    fn windows_and_macos_run_the_package_manager_as_the_user() {
        assert_eq!(line(Os::Windows, None).unwrap(), ["apt-get", "install"]);
        assert_eq!(line(Os::Mac, Some("/x")).unwrap(), ["apt-get", "install"]);
        assert!(line(Os::Other, None).unwrap_err().contains("not supported"));
    }
}

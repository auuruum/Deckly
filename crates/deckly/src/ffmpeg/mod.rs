//! FFmpeg for video import: where Deckly looks for it, which version it is,
//! and the routes that set it up (see `video_setup` for the window).
//!
//! Lookup order: the path chosen in Preferences, Deckly's own copy in its
//! data folder, then PATH and the folders winget and Homebrew install to.
//! The first FFmpeg 7 or newer wins. FFmpeg always runs as a separate
//! process.

pub mod download;
pub mod pins;
pub mod process;
pub mod system;
pub mod version;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use version::Version;

/// Development switch: ignore FFmpeg on PATH and in package folders.
const FAKE_NO_FFMPEG_ENV: &str = "DECKLY_FAKE_NO_FFMPEG";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROBE_OUTPUT: usize = 64 * 1024;

pub fn program_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// The folder that holds Deckly's own FFmpeg.
pub fn own_dir() -> Option<PathBuf> {
    Some(crate::store::data_dir()?.join("ffmpeg"))
}

/// Where an FFmpeg was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Chosen in Preferences.
    Chosen,
    /// Deckly's own download.
    Deckly,
    /// On PATH or where a package manager installs.
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub version: Version,
    pub source: Source,
}

/// Why video import cannot run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Missing {
    NotFound,
    TooOld { path: PathBuf, version: Version },
    Unusable { path: PathBuf, error: String },
}

impl Missing {
    pub fn message(&self) -> String {
        match self {
            Missing::NotFound => format!(
                "Video import needs FFmpeg {} or newer, and Deckly found none.",
                version::MINIMUM_MAJOR
            ),
            Missing::TooOld { path, version } => format!(
                "Deckly found FFmpeg {version} at {}. Video import needs FFmpeg {} or newer.",
                path.display(),
                version::MINIMUM_MAJOR
            ),
            Missing::Unusable { path, error } => {
                format!("The FFmpeg at {} cannot be used: {error}", path.display())
            }
        }
    }
}

/// Where to look, in order, without duplicates. Only absolute paths count.
pub fn candidates(
    chosen: Option<&Path>,
    own: Option<PathBuf>,
    path_var: Option<&OsStr>,
    extra: Vec<PathBuf>,
) -> Vec<(Source, PathBuf)> {
    let name = program_name();
    let on_path: Vec<PathBuf> = path_var
        .map(|p| std::env::split_paths(p).map(|dir| dir.join(name)).collect())
        .unwrap_or_default();
    let all = chosen
        .map(|p| (Source::Chosen, p.to_path_buf()))
        .into_iter()
        .chain(own.map(|p| (Source::Deckly, p)))
        .chain(
            on_path
                .into_iter()
                .chain(extra)
                .map(|p| (Source::System, p)),
        );
    let mut list: Vec<(Source, PathBuf)> = Vec::new();
    for (source, path) in all {
        if path.is_absolute() && !list.iter().any(|(_, p)| *p == path) {
            list.push((source, path));
        }
    }
    list
}

/// The first candidate that exists and is FFmpeg 7 or newer. Otherwise the
/// first problem found, so the window can explain it.
pub fn locate_in(
    candidates: Vec<(Source, PathBuf)>,
    exists: &dyn Fn(&Path) -> bool,
    probe: &dyn Fn(&Path) -> Result<Version, String>,
) -> Result<Found, Missing> {
    let mut problem = None;
    for (source, path) in candidates {
        if !exists(&path) {
            continue;
        }
        match probe(&path) {
            Ok(version) if version.supported() => {
                return Ok(Found {
                    path,
                    version,
                    source,
                });
            }
            Ok(version) => {
                problem.get_or_insert(Missing::TooOld { path, version });
            }
            Err(error) => {
                problem.get_or_insert(Missing::Unusable { path, error });
            }
        }
    }
    Err(problem.unwrap_or(Missing::NotFound))
}

/// Folders where a package manager puts FFmpeg that may be missing from the
/// PATH of an app started from the desktop.
fn package_folders() -> Vec<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(|local| vec![PathBuf::from(local).join(r"Microsoft\WinGet\Links\ffmpeg.exe")])
            .unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/opt/homebrew/bin/ffmpeg"),
            PathBuf::from("/usr/local/bin/ffmpeg"),
        ]
    } else {
        Vec::new()
    }
}

/// Finds FFmpeg on this computer. Starts processes; call from a worker
/// thread.
pub fn locate(chosen: Option<&Path>) -> Result<Found, Missing> {
    let own = own_dir().map(|dir| dir.join(program_name()));
    // Development switch: act as if the system had no FFmpeg, to see the
    // setup window. A chosen FFmpeg and Deckly's own copy still count.
    let (path_var, folders) = if std::env::var_os(FAKE_NO_FFMPEG_ENV).is_some() {
        (None, Vec::new())
    } else {
        (std::env::var_os("PATH"), package_folders())
    };
    let list = candidates(chosen, own, path_var.as_deref(), folders);
    locate_in(list, &|p| p.is_file(), &probe)
}

/// Runs `ffmpeg -version` and reads its version.
pub fn probe(exe: &Path) -> Result<Version, String> {
    let mut command = Command::new(exe);
    command.arg("-version").env_remove("FFREPORT");
    let out = process::output(command, PROBE_TIMEOUT, MAX_PROBE_OUTPUT)?;
    if !out.status.success() {
        return Err("it did not run (ffmpeg -version failed).".into());
    }
    version::from_ffmpeg_output(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "it does not report an FFmpeg version.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn v(major: u32) -> Version {
        Version {
            major,
            minor: 0,
            release: true,
        }
    }

    fn abs(path: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"C:{path}"))
        } else {
            PathBuf::from(path)
        }
    }

    #[test]
    fn candidates_follow_the_lookup_order() {
        let path_var =
            std::env::join_paths([abs("/usr/bin"), PathBuf::from("relative"), abs("/opt/bin")])
                .unwrap();
        let list = candidates(
            Some(&abs("/chosen/ffmpeg")),
            Some(abs("/data/ffmpeg/ffmpeg")),
            Some(&path_var),
            vec![
                abs("/opt/homebrew/bin/ffmpeg"),
                abs("/usr/bin").join(program_name()),
            ],
        );
        let expected = [
            (Source::Chosen, abs("/chosen/ffmpeg")),
            (Source::Deckly, abs("/data/ffmpeg/ffmpeg")),
            (Source::System, abs("/usr/bin").join(program_name())),
            (Source::System, abs("/opt/bin").join(program_name())),
            (Source::System, abs("/opt/homebrew/bin/ffmpeg")),
        ];
        assert_eq!(list, expected);
        // A relative choice is ignored rather than resolved against the
        // current folder.
        let list = candidates(Some(Path::new("ffmpeg")), None, None, Vec::new());
        assert!(list.is_empty());
    }

    #[test]
    fn the_first_ffmpeg_7_wins_and_problems_are_explained() {
        let versions: HashMap<PathBuf, Result<Version, String>> = [
            (abs("/chosen"), Ok(v(6))),
            (abs("/own"), Ok(v(9))),
            (abs("/usr/bin/ffmpeg"), Ok(v(7))),
            (abs("/broken"), Err("it did not run".to_owned())),
        ]
        .into_iter()
        .collect();
        let probe = |p: &Path| versions.get(p).cloned().unwrap_or(Err("no".into()));
        let exists = |p: &Path| versions.contains_key(p);
        let list = |paths: &[&str]| -> Vec<(Source, PathBuf)> {
            paths.iter().map(|p| (Source::System, abs(p))).collect()
        };

        let found = locate_in(
            list(&["/chosen", "/own", "/usr/bin/ffmpeg"]),
            &exists,
            &probe,
        )
        .unwrap();
        assert_eq!((found.path, found.version), (abs("/own"), v(9)));

        // Missing files are skipped.
        let found = locate_in(list(&["/gone", "/usr/bin/ffmpeg"]), &exists, &probe).unwrap();
        assert_eq!(found.path, abs("/usr/bin/ffmpeg"));

        // An old FFmpeg is reported when nothing newer exists.
        let error = locate_in(list(&["/gone", "/chosen", "/broken"]), &exists, &probe).unwrap_err();
        assert_eq!(
            error,
            Missing::TooOld {
                path: abs("/chosen"),
                version: v(6)
            }
        );
        assert!(
            error.message().contains("FFmpeg 6.0"),
            "{}",
            error.message()
        );

        let error = locate_in(list(&["/broken"]), &exists, &probe).unwrap_err();
        assert!(matches!(error, Missing::Unusable { .. }));
        assert_eq!(
            locate_in(list(&["/gone"]), &exists, &probe),
            Err(Missing::NotFound)
        );
    }

    #[cfg(unix)]
    #[test]
    fn probing_reads_the_version_of_a_fake_ffmpeg() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, script: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let good = write("good", "echo 'ffmpeg version 7.1.2 Copyright'");
        assert_eq!(probe(&good).unwrap().major, 7);
        let other = write("other", "echo hello");
        assert!(probe(&other).unwrap_err().contains("version"));
        let failing = write("failing", "exit 1");
        assert!(probe(&failing).unwrap_err().contains("did not run"));
        assert!(probe(&dir.path().join("missing")).is_err());
    }
}

//! Developer commands, run through the `cargo dev` alias:
//!
//! - `cargo dev`: build both application processes, then start the editor.
//! - `cargo dev dist`: build the release archive for one target.
//! - `cargo dev release-files`: write the Homebrew formula and AUR package
//!   for a release from its checksums.

mod dist;
mod licenses;
mod release_files;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).peekable();
    let result = match args.peek().and_then(|a| a.to_str()) {
        Some("dist") => dist::run(args.skip(1)).map(|()| true),
        Some("release-files") => release_files::run(args.skip(1)).map(|()| true),
        _ => run(args),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("deckly dev: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root.
fn root() -> Result<&'static Path, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "cannot find the workspace".to_owned())
}

/// The Cargo that started this command.
fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// The value after an option such as `--target`.
fn value(name: &str, args: &mut impl Iterator<Item = OsString>) -> Result<String, String> {
    args.next()
        .and_then(|v| v.into_string().ok())
        .filter(|v| !v.is_empty() && !v.starts_with("--"))
        .ok_or_else(|| format!("{name} needs a value"))
}

/// A path value after an option such as `--out`.
fn path_value(name: &str, args: &mut impl Iterator<Item = OsString>) -> Result<PathBuf, String> {
    value(name, args).map(PathBuf::from)
}

fn run(args: impl IntoIterator<Item = OsString>) -> Result<bool, String> {
    let options = Options::parse(args)?;
    if options.help {
        println!(
            "cargo dev [--release] [--no-run]\n\
             Builds the workspace, including deckly-web, then starts Deckly.\n\
             --release  Build and run optimized binaries.\n\
             --no-run   Build without starting the editor or plugins.\n\
             \n\
             cargo dev dist [--target <triple>] [--out <dir>]\n\
             Builds the release archive for one target (default: this computer).\n\
             \n\
             cargo dev release-files --tag <vX.Y.Z> --checksums <SHA256SUMS> [--out <dir>]\n\
             Writes the Homebrew formula and the AUR package for a release."
        );
        return Ok(true);
    }
    let root = root()?;
    let cargo = cargo();
    let profile = options.release.then_some("--release");
    let build = Command::new(&cargo)
        .args(["build", "--workspace"])
        .args(profile)
        .current_dir(root)
        .status()
        .map_err(|e| format!("cannot build the workspace: {e}"))?;
    if !build.success() || options.no_run {
        return Ok(build.success());
    }
    Command::new(cargo)
        .args(["run", "--package", "deckly"])
        .args(profile)
        .current_dir(root)
        .status()
        .map(|s| s.success())
        .map_err(|e| format!("cannot start Deckly: {e}"))
}

struct Options {
    release: bool,
    no_run: bool,
    help: bool,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self {
            // Also makes plain `cargo run --release` use the release profile.
            release: !cfg!(debug_assertions),
            no_run: false,
            help: false,
        };
        for arg in args {
            match arg.to_str() {
                Some("--release") => options.release = true,
                Some("--no-run") => options.no_run = true,
                Some("--help" | "-h") => options.help = true,
                _ => {
                    return Err(format!(
                        "unknown option {}; use --help",
                        arg.to_string_lossy()
                    ));
                }
            }
        }
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_build_only_and_release_but_rejects_unknown_options() {
        let options = Options::parse(["--release", "--no-run"].map(OsString::from)).unwrap();
        assert!(options.release && options.no_run && !options.help);
        assert!(Options::parse([OsString::from("--help")]).unwrap().help);
        assert!(Options::parse([OsString::from("--relase")]).is_err());
    }

    #[test]
    fn option_values_must_be_present() {
        let mut args = [OsString::from("x86_64-pc-windows-msvc")].into_iter();
        assert_eq!(
            value("--target", &mut args).unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert!(value("--target", &mut std::iter::empty()).is_err());
        let mut flag = [OsString::from("--out")].into_iter();
        assert!(value("--target", &mut flag).is_err());
    }
}

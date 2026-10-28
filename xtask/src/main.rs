//! Build both application processes before starting the editor (`cargo dev`).

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("deckly dev: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool, String> {
    let options = Options::parse(std::env::args_os().skip(1))?;
    if options.help {
        println!(
            "cargo dev [--release] [--no-run]\n\
             Builds the workspace, including deckly-web, then starts Deckly.\n\
             --release  Build and run optimized binaries.\n\
             --no-run   Build without starting the editor or plugins."
        );
        return Ok(true);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("cannot find the workspace")?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
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
}

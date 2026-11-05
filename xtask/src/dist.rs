//! `cargo dev dist`: builds the release binaries for one target and packs
//! them, with the license texts and desktop files, into the archive a
//! release ships.
//!
//! Archive names carry no version (`deckly-<target>.tar.gz`, `.zip` on
//! Windows), so `releases/latest/download/<name>` always points at the
//! newest release. Each archive holds one folder of the same name.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{cargo, licenses, path_value, root, value};

/// The operating systems a release is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    pub fn of(target: &str) -> Result<Self, String> {
        if target.contains("-linux-") {
            Ok(Self::Linux)
        } else if target.contains("-apple-darwin") {
            Ok(Self::Mac)
        } else if target.contains("-windows-") {
            Ok(Self::Windows)
        } else {
            Err(format!("no release packaging for target {target}"))
        }
    }

    fn exe(self, name: &str) -> String {
        match self {
            Self::Windows => format!("{name}.exe"),
            Self::Linux | Self::Mac => name.to_owned(),
        }
    }
}

/// The archive file name for a target.
pub fn archive_name(target: &str) -> Result<String, String> {
    let ext = match Os::of(target)? {
        Os::Windows => "zip",
        Os::Linux | Os::Mac => "tar.gz",
    };
    Ok(format!("deckly-{target}.{ext}"))
}

pub fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let root = root()?;
    let mut target = None;
    let mut out = None;
    let mut setup = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--target") => target = Some(value("--target", &mut args)?),
            Some("--out") => out = Some(path_value("--out", &mut args)?),
            Some("--setup") => setup = true,
            _ => {
                return Err(format!(
                    "unknown dist option {}; use cargo dev --help",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    let target = match target {
        Some(target) => target,
        None => host_target()?,
    };
    let os = Os::of(&target)?;
    if setup && os != Os::Windows {
        return Err("--setup is for Windows targets only".to_owned());
    }
    let out = out.unwrap_or_else(|| root.join("dist"));

    build(root, &target)?;
    let bin = root.join("target").join(&target).join("release");

    let stem = format!("deckly-{target}");
    let stage = out.join(&stem);
    if stage.exists() {
        fs::remove_dir_all(&stage).map_err(|e| format!("cannot clear {}: {e}", stage.display()))?;
    }
    let version = env!("CARGO_PKG_VERSION");
    let program_dir = match os {
        Os::Mac => {
            let contents = stage.join("Deckly.app").join("Contents");
            let plist =
                include_str!("../../packaging/macos/Info.plist.in").replace("@VERSION@", version);
            write(&contents.join("Info.plist"), plist.as_bytes())?;
            contents.join("MacOS")
        }
        Os::Linux | Os::Windows => stage.clone(),
    };
    for name in ["deckly", "deckly-web"] {
        let exe = os.exe(name);
        copy(&bin.join(&exe), &program_dir.join(&exe))?;
    }
    if os == Os::Linux {
        let assets = root.join("assets");
        copy(
            &assets.join("linux/deckly.desktop"),
            &stage.join("share/applications/deckly.desktop"),
        )?;
        copy(
            &assets.join("logo/deckly.svg"),
            &stage.join("share/icons/hicolor/scalable/apps/deckly.svg"),
        )?;
        copy(
            &assets.join("linux/60-deckly.rules"),
            &stage.join("udev/60-deckly.rules"),
        )?;
    }
    if os == Os::Windows {
        // The Start menu entry and the setup take their icon from here.
        copy(
            &root.join("assets/windows/deckly.ico"),
            &stage.join("deckly.ico"),
        )?;
    }
    copy(&root.join("LICENSE"), &stage.join("LICENSE"))?;
    copy(&root.join("README.md"), &stage.join("README.md"))?;
    let notices = licenses::third_party(root, &target)?;
    write(&stage.join("THIRD-PARTY-LICENSES.txt"), notices.as_bytes())?;
    if os == Os::Mac && cfg!(target_os = "macos") {
        // Ad-hoc signature: free, lets the bundle run on Apple Silicon, but
        // is not a Developer ID signature, so Gatekeeper still asks.
        status(
            Command::new("codesign")
                .args(["--force", "--deep", "--sign", "-"])
                .arg(stage.join("Deckly.app")),
            "codesign",
        )?;
    }

    let archive = out.join(archive_name(&target)?);
    if archive.exists() {
        fs::remove_file(&archive)
            .map_err(|e| format!("cannot replace {}: {e}", archive.display()))?;
    }
    pack(os, &out, &stem, &archive)?;
    println!("{}", archive.display());
    if setup {
        println!("{}", windows_setup(root, &out, &stage, version)?.display());
    }
    Ok(())
}

/// The setup program's file name; versionless like the archives.
pub const WINDOWS_SETUP: &str = "deckly-setup-windows-x64.exe";

/// Builds the Windows setup from the staged folder with Inno Setup's
/// compiler: `DECKLY_ISCC`, else the default install folder, else `ISCC` on
/// `PATH`.
fn windows_setup(root: &Path, out: &Path, stage: &Path, version: &str) -> Result<PathBuf, String> {
    let iscc = std::env::var_os("DECKLY_ISCC")
        .map(PathBuf::from)
        .or_else(|| {
            [
                r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
                r"C:\Program Files\Inno Setup 6\ISCC.exe",
            ]
            .map(PathBuf::from)
            .into_iter()
            .find(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("ISCC"));
    let setup = out.join(WINDOWS_SETUP);
    if setup.exists() {
        fs::remove_file(&setup).map_err(|e| format!("cannot replace {}: {e}", setup.display()))?;
    }
    let define = |name: &str, value: &Path| {
        let mut arg = OsString::from(format!("/D{name}="));
        arg.push(value);
        arg
    };
    status(
        Command::new(&iscc)
            .arg("/Q")
            .arg(format!("/DAppVersion={version}"))
            .arg(define("SourceDir", stage))
            .arg(define("OutputDir", out))
            .arg(root.join("packaging/windows/deckly.iss")),
        "Inno Setup (install it, or set DECKLY_ISCC to ISCC.exe)",
    )?;
    if setup.is_file() {
        Ok(setup)
    } else {
        Err(format!("Inno Setup did not write {}", setup.display()))
    }
}

/// The target triple of this computer, from `rustc -vV`.
fn host_target() -> Result<String, String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("cannot run rustc: {e}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| "rustc did not report the host target".to_owned())
}

fn build(root: &Path, target: &str) -> Result<(), String> {
    status(
        Command::new(cargo())
            .args(["build", "--release", "--locked"])
            .args(["--package", "deckly", "--package", "deckly-web"])
            .args(["--target", target])
            .current_dir(root),
        "cargo build",
    )
}

/// Packs `out/<stem>` into `archive`: a zip for Windows, tar.gz elsewhere.
fn pack(os: Os, out: &Path, stem: &str, archive: &Path) -> Result<(), String> {
    let mut command = match os {
        Os::Windows if cfg!(windows) => {
            // The bsdtar that ships with Windows 10 and later writes zips;
            // a GNU tar earlier on PATH (Git for Windows) cannot.
            let system = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let mut tar = Command::new(system.join(r"System32\tar.exe"));
            tar.arg("-a").arg("-cf").arg(archive).arg(stem);
            tar
        }
        Os::Windows => {
            let mut zip = Command::new("zip");
            zip.arg("-qr").arg(archive).arg(stem);
            zip
        }
        Os::Linux | Os::Mac => {
            let mut tar = Command::new("tar");
            // Keeps macOS tar from adding `._` resource files.
            tar.env("COPYFILE_DISABLE", "1");
            tar.arg("-czf").arg(archive).arg(stem);
            tar
        }
    };
    status(command.current_dir(out), "the archiver")
}

fn status(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

fn create_parent(path: &Path) -> Result<(), String> {
    match path.parent() {
        Some(dir) => {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
        }
        None => Ok(()),
    }
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    create_parent(to)?;
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    create_parent(path)?;
    fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_are_named_after_the_target_only() {
        assert_eq!(
            archive_name("x86_64-unknown-linux-gnu").unwrap(),
            "deckly-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            archive_name("aarch64-apple-darwin").unwrap(),
            "deckly-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            archive_name("x86_64-pc-windows-msvc").unwrap(),
            "deckly-x86_64-pc-windows-msvc.zip"
        );
        assert!(archive_name("wasm32-unknown-unknown").is_err());
    }

    #[test]
    fn inno_setup_writes_the_name_releases_use() {
        let script = include_str!("../../packaging/windows/deckly.iss");
        let base = WINDOWS_SETUP.trim_end_matches(".exe");
        let line = format!("OutputBaseFilename={base}");
        assert!(script.lines().any(|l| l.trim_end() == line));
    }

    #[test]
    fn info_plist_template_takes_the_version() {
        let plist =
            include_str!("../../packaging/macos/Info.plist.in").replace("@VERSION@", "1.2.3");
        assert!(plist.contains("<string>1.2.3</string>"));
        assert!(!plist.contains('@'));
    }
}

//! The Node.js runtime for Node plugins.
//!
//! A plugin's manifest names the Node major version it was built and tested
//! with (`Nodejs.Version`), and plugins can break on another major, so each
//! plugin runs on the major it asks for: a `node` on `PATH` of that major,
//! else an official build Deckly downloaded from nodejs.org into its
//! runtimes folder. Downloads use the system's `curl` and `tar` (present on
//! Windows 10 and later, macOS and Linux) and are checked against the
//! release's SHA-256 list before anything is unpacked.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The major used when a Node plugin's manifest names none.
pub const DEFAULT_MAJOR: &str = "20";
const MAX_SUMS_BYTES: u64 = 256 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;

fn exe_name() -> &'static str {
    if cfg!(windows) { "node.exe" } else { "node" }
}

/// The major version a manifest asks for ("20", "20.x" or "20.19.0" all
/// mean 20). `None` when it is not a plain number.
pub fn major(requested: &str) -> Option<String> {
    let digits: String = requested
        .trim()
        .trim_start_matches('v')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (1..=3).contains(&digits.len()).then_some(digits)
}

/// Where Deckly keeps the Node build for a major.
fn installed(runtimes: &Path, major: &str) -> PathBuf {
    runtimes.join(format!("node-{major}")).join(exe_name())
}

/// A Node of the given major that is already on this machine.
pub fn find(runtimes: &Path, major: &str) -> Option<PathBuf> {
    let own = installed(runtimes, major);
    if own.is_file() {
        return Some(own);
    }
    let on_path = on_path()?;
    (path_major(&on_path).as_deref() == Some(major)).then_some(on_path)
}

/// `node` on `PATH`, of any version.
pub fn on_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(exe_name()))
        .find(|p| p.is_file())
}

fn path_major(node: &Path) -> Option<String> {
    let mut c = Command::new(node);
    c.arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    crate::hide_console(&mut c);
    let out = c.output().ok()?;
    major(&String::from_utf8_lossy(&out.stdout))
}

/// The release asset for this platform: `(os, arch, extension)` as
/// nodejs.org names them.
fn platform() -> Result<(&'static str, &'static str, &'static str), String> {
    let (os, ext) = match std::env::consts::OS {
        "windows" => ("win", "zip"),
        "macos" => ("darwin", "tar.gz"),
        "linux" => ("linux", "tar.gz"),
        other => return Err(format!("no Node.js download for {other}")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => return Err(format!("no Node.js download for {other}")),
    };
    Ok((os, arch, ext))
}

/// Finds this platform's archive in a release's `SHASUMS256.txt`: its
/// SHA-256 and file name.
fn asset(sums: &str, os: &str, arch: &str, ext: &str) -> Option<(String, String)> {
    let suffix = format!("-{os}-{arch}.{ext}");
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim();
        let ok = hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
            && name.starts_with("node-v")
            && name.ends_with(&suffix)
            && !name.contains(['/', '\\']);
        ok.then(|| (hash.to_ascii_lowercase(), name.to_owned()))
    })
}

/// Downloads the newest official build of a major into the runtimes folder
/// and returns its `node`. Blocks for the download; run it off the plugin
/// thread.
pub fn install(runtimes: &Path, major: &str) -> Result<PathBuf, String> {
    if self::major(major).as_deref() != Some(major) {
        return Err(format!("bad Node.js version {major}"));
    }
    let (os, arch, ext) = platform()?;
    std::fs::create_dir_all(runtimes).map_err(|e| e.to_string())?;
    let work = runtimes.join(format!(".download-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = download_and_unpack(&work, runtimes, major, (os, arch, ext));
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn download_and_unpack(
    work: &Path,
    runtimes: &Path,
    major: &str,
    (os, arch, ext): (&str, &str, &str),
) -> Result<PathBuf, String> {
    let base = format!("https://nodejs.org/dist/latest-v{major}.x/");
    let sums_file = work.join("SHASUMS256.txt");
    fetch(&format!("{base}SHASUMS256.txt"), &sums_file, MAX_SUMS_BYTES)?;
    let sums = std::fs::read_to_string(&sums_file).map_err(|e| e.to_string())?;
    let (hash, name) = asset(&sums, os, arch, ext)
        .ok_or_else(|| format!("Node.js {major} has no build for {os}-{arch}"))?;
    let archive = work.join(&name);
    fetch(&format!("{base}{name}"), &archive, MAX_ARCHIVE_BYTES)?;
    if sha256_file(&archive)? != hash {
        return Err("the Node.js download does not match its checksum".into());
    }
    // Only the node program is unpacked.
    let stem = name.trim_end_matches(&format!(".{ext}"));
    let member = if cfg!(windows) {
        format!("{stem}/node.exe")
    } else {
        format!("{stem}/bin/node")
    };
    let mut tar = system_tool("tar");
    tar.arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(work)
        .arg(&member);
    run(tar, "tar")?;
    let unpacked = work.join(&member);
    let target = installed(runtimes, major);
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&unpacked, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

fn fetch(url: &str, out: &Path, max_bytes: u64) -> Result<(), String> {
    let mut curl = system_tool("curl");
    curl.args(["-fsSL", "--proto", "=https", "--max-filesize"])
        .arg(max_bytes.to_string())
        .arg("-o")
        .arg(out)
        .arg(url);
    run(curl, "curl")
}

/// A tool that ships with the system; on Windows it is taken from System32
/// so a program of the same name elsewhere on `PATH` is not used.
fn system_tool(name: &str) -> Command {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Command::new(
            Path::new(&root)
                .join("System32")
                .join(format!("{name}.exe")),
        )
    } else {
        Command::new(name)
    }
}

fn run(mut command: Command, name: &str) -> Result<(), String> {
    command.stdin(Stdio::null()).stdout(Stdio::null());
    crate::hide_console(&mut command);
    let out = command
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{name} failed: {}", err.trim()))
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majors_are_plain_numbers() {
        assert_eq!(major("20").as_deref(), Some("20"));
        assert_eq!(major("v22.23.4\n").as_deref(), Some("22"));
        assert_eq!(major("24.x").as_deref(), Some("24"));
        assert_eq!(major("../20"), None);
        assert_eq!(major(""), None);
        assert_eq!(major("12345"), None);
    }

    #[test]
    fn finds_the_platform_asset() {
        let h = "a".repeat(64);
        let sums = format!(
            "{h}  node-v20.19.5-darwin-arm64.tar.gz\n\
             {h}  node-v20.19.5-linux-x64.tar.gz\n\
             {h}  node-v20.19.5-win-x64.zip\n\
             {h}  node-v20.19.5-win-x64.7z\n\
             {h}  ../evil-win-arm64.zip\n\
             short  node-v20.19.5-win-arm64.zip\n"
        );
        assert_eq!(
            asset(&sums, "win", "x64", "zip"),
            Some((h.clone(), "node-v20.19.5-win-x64.zip".into()))
        );
        assert_eq!(
            asset(&sums, "linux", "x64", "tar.gz").map(|a| a.1),
            Some("node-v20.19.5-linux-x64.tar.gz".into())
        );
        assert_eq!(asset(&sums, "win", "arm64", "zip"), None);
        assert_eq!(asset(&sums, "linux", "arm64", "tar.gz"), None);
    }

    #[test]
    fn hashes_files() {
        let path = std::env::temp_dir().join(format!("deckly-sha-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn refuses_bad_versions_before_downloading() {
        let dir = std::env::temp_dir().join("deckly-node-bad");
        assert!(install(&dir, "20/../x").is_err());
        assert!(!dir.exists());
    }
}

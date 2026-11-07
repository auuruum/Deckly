//! "Download for Deckly": fetches a pinned FFmpeg build into Deckly's data
//! folder, checks its size and SHA-256, and unpacks only the `ffmpeg`
//! program. Needs no administrator rights. Runs only after the user clicks
//! Download, on a worker thread, and can be cancelled.
//!
//! Like Deckly's own updates, the download uses the system's `curl`, and on
//! Linux the system's `tar` unpacks the `.tar.xz`.

use super::pins::{Archive, Pin};
use super::process::Reporter;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// No pinned download may be larger.
pub const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;
/// The unpacked program may not be larger.
pub const MAX_PROGRAM: u64 = 256 * 1024 * 1024;
/// A download that takes longer is given up.
const MAX_TIME: Duration = Duration::from_secs(60 * 60);
const POLL: Duration = Duration::from_millis(200);

/// Downloads, checks and unpacks `pin` into `dir`, which then holds the
/// program. Returns the program's path.
pub fn install(pin: &Pin, dir: &Path, reporter: &Reporter<'_>) -> Result<PathBuf, String> {
    if pin.size > MAX_DOWNLOAD {
        return Err("The pinned download is larger than Deckly allows.".into());
    }
    let work = dir.join("download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("Cannot create {}: {e}", work.display()))?;
    let result = install_in(pin, dir, &work, reporter);
    // The archive is only needed while it is unpacked.
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn install_in(
    pin: &Pin,
    dir: &Path,
    work: &Path,
    reporter: &Reporter<'_>,
) -> Result<PathBuf, String> {
    let archive = work.join(match pin.archive {
        Archive::Zip => "ffmpeg.zip",
        Archive::TarXz => "ffmpeg.tar.xz",
    });
    reporter.note(&format!("Downloading {}", pin.url));
    fetch(pin.url, &archive, pin.size, reporter)?;
    reporter.note("Checking the SHA-256 of the download…");
    verify(&archive, pin.size, pin.sha256)?;
    if reporter.cancelled() {
        return Err("Cancelled.".into());
    }
    reporter.note("Unpacking ffmpeg…");
    let staged = work.join(super::program_name());
    match pin.archive {
        Archive::Zip => extract_zip(&archive, pin.member, &staged, MAX_PROGRAM)?,
        Archive::TarXz => extract_tar_xz(&archive, pin.member, work, &staged, MAX_PROGRAM)?,
    }
    set_executable(&staged)?;
    let target = dir.join(super::program_name());
    std::fs::rename(&staged, &target)
        .map_err(|e| format!("Cannot put ffmpeg in {}: {e}", dir.display()))?;
    Ok(target)
}

/// Fetches `url` into `out`, never more than `size` bytes, and reports the
/// progress. Cancelling stops curl.
fn fetch(url: &str, out: &Path, size: u64, reporter: &Reporter<'_>) -> Result<(), String> {
    let mut curl = crate::update::system_tool("curl");
    curl.args(["--proto", "=https", "--proto-redir", "=https", "-fsSL"])
        .args(["--max-time", &MAX_TIME.as_secs().to_string()])
        .args(["--max-filesize", &size.to_string()])
        .arg("-o")
        .arg(out)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    super::process::no_window(&mut curl);
    let mut child = curl
        .spawn()
        .map_err(|e| format!("Cannot run curl to download FFmpeg: {e}"))?;
    let started = Instant::now();
    let status = loop {
        let done = out.metadata().map_or(0, |m| m.len());
        reporter.progress(done.min(size), size);
        let stop = if reporter.cancelled() {
            Some("Cancelled.".to_owned())
        } else if done > size {
            Some("The download is larger than expected; it was stopped.".to_owned())
        } else if started.elapsed() > MAX_TIME + Duration::from_secs(60) {
            Some("The download took too long; it was stopped.".to_owned())
        } else {
            None
        };
        if let Some(error) = stop {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Cannot wait for curl: {e}"));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut error = String::new();
    if let Some(stderr) = child.stderr.take() {
        let _ = stderr.take(4096).read_to_string(&mut error);
    }
    let error: String = error.trim().chars().take(300).collect();
    Err(if error.is_empty() {
        "The download failed.".into()
    } else {
        format!("The download failed: {error}")
    })
}

/// Checks that `path` is exactly `size` bytes with SHA-256 `sha256`.
pub fn verify(path: &Path, size: u64, sha256: &str) -> Result<(), String> {
    let len = path
        .metadata()
        .map_err(|e| format!("Cannot read the download: {e}"))?
        .len();
    if len != size {
        return Err(format!(
            "The download has {len} bytes instead of {size}; it was not used."
        ));
    }
    if sha256_file(path)? != sha256.to_ascii_lowercase() {
        return Err("The download does not match its pinned SHA-256; it was not used.".into());
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("Cannot read the download: {e}"))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(|e| format!("Cannot read the download: {e}"))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Copies at most `limit` bytes of `from` into a new file `to`.
fn copy_bounded(from: &mut dyn Read, to: &Path, limit: u64) -> Result<(), String> {
    let write_error = |e: io::Error| format!("Cannot write {}: {e}", to.display());
    let mut out = File::create(to).map_err(write_error)?;
    let copied = io::copy(&mut from.take(limit + 1), &mut out).map_err(write_error)?;
    if copied > limit {
        drop(out);
        let _ = std::fs::remove_file(to);
        return Err("The unpacked ffmpeg is larger than Deckly allows.".into());
    }
    Ok(())
}

/// Unpacks the file `member` of a zip archive to `to`.
pub fn extract_zip(archive: &Path, member: &str, to: &Path, limit: u64) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| format!("Cannot read the download: {e}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read the download: {e}"))?;
    let mut entry = zip
        .by_name(member)
        .map_err(|_| format!("The download holds no {member}."))?;
    if !entry.is_file() || entry.size() > limit {
        return Err("The ffmpeg in the download is not a usable file.".into());
    }
    copy_bounded(&mut entry, to, limit)
}

/// Unpacks the file `member` of a `.tar.xz` with the system's `tar` into
/// `work`, then moves it to `to`.
fn extract_tar_xz(
    archive: &Path,
    member: &str,
    work: &Path,
    to: &Path,
    limit: u64,
) -> Result<(), String> {
    if member.starts_with('/') || member.split('/').any(|part| part == "..") {
        return Err("The pinned archive path is not relative.".into());
    }
    let mut tar = crate::update::system_tool("tar");
    tar.arg("-xJf").arg(archive).arg("-C").arg(work).arg(member);
    let out = super::process::output(tar, Duration::from_secs(600), 0)?;
    if !out.status.success() {
        return Err("Cannot unpack the download with tar (it needs xz support).".into());
    }
    let unpacked = work.join(member);
    let info = unpacked
        .symlink_metadata()
        .map_err(|_| format!("The download holds no {member}."))?;
    if !info.is_file() || info.len() > limit {
        return Err("The ffmpeg in the download is not a usable file.".into());
    }
    std::fs::rename(&unpacked, to).map_err(|e| format!("Cannot move ffmpeg: {e}"))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("Cannot make ffmpeg executable: {e}"))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn verification_accepts_the_pinned_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256_file(&path).unwrap(), ABC_SHA256);
        assert!(verify(&path, 3, ABC_SHA256).is_ok());
        assert!(verify(&path, 3, &ABC_SHA256.to_ascii_uppercase()).is_ok());
        let bad = format!("{}0", &ABC_SHA256[..63]);
        assert!(verify(&path, 3, &bad).unwrap_err().contains("SHA-256"));
        assert!(
            verify(&path, 4, ABC_SHA256)
                .unwrap_err()
                .contains("3 bytes")
        );
        assert!(verify(&dir.path().join("missing"), 3, ABC_SHA256).is_err());
    }

    fn zip_with(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn only_the_named_member_is_unpacked_within_the_limit() {
        let zip = zip_with(&[
            ("top/bin/ffplay.exe", b"player"),
            ("top/bin/ffmpeg.exe", b"program"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ffmpeg.exe");
        extract_zip(zip.path(), "top/bin/ffmpeg.exe", &out, 100).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"program");
        assert!(!dir.path().join("ffplay.exe").exists());

        let error = extract_zip(zip.path(), "top/bin/ffmpeg.exe", &out, 3).unwrap_err();
        assert!(error.contains("not a usable file"), "{error}");
        let error = extract_zip(zip.path(), "other/ffmpeg.exe", &out, 100).unwrap_err();
        assert!(error.contains("holds no"), "{error}");
        let garbage = dir.path().join("garbage.zip");
        std::fs::write(&garbage, b"not a zip").unwrap();
        assert!(extract_zip(&garbage, "top/bin/ffmpeg.exe", &out, 100).is_err());
    }

    #[test]
    fn bounded_copies_stop_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        copy_bounded(&mut &b"12345"[..], &out, 5).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"12345");
        let error = copy_bounded(&mut io::repeat(0), &out, 1024).unwrap_err();
        assert!(error.contains("larger"), "{error}");
        assert!(!out.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tar_xz_members_are_unpacked_and_checked() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/top/bin");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("ffmpeg"), b"program").unwrap();
        std::fs::write(src.join("ffprobe"), b"probe").unwrap();
        let archive = dir.path().join("a.tar.xz");
        let packed = Command::new("tar")
            .arg("-cJf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.path().join("src"))
            .arg("top")
            .status();
        if !packed.is_ok_and(|s| s.success()) {
            eprintln!("skipped: this system's tar cannot write .tar.xz");
            return;
        }
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let out = dir.path().join("ffmpeg");
        extract_tar_xz(&archive, "top/bin/ffmpeg", &work, &out, 100).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"program");
        assert!(!work.join("top/bin/ffprobe").exists());
        assert!(extract_tar_xz(&archive, "top/bin/ffprobe", &work, &out, 2).is_err());
        assert!(extract_tar_xz(&archive, "top/bin/missing", &work, &out, 100).is_err());
        assert!(extract_tar_xz(&archive, "../x", &work, &out, 100).is_err());
    }
}

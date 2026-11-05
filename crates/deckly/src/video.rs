//! Converts a local video once, on the key image worker. Only the resulting
//! APNG goes into a profile, so playback and transfer never launch FFmpeg.

use crate::{animation, store};
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv"];
const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_OUTPUT: u64 = 32 * 1024 * 1024;
const MAX_PIXELS: u32 = 4096 * 4096;
// 32 ten-second clips fit in the shared decoded animation cache.
const SIDE: u32 = 96;
const SECONDS: u32 = 10;
const FPS: u32 = 10;
const TIMEOUT: Duration = Duration::from_secs(30);
static CONVERTING: Mutex<()> = Mutex::new(());

fn demuxer(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" | "mov" => Some("mov"),
        "webm" | "mkv" => Some("matroska"),
        _ => None,
    }
}

fn ffmpeg() -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(name))
                .find(|file| file.is_file())
        })
        .ok_or_else(|| "Video import requires FFmpeg 7 or newer on Deckly's PATH.".into())
}

pub fn import(path: &Path, profile: &str) -> Result<PathBuf, String> {
    let _slot = CONVERTING
        .try_lock()
        .map_err(|_| "A video is already being converted. Try again when it finishes.")?;
    let format = demuxer(path).ok_or("Choose an MP4, MOV, WebM or MKV video.")?;
    let exe = ffmpeg()?;
    let output = tempfile::Builder::new()
        .suffix(".apng")
        .tempfile()
        .map_err(|e| format!("Cannot create a temporary file for the video: {e}"))?;
    convert(&exe, path, format, output.as_file())?;
    if animation::decode(output.path())
        .map_err(|e| format!("Cannot read the converted video: {e}"))?
        .is_none()
    {
        return Err("The video is too short to make an animated key image.".into());
    }
    store::save_key_animation(profile, output.path())
}

/// Give FFmpeg a seekable copy of the selected file. It can read no more
/// than the copy, even if the original grows while conversion runs.
fn input_copy(path: &Path) -> Result<File, String> {
    let io_error = |e| format!("Cannot read the video: {e}");
    let info = path.metadata().map_err(io_error)?;
    if !info.is_file() {
        return Err("Choose a local video file.".into());
    }
    let too_large = "The video exceeds the 64 MiB import limit.";
    if info.len() > MAX_INPUT {
        return Err(too_large.into());
    }
    let mut src = File::open(path).map_err(io_error)?.take(MAX_INPUT + 1);
    let mut input = tempfile::tempfile().map_err(io_error)?;
    if io::copy(&mut src, &mut input).map_err(io_error)? > MAX_INPUT {
        return Err(too_large.into());
    }
    input.rewind().map_err(io_error)?;
    Ok(input)
}

fn convert(exe: &Path, path: &Path, format: &str, output: &File) -> Result<(), String> {
    let input = input_copy(path)?;
    let output = output
        .try_clone()
        .map_err(|e| format!("Cannot write the converted video: {e}"))?;
    let filter =
        format!("fps={FPS},crop='min(iw,ih)':'min(iw,ih)',scale={SIDE}:{SIDE}:flags=area,setsar=1");
    // Both ends use FFmpeg's seekable fd protocol (stdin/stdout). Restrict
    // input to that descriptor and an explicit demuxer, excluding playlists,
    // URLs, and file-based external tracks. MOV data references default off.
    let mut command = Command::new(exe);
    command
        .env_remove("FFREPORT")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-xerror",
            "-abort_on",
            "empty_output",
            "-max_alloc",
            "67108864",
            "-filter_threads",
            "1",
            "-threads",
            "1",
            "-max_pixels",
            &MAX_PIXELS.to_string(),
            "-max_streams",
            "32",
            "-protocol_whitelist",
            "fd",
            "-f",
            format,
            "-i",
            "fd:",
            "-map",
            "0:V:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filter,
            "-pix_fmt",
            "rgba",
            "-c:v",
            "apng",
            "-threads",
            "1",
            "-compression_level",
            "3",
            "-t",
            &SECONDS.to_string(),
            "-frames:v",
            &(SECONDS * FPS).to_string(),
            "-fs",
            &MAX_OUTPUT.to_string(),
            "-plays",
            "0",
            "-f",
            "apng",
            "fd:",
        ])
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start FFmpeg: {e}"))?;
    let deadline = Instant::now() + TIMEOUT;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(concat!(
                    "FFmpeg could not convert this video. Check the file and its size, ",
                    "and make sure FFmpeg 7 or newer supports the codec.",
                )
                .into());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => break "Video conversion timed out after 30 seconds.".into(),
            Err(e) => break format!("Cannot wait for FFmpeg: {e}"),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    Err(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_video_containers_are_selected_and_large_inputs_are_rejected() {
        assert_eq!(demuxer(Path::new("clip.MP4")), Some("mov"));
        assert_eq!(demuxer(Path::new("clip.webm")), Some("matroska"));
        assert_eq!(demuxer(Path::new("clip.m3u8")), None);
        let dir = tempfile::tempdir().unwrap();
        let large = dir.path().join("large.mp4");
        File::create(&large)
            .unwrap()
            .set_len(MAX_INPUT + 1)
            .unwrap();
        assert!(input_copy(&large).unwrap_err().contains("64 MiB"));
        assert!(input_copy(dir.path()).is_err());
    }

    #[test]
    #[ignore = "requires FFmpeg with MPEG-4 and VP9 encoders on PATH"]
    fn ffmpeg_converts_mp4_and_webm_to_bounded_playable_apng_and_rejects_garbage() {
        let exe = ffmpeg().unwrap();
        let dir = tempfile::tempdir().unwrap();
        for (name, codec) in [("clip.mp4", "mpeg4"), ("clip.webm", "libvpx-vp9")] {
            let path = dir.path().join(name);
            let status = Command::new(&exe)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=160x90:r=24:d=11",
                    "-threads",
                    "1",
                    "-c:v",
                    codec,
                ])
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success());
            let output = dir.path().join("video.apng");
            let file = File::create(&output).unwrap();
            convert(&exe, &path, demuxer(&path).unwrap(), &file).unwrap();
            drop(file);
            assert!(output.metadata().unwrap().len() <= MAX_OUTPUT);
            let animation = animation::decode(&output).unwrap().unwrap();
            assert_eq!(animation.frame(0).unwrap().dimensions(), (SIDE, SIDE));
            assert_ne!(animation.frame(0), animation.frame(1));
            let end = Duration::from_secs(u64::from(SECONDS));
            assert_eq!(
                animation.at(end - Duration::from_millis(1)).0,
                (SECONDS * FPS - 1) as usize
            );
            assert_eq!(animation.at(end).0, 0);
        }
        let corrupt = dir.path().join("broken.mp4");
        std::fs::write(&corrupt, b"bad video").unwrap();
        let output = tempfile::tempfile().unwrap();
        assert!(convert(&exe, &corrupt, "mov", &output).is_err());
    }
}

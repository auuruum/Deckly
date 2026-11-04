//! Animated key images: GIF, animated WebP and APNG, decoded within limits
//! into small frames and played on one clock shared by the editor and the
//! devices (docs/clean-room/2026-11-04-animated-keys.md).
//!
//! Files are untrusted. Frame count, canvas size, decoded bytes and frame
//! rate are bounded; a file that breaks a limit while decoding keeps the
//! frames read so far, and one that does not decode at all is an error the
//! key shows as a warning.

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frames, ImageDecoder, Limits, RgbaImage, imageops};
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Files larger than this are not read.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Frames kept; later ones are dropped.
pub const MAX_FRAMES: usize = 200;
/// Widest and tallest canvas decoded.
pub const MAX_SIDE: u32 = 2048;
/// Pixels decoded over all frames, in bytes; later frames are dropped.
pub const MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;
/// Frames are kept at most this large: big enough for any device key and
/// the editor's largest key.
pub const FRAME_SIDE: u32 = 192;
/// Fastest playback.
pub const MAX_FPS: u32 = 30;
/// Slowest frame, so a broken delay cannot stop the animation for good.
const MAX_DELAY_MS: u32 = 60_000;
/// Delays this short are a common way to say "default" in GIFs; browsers
/// show them for 100 ms, and so does Deckly.
const TINY_DELAY_MS: u32 = 10;
const DEFAULT_DELAY_MS: u32 = 100;
/// Decoded files kept, and their bytes; more start the cache over.
const MAX_CACHED: usize = 32;
const MAX_CACHED_BYTES: usize = 128 * 1024 * 1024;

/// A decoded animation: square frames, flattened onto black, and when each
/// one ends.
pub struct Animation {
    frames: Vec<RgbaImage>,
    /// Milliseconds from the start of the loop to the end of each frame.
    ends: Vec<u32>,
}

impl Animation {
    fn new(frames: Vec<(RgbaImage, u32)>) -> Option<Self> {
        let mut ends = Vec::with_capacity(frames.len());
        let mut total = 0u32;
        let mut images = Vec::with_capacity(frames.len());
        for (image, delay) in frames {
            total = total.saturating_add(delay);
            ends.push(total);
            images.push(image);
        }
        (images.len() > 1 && total > 0).then_some(Self {
            frames: images,
            ends,
        })
    }

    /// The frame at `index`, wrapping around.
    pub fn frame(&self, index: usize) -> Option<&RgbaImage> {
        self.frames.get(index % self.frames.len().max(1))
    }

    /// The frame shown `elapsed` into playback, and how long until the next.
    pub fn at(&self, elapsed: Duration) -> (usize, Duration) {
        let total = u128::from(self.ends.last().copied().unwrap_or(0).max(1));
        let into = (elapsed.as_millis() % total) as u32;
        let index = self.ends.partition_point(|&end| end <= into);
        let end = self.ends.get(index).copied().unwrap_or(0);
        (index, Duration::from_millis(u64::from(end - into)))
    }

    fn bytes(&self) -> usize {
        self.frames.iter().map(|f| f.as_raw().len()).sum()
    }
}

/// When playback started; every animated key counts from here, so keys
/// showing the same file stay in step.
pub fn clock() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

type Loaded = Result<Option<Arc<Animation>>, String>;

/// Decoded files, shared by the editor and the devices.
fn cache() -> &'static Mutex<HashMap<PathBuf, Loaded>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Loaded>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// The animation in `path`, decoded once and shared. `Ok(None)` is a file
/// with a single picture, or not an animated format.
pub fn load(path: &Path) -> Loaded {
    if let Some(found) = cache().lock().unwrap_or_else(|p| p.into_inner()).get(path) {
        return found.clone();
    }
    // Decode without the lock; the editor and the devices may both ask.
    let loaded = decode(path).map(|a| a.map(Arc::new));
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    let bytes: usize = cache
        .values()
        .filter_map(|l| l.as_ref().ok()?.as_ref().map(|a| a.bytes()))
        .sum();
    if cache.len() >= MAX_CACHED || bytes > MAX_CACHED_BYTES {
        cache.clear();
    }
    cache.insert(path.to_owned(), loaded.clone());
    loaded
}

/// Forgets decoded files so changed files are read again.
pub fn clear_cache() {
    cache().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// Whether `path` names a format that can hold an animation.
pub fn may_animate(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["gif", "webp", "png", "apng"].contains(&e.to_ascii_lowercase().as_str()))
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    // One canvas and the frame being composited onto it.
    limits.max_alloc = Some(u64::from(MAX_SIDE) * u64::from(MAX_SIDE) * 4 * 3);
    limits
}

fn decode(path: &Path) -> Result<Option<Animation>, String> {
    if !may_animate(path) {
        return Ok(None);
    }
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE_BYTES {
        return Err("file is too large".into());
    }
    let open = || {
        std::fs::File::open(path)
            .map(BufReader::new)
            .map_err(|e| e.to_string())
    };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let frames = match ext.as_str() {
        "gif" => {
            let mut d = GifDecoder::new(open()?).map_err(|e| e.to_string())?;
            d.set_limits(limits()).map_err(|e| e.to_string())?;
            d.into_frames()
        }
        "webp" => {
            let mut d = WebPDecoder::new(open()?).map_err(|e| e.to_string())?;
            if !d.has_animation() {
                return Ok(None);
            }
            d.set_limits(limits()).map_err(|e| e.to_string())?;
            d.into_frames()
        }
        "png" | "apng" => {
            let d = PngDecoder::with_limits(open()?, limits()).map_err(|e| e.to_string())?;
            if !d.is_apng().map_err(|e| e.to_string())? {
                return Ok(None);
            }
            d.apng().map_err(|e| e.to_string())?.into_frames()
        }
        _ => return Ok(None),
    };
    collect(frames, MAX_FRAMES, MAX_DECODED_BYTES)
}

/// Reads frames until the end or a limit: `max_frames` kept, `max_bytes`
/// decoded. A first frame that fails is an error; a later one ends the
/// animation there.
fn collect(
    frames: Frames<'_>,
    max_frames: usize,
    max_bytes: u64,
) -> Result<Option<Animation>, String> {
    let mut kept = Vec::new();
    let mut decoded = 0u64;
    for frame in frames {
        let frame = match frame {
            Ok(frame) => frame,
            Err(e) if kept.is_empty() => return Err(e.to_string()),
            Err(e) => {
                eprintln!("deckly: animation ends early: {e}");
                break;
            }
        };
        let (numer, denom) = frame.delay().numer_denom_ms();
        let image = frame.into_buffer();
        if image.width() == 0 || image.height() == 0 {
            continue;
        }
        decoded += u64::from(image.width()) * u64::from(image.height()) * 4;
        if decoded > max_bytes {
            break;
        }
        kept.push((small(&image), delay_ms(numer, denom)));
        if kept.len() >= max_frames {
            break;
        }
    }
    if kept.is_empty() {
        return Err("the file holds no pictures".into());
    }
    Ok(Animation::new(kept))
}

/// How long a frame shows, within the frame rate limits.
fn delay_ms(numer: u32, denom: u32) -> u32 {
    let ms = numer.checked_div(denom).unwrap_or(0);
    let ms = if ms <= TINY_DELAY_MS {
        DEFAULT_DELAY_MS
    } else {
        ms
    };
    ms.clamp(1000_u32.div_ceil(MAX_FPS), MAX_DELAY_MS)
}

/// A frame as kept: the middle square, flattened onto black and no larger
/// than [`FRAME_SIDE`].
fn small(image: &RgbaImage) -> RgbaImage {
    let (w, h) = image.dimensions();
    let side = w.min(h).max(1);
    let square = imageops::crop_imm(
        image,
        (w - side.min(w)) / 2,
        (h - side.min(h)) / 2,
        side,
        side,
    );
    let mut flat = square.to_image();
    for px in flat.pixels_mut() {
        let a = u32::from(px.0[3]);
        for c in &mut px.0[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
        px.0[3] = 255;
    }
    let out = side.min(FRAME_SIDE);
    if out == side {
        flat
    } else {
        imageops::resize(&flat, out, out, imageops::FilterType::Triangle)
    }
}

#[cfg(test)]
pub mod testing {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame, Rgba, RgbaImage};
    use std::path::{Path, PathBuf};

    /// A GIF of solid frames, each with its colour and delay in ms.
    pub fn gif(path: &Path, size: u32, frames: &[([u8; 3], u32)]) {
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = GifEncoder::new(file);
        encoder.set_repeat(Repeat::Infinite).unwrap();
        for &([r, g, b], ms) in frames {
            let img = RgbaImage::from_pixel(size, size, Rgba([r, g, b, 255]));
            let delay = Delay::from_numer_denom_ms(ms, 1);
            encoder
                .encode_frame(Frame::from_parts(img, 0, 0, delay))
                .unwrap();
        }
    }

    /// A new file name in the temporary folder.
    pub fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("deckly-{}-{name}", uuid::Uuid::new_v4()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, Rgba};

    fn frames(count: usize, side: u32) -> Frames<'static> {
        Frames::new(Box::new((0..count).map(move |i| {
            let img = RgbaImage::from_pixel(side, side, Rgba([i as u8, 0, 0, 255]));
            Ok(Frame::from_parts(
                img,
                0,
                0,
                Delay::from_numer_denom_ms(50, 1),
            ))
        })))
    }

    #[test]
    fn frames_play_with_their_own_delays_and_loop() {
        let path = testing::temp("two.gif");
        testing::gif(&path, 8, &[([255, 0, 0], 100), ([0, 0, 255], 200)]);
        let a = decode(&path).unwrap().unwrap();
        assert_eq!(a.frames.len(), 2);
        assert_eq!(a.frame(0).unwrap().get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(a.frame(3).unwrap().get_pixel(0, 0).0, [0, 0, 255, 255]);
        let ms = Duration::from_millis;
        assert_eq!(a.at(ms(0)), (0, ms(100)));
        assert_eq!(a.at(ms(99)), (0, ms(1)));
        assert_eq!(a.at(ms(100)), (1, ms(200)));
        assert_eq!(a.at(ms(250)), (1, ms(50)));
        assert_eq!(a.at(ms(300)), (0, ms(100)));
        assert_eq!(a.at(ms(300 * 1000 + 120)), (1, ms(180)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_single_picture_is_not_an_animation() {
        let path = testing::temp("one.gif");
        testing::gif(&path, 8, &[([255, 0, 0], 100)]);
        assert!(decode(&path).unwrap().is_none());
        let _ = std::fs::remove_file(path);
        assert!(decode(Path::new("still.jpg")).unwrap().is_none());
    }

    #[test]
    fn broken_files_are_errors() {
        let path = testing::temp("broken.gif");
        std::fs::write(&path, b"GIF89a\x10\x00\x10\x00garbage").unwrap();
        assert!(load(&path).is_err());
        std::fs::write(&path, b"").unwrap();
        clear_cache();
        assert!(load(&path).is_err());
        let _ = std::fs::remove_file(&path);
        assert!(decode(&path).is_err());
    }

    #[test]
    fn oversized_canvases_are_refused() {
        let path = testing::temp("wide.gif");
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        for _ in 0..2 {
            let img = RgbaImage::from_pixel(MAX_SIDE + 1, 1, Rgba([1, 2, 3, 255]));
            encoder
                .encode_frame(Frame::from_parts(
                    img,
                    0,
                    0,
                    Delay::from_numer_denom_ms(50, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        assert!(decode(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn frame_count_and_decoded_bytes_are_bounded() {
        let a = collect(frames(10, 4), 3, u64::MAX).unwrap().unwrap();
        assert_eq!(a.frames.len(), 3);
        // Each 4 x 4 frame decodes to 64 bytes.
        let a = collect(frames(10, 4), 100, 64 * 5).unwrap().unwrap();
        assert_eq!(a.frames.len(), 5);
        // One frame within the budget is no animation.
        assert!(collect(frames(10, 4), 100, 64).unwrap().is_none());
        assert!(collect(frames(0, 4), 100, u64::MAX).is_err());
    }

    #[test]
    fn frames_are_kept_small_and_square() {
        let a = collect(frames(2, 600), 10, u64::MAX).unwrap().unwrap();
        assert!(
            a.frames
                .iter()
                .all(|f| f.dimensions() == (FRAME_SIDE, FRAME_SIDE))
        );
        let wide = RgbaImage::from_pixel(40, 10, Rgba([9, 9, 9, 128]));
        let kept = small(&wide);
        assert_eq!(kept.dimensions(), (10, 10));
        assert_eq!(kept.get_pixel(0, 0).0, [5, 5, 5, 255]);
        assert_eq!(small(&RgbaImage::new(0, 0)).dimensions(), (0, 0));
    }

    #[test]
    fn delays_stay_within_the_frame_rate() {
        assert_eq!(delay_ms(0, 1), DEFAULT_DELAY_MS);
        assert_eq!(delay_ms(10, 1), DEFAULT_DELAY_MS);
        assert_eq!(delay_ms(20, 1), 1000_u32.div_ceil(MAX_FPS));
        assert_eq!(delay_ms(50, 1), 50);
        assert_eq!(delay_ms(u32::MAX, 1), MAX_DELAY_MS);
        assert_eq!(delay_ms(5, 0), DEFAULT_DELAY_MS);
    }
}

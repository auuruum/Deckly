//! Small, bounded preview images. Platform code supplies icons; no assets are
//! copied into profiles or the repository.

use image::RgbaImage;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
use image::{ImageReader, Limits};
#[cfg(any(target_os = "linux", target_os = "macos", test))]
use std::io::Cursor;
#[cfg(any(target_os = "linux", test))]
use std::io::Read;
#[cfg(any(target_os = "linux", test))]
use std::path::Path;

pub const SIZE: u32 = 64;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub struct Loader {
    #[cfg(target_os = "linux")]
    resolver: super::linux_icons::Resolver,
}

impl Loader {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            resolver: super::linux_icons::Resolver::new(),
        }
    }

    pub fn load(&mut self, source: &str) -> Option<RgbaImage> {
        if source.is_empty() || source.len() > 8192 || source.contains('\0') {
            return None;
        }
        #[cfg(target_os = "linux")]
        return self
            .resolver
            .find(source)
            .and_then(|path| decode_file(&path));
        #[cfg(windows)]
        return super::windows::icon(source).map(normalize);
        #[cfg(target_os = "macos")]
        return super::macos::icon(source);
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        None
    }
}

#[cfg(any(target_os = "linux", test))]
fn decode_file(path: &Path) -> Option<RgbaImage> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        svg(&bytes)
    } else {
        decode(&bytes)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
pub(super) fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    Some(normalize(reader.decode().ok()?.into_rgba8()))
}

#[cfg(any(windows, test))]
pub(super) fn bgra(bytes: &mut [u8]) {
    let has_alpha = bytes.as_chunks::<4>().0.iter().any(|[_, _, _, a]| *a != 0);
    for pixel in bytes.as_chunks_mut::<4>().0 {
        let [b, g, r, a] = *pixel;
        let alpha = if has_alpha { a } else { 255 };
        let straight = |value: u8| {
            if alpha == 0 {
                0
            } else {
                (u32::from(value) * 255 / u32::from(alpha)).min(255) as u8
            }
        };
        *pixel = [straight(r), straight(g), straight(b), alpha];
    }
}

fn normalize(image: RgbaImage) -> RgbaImage {
    let mut output = RgbaImage::new(SIZE, SIZE);
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return output;
    }
    let (width, height) = if width >= height {
        (
            SIZE,
            (u64::from(height) * u64::from(SIZE) / u64::from(width)).max(1) as u32,
        )
    } else {
        (
            (u64::from(width) * u64::from(SIZE) / u64::from(height)).max(1) as u32,
            SIZE,
        )
    };
    let resized = image::imageops::thumbnail(&image, width, height);
    let x = (SIZE.saturating_sub(resized.width())) / 2;
    let y = (SIZE.saturating_sub(resized.height())) / 2;
    image::imageops::overlay(&mut output, &resized, i64::from(x), i64::from(y));
    output
}

#[cfg(any(target_os = "linux", test))]
fn svg(bytes: &[u8]) -> Option<RgbaImage> {
    use resvg::{tiny_skia, usvg};
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    // Icons are paths and shapes; external images must not cause arbitrary
    // file reads or unbounded embedded bitmap allocations.
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let size = tree.size();
    let scale = (SIZE as f32 / size.width()).min(SIZE as f32 / size.height());
    let transform = tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (SIZE as f32 - size.width() * scale) / 2.0,
        (SIZE as f32 - size.height() * scale) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut image = RgbaImage::new(SIZE, SIZE);
    for (dst, src) in image.pixels_mut().zip(pixmap.pixels()) {
        let color = src.demultiply();
        *dst = image::Rgba([color.red(), color.green(), color.blue(), color.alpha()]);
    }
    image.pixels().any(|p| p.0[3] != 0).then_some(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previews_fit_without_stretching_and_keep_transparency() {
        let image = RgbaImage::from_pixel(128, 64, image::Rgba([1, 2, 3, 255]));
        let preview = normalize(image);
        assert_eq!(preview.dimensions(), (SIZE, SIZE));
        assert_eq!(preview.get_pixel(0, 0).0[3], 0);
        assert_eq!(preview.get_pixel(32, 32).0, [1, 2, 3, 255]);
        let svg = svg(br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#).unwrap();
        assert_eq!(svg.dimensions(), (SIZE, SIZE));
        assert!(decode(b"invalid image").is_none());
        assert!(super::svg(b"invalid svg").is_none());
    }
    #[test]
    fn missing_large_and_corrupt_files_have_no_preview() {
        let path = std::env::temp_dir().join(format!("deckly-icon-{}", uuid::Uuid::new_v4()));
        assert!(decode_file(&path).is_none());
        std::fs::write(&path, b"invalid").unwrap();
        assert!(decode_file(&path).is_none());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(decode_file(&path).is_none());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn oversized_rasters_and_svg_external_images_are_refused() {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(RgbaImage::new(2049, 1))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(decode(bytes.get_ref()).is_none());
        let path =
            std::env::temp_dir().join(format!("deckly-external-icon-{}.png", uuid::Uuid::new_v4()));
        RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let markup = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="64" height="64"><image xlink:href="{}" width="64" height="64"/></svg>"#,
            path.display()
        );
        assert!(svg(markup.as_bytes()).is_none());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn native_bgra_has_straight_rgba_and_opaque_legacy_icons() {
        let mut alpha = [0, 32, 64, 128];
        bgra(&mut alpha);
        assert_eq!(alpha, [127, 63, 0, 128]);
        let mut opaque = [1, 2, 3, 0];
        bgra(&mut opaque);
        assert_eq!(opaque, [3, 2, 1, 255]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "reads installed application icons on this desktop"]
    fn installed_applications_have_real_icons() {
        let catalog = super::super::discover().unwrap();
        let mut loader = Loader::new();
        for name in ["spotify", "1password", "alacritty"] {
            let app = catalog
                .applications
                .iter()
                .find(|app| app.name.to_lowercase().contains(name))
                .unwrap();
            let icon = loader.load(&app.icon).unwrap();
            assert_eq!(icon.dimensions(), (SIZE, SIZE));
            assert!(icon.pixels().any(|pixel| pixel.0[3] != 0));
            eprintln!("{}: loaded {}", app.name, app.icon);
        }
    }
}

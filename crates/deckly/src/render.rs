//! Key image rendering, shared by the editor canvas and the device.
//!
//! A key is drawn from its configuration alone: a custom image or the
//! action's icon, then the title on top. The same pixels go to the screen
//! (at a larger size) and to the device (as a JPEG at the key size).

use crate::actions;
use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use deckly_core::{DeviceModel, ImageFormat, KeyConfig, Rotation, TitleAlign};
use image::{ImageReader, Limits, Rgba, RgbaImage, imageops};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

const EMPTY: Rgba<u8> = Rgba([0, 0, 0, 255]);
const ICON_BACKGROUND: Rgba<u8> = Rgba([0, 0, 0, 255]);
const ICON_COLOR: [u8; 3] = [0xE6, 0xE7, 0xEA];
const MAX_IMAGE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TITLE_LINES: usize = 5;

pub struct Renderer {
    title_font: Option<FontArc>,
    icon_font: Option<FontArc>,
    images: HashMap<PathBuf, Option<RgbaImage>>,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            title_font: load_title_font(),
            icon_font: FontArc::try_from_slice(egui_phosphor::Variant::Regular.font_bytes()).ok(),
            images: HashMap::new(),
        }
    }

    /// Forgets decoded custom images so changed files are read again.
    pub fn clear_image_cache(&mut self) {
        self.images.clear();
    }

    pub fn render(&mut self, key: Option<&KeyConfig>, size: u32) -> RgbaImage {
        let size = size.clamp(8, 1024);
        let Some(key) = key else {
            return RgbaImage::from_pixel(size, size, EMPTY);
        };
        let mut img = match key.image.as_deref().and_then(|p| self.custom_image(p)) {
            Some(src) => imageops::resize(src, size, size, imageops::FilterType::Triangle),
            None => {
                let mut img = RgbaImage::from_pixel(size, size, ICON_BACKGROUND);
                if let Some(font) = &self.icon_font {
                    let lift = if has_title(key) && key.title_style.align == TitleAlign::Bottom {
                        size as f32 * 0.08
                    } else {
                        0.0
                    };
                    draw_icon(
                        &mut img,
                        font,
                        actions::key_icon(key),
                        size as f32 * 0.5,
                        lift,
                    );
                }
                img
            }
        };
        if has_title(key)
            && let Some(font) = &self.title_font
        {
            draw_title(&mut img, font, key, size);
        }
        img
    }

    /// Key image for the device: its size, orientation and file format.
    pub fn device_image(&mut self, key: Option<&KeyConfig>, model: &DeviceModel) -> Vec<u8> {
        let img = self.render(key, model.key_image_size);
        let img = match model.rotation {
            Rotation::None => img,
            Rotation::Half => imageops::rotate180(&img),
            Rotation::Clockwise => imageops::rotate90(&img),
            Rotation::CounterClockwise => imageops::rotate270(&img),
        };
        let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
        let mut out = Vec::new();
        let result = match model.image_format() {
            ImageFormat::Jpeg => rgb.write_with_encoder(
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90),
            ),
            ImageFormat::Bmp => {
                rgb.write_with_encoder(image::codecs::bmp::BmpEncoder::new(&mut out))
            }
        };
        if let Err(e) = result {
            eprintln!("deckly: key image encoding failed: {e}");
        }
        out
    }

    fn custom_image(&mut self, path: &Path) -> Option<&RgbaImage> {
        self.images
            .entry(path.to_owned())
            .or_insert_with(|| match decode_image(path) {
                Ok(img) => Some(img),
                Err(e) => {
                    eprintln!("deckly: cannot load key image {}: {e}", path.display());
                    None
                }
            })
            .as_ref()
    }
}

/// Changes whenever anything that affects a key's pixels changes.
pub fn key_hash(key: Option<&KeyConfig>) -> u64 {
    let mut h = DefaultHasher::new();
    if let Some(key) = key {
        key.action.hash(&mut h);
        key.title.hash(&mut h);
        key.title_style.hash(&mut h);
        key.image.hash(&mut h);
        // Only multimedia icons depend on settings today.
        if key.action == actions::MULTIMEDIA {
            actions::str_setting(&key.settings, "media").hash(&mut h);
        }
    }
    h.finish()
}

fn has_title(key: &KeyConfig) -> bool {
    key.title_style.show && !key.title.trim().is_empty()
}

fn decode_image(path: &Path) -> Result<RgbaImage, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_IMAGE_FILE_BYTES {
        return Err("file is too large".into());
    }
    let mut reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
    // Square crop from the center, like "fill".
    let (w, h) = img.dimensions();
    let side = w.min(h);
    Ok(imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image())
}

fn load_title_font() -> Option<FontArc> {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Windows\Fonts\segoeuib.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &["/System/Library/Fonts/Supplemental/Arial Bold.ttf"]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
        ]
    };
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(font) = FontArc::try_from_vec(bytes)
        {
            return Some(font);
        }
    }
    // egui's bundled font is always available.
    let defaults = eframe::egui::FontDefinitions::default();
    let data = defaults.font_data.get("Ubuntu-Light")?;
    FontArc::try_from_vec(data.font.to_vec()).ok()
}

fn blend(img: &mut RgbaImage, x: i32, y: i32, color: [u8; 3], coverage: f32) {
    if x < 0 || y < 0 {
        return;
    }
    let Some(px) = img.get_pixel_mut_checked(x as u32, y as u32) else {
        return;
    };
    let a = coverage.clamp(0.0, 1.0);
    for (dst, &src) in px.0.iter_mut().zip(color.iter()) {
        *dst = (f32::from(src) * a + f32::from(*dst) * (1.0 - a)).round() as u8;
    }
    px.0[3] = 255;
}

fn draw_icon(img: &mut RgbaImage, font: &FontArc, glyph: &str, px: f32, lift: f32) {
    let Some(c) = glyph.chars().next() else {
        return;
    };
    let g = font
        .glyph_id(c)
        .with_scale_and_position(PxScale::from(px), point(0.0, 0.0));
    let Some(outline) = font.outline_glyph(g) else {
        return;
    };
    let b = outline.px_bounds();
    let (w, h) = (img.width() as f32, img.height() as f32);
    let ox = ((w - b.width()) / 2.0).round() as i32;
    let oy = ((h - b.height()) / 2.0 - lift).round() as i32;
    outline.draw(|x, y, cov| blend(img, ox + x as i32, oy + y as i32, ICON_COLOR, cov));
}

fn line_width(font: &FontArc, scale: PxScale, line: &str) -> f32 {
    let sf = font.as_scaled(scale);
    let mut width = 0.0;
    let mut prev = None;
    for c in line.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            width += sf.kern(p, id);
        }
        width += sf.h_advance(id);
        prev = Some(id);
    }
    width
}

fn draw_line(
    img: &mut RgbaImage,
    font: &FontArc,
    scale: PxScale,
    line: &str,
    (dx, baseline): (f32, f32),
    color: [u8; 3],
) {
    let sf = font.as_scaled(scale);
    let mut x = (img.width() as f32 - line_width(font, scale, line)) / 2.0 + dx;
    let mut prev = None;
    for c in line.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            x += sf.kern(p, id);
        }
        let g = id.with_scale_and_position(scale, point(x, baseline));
        if let Some(outline) = font.outline_glyph(g) {
            let b = outline.px_bounds();
            let (bx, by) = (b.min.x as i32, b.min.y as i32);
            outline.draw(|gx, gy, cov| blend(img, bx + gx as i32, by + gy as i32, color, cov));
        }
        x += sf.h_advance(id);
        prev = Some(id);
    }
}

fn draw_title(img: &mut RgbaImage, font: &FontArc, key: &KeyConfig, size: u32) {
    let style = key.title_style;
    let k = size as f32 / 72.0;
    let scale = PxScale::from(f32::from(style.size) * 1.2 * k);
    let sf = font.as_scaled(scale);
    let lines: Vec<&str> = key.title.lines().take(MAX_TITLE_LINES).collect();
    let line_height = sf.height() + sf.line_gap();
    let block = line_height * lines.len() as f32 - sf.line_gap();
    let pad = 4.0 * k;
    let top = match style.align {
        TitleAlign::Top => pad,
        TitleAlign::Middle => (size as f32 - block) / 2.0,
        TitleAlign::Bottom => size as f32 - pad - block,
    };
    let o = k.max(1.0);
    for (i, line) in lines.iter().enumerate() {
        let baseline = top + sf.ascent() + line_height * i as f32;
        // A dark outline keeps titles readable on any image.
        for (dx, dy) in [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o)] {
            draw_line(img, font, scale, line, (dx, baseline + dy), [0, 0, 0]);
        }
        draw_line(img, font, scale, line, (0.0, baseline), style.color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_requested_sizes() {
        let mut r = Renderer::new();
        assert_eq!(r.render(None, 72).dimensions(), (72, 72));
        let mut key = KeyConfig::new(actions::WEBSITE, json!({}));
        key.title = "Hello\nWorld".into();
        assert_eq!(r.render(Some(&key), 96).dimensions(), (96, 96));
        assert_eq!(r.render(Some(&key), 0).dimensions(), (8, 8));
    }

    #[test]
    fn missing_custom_image_falls_back_to_icon() {
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.image = Some("does/not/exist.png".into());
        let img = r.render(Some(&key), 72);
        assert_eq!(img.get_pixel(0, 0), &ICON_BACKGROUND);
    }

    #[test]
    fn device_images_fit_every_model() {
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.title = "Title".into();
        for model in DeviceModel::ALL {
            let data = r.device_image(Some(&key), model);
            let magic: &[u8] = match model.image_format() {
                ImageFormat::Jpeg => &[0xFF, 0xD8],
                ImageFormat::Bmp => b"BM",
            };
            assert_eq!(&data[..2], magic, "{}", model.name);
            assert!(data.len() < deckly_device::protocol::MAX_IMAGE_BYTES);
            let decoded = image::load_from_memory(&data).unwrap();
            assert_eq!(decoded.width(), model.key_image_size);
            assert_eq!(decoded.height(), model.key_image_size);
        }
    }

    #[test]
    fn device_images_are_turned_per_model() {
        // A key with a white top-left corner shows where that corner lands.
        let path = std::env::temp_dir().join(format!("deckly-corner-{}.png", std::process::id()));
        let mut src = RgbaImage::from_pixel(80, 80, Rgba([0, 0, 0, 255]));
        for y in 0..20 {
            for x in 0..20 {
                src.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        src.save(&path).unwrap();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.image = Some(path.clone());
        let mut r = Renderer::new();
        let mut corner = |kind| {
            let model = DeviceModel::of(kind);
            let img = image::load_from_memory(&r.device_image(Some(&key), model))
                .unwrap()
                .into_luma8();
            let s = model.key_image_size - 1;
            let bright = |x, y| img.get_pixel(x, y)[0] > 128;
            [bright(0, 0), bright(s, 0), bright(s, s), bright(0, s)]
        };
        use deckly_core::DeviceKind as K;
        // Corners in order: top left, top right, bottom right, bottom left.
        assert_eq!(corner(K::Plus), [true, false, false, false]);
        assert_eq!(corner(K::Mk2), [false, false, true, false]);
        assert_eq!(corner(K::Mini), [false, true, false, false]);
        assert_eq!(corner(K::PlusXl), [false, false, false, true]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn hash_tracks_visible_changes() {
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        let a = key_hash(Some(&key));
        key.title = "x".into();
        assert_ne!(a, key_hash(Some(&key)));
        assert_ne!(key_hash(None), key_hash(Some(&key)));
    }
}

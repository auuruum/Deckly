//! Key image rendering, shared by the editor canvas and the device.
//!
//! A key is drawn from its configuration and the global title style: a
//! custom image or the action's icon, then the title on top. The same pixels go to the screen
//! (at a larger size) and to the device (as a JPEG at the key size).

use crate::actions;
use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use deckly_core::{
    DeviceModel, ImageFormat, KeyConfig, Orientation, Rotation, TitleAlign, TitleStyle, Turn,
};
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
    /// Title style of keys that do not set their own.
    pub title_defaults: TitleStyle,
    title_font: Option<FontArc>,
    icon_font: Option<FontArc>,
    images: HashMap<PathBuf, Option<RgbaImage>>,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            title_defaults: TitleStyle::default(),
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
        let style = self.title_style(key);
        let mut img = match key.image.as_deref().and_then(|p| self.custom_image(p)) {
            Some(src) => imageops::resize(src, size, size, imageops::FilterType::Triangle),
            None => {
                let mut img = RgbaImage::from_pixel(size, size, ICON_BACKGROUND);
                if let Some(font) = &self.icon_font {
                    let lift = if has_title(key, &style) && style.align == TitleAlign::Bottom {
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
        if has_title(key, &style)
            && let Some(font) = &self.title_font
        {
            draw_title(&mut img, font, &key.title, &style, size);
        }
        img
    }

    /// The title style a key is drawn with.
    pub fn title_style(&self, key: &KeyConfig) -> TitleStyle {
        key.title_style.unwrap_or(self.title_defaults)
    }

    /// Changes whenever anything that affects a key's pixels changes.
    pub fn key_hash(&self, key: Option<&KeyConfig>) -> u64 {
        let mut h = DefaultHasher::new();
        if let Some(key) = key {
            key.action.hash(&mut h);
            key.title.hash(&mut h);
            self.title_style(key).hash(&mut h);
            key.image.hash(&mut h);
            // Only multimedia icons depend on settings today.
            if key.action == actions::MULTIMEDIA {
                actions::str_setting(&key.settings, "media").hash(&mut h);
            }
        }
        h.finish()
    }

    /// Key image for the device: its size, the model's and the user's
    /// orientation, and the model's file format.
    pub fn device_image(
        &mut self,
        key: Option<&KeyConfig>,
        model: &DeviceModel,
        orientation: Orientation,
    ) -> Vec<u8> {
        let img = self.render(key, model.key_image_size);
        encode_key(img, model, orientation)
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

fn has_title(key: &KeyConfig, style: &TitleStyle) -> bool {
    style.show && !key.title.trim().is_empty()
}

/// Turns a key-sized image for the user's orientation and the model, then
/// encodes it in the model's format.
pub fn encode_key(img: RgbaImage, model: &DeviceModel, orientation: Orientation) -> Vec<u8> {
    let img = turn(turn_image(img, orientation.turn()), model.rotation);
    let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
    let mut out = Vec::new();
    let result = match model.image_format() {
        ImageFormat::Jpeg => rgb.write_with_encoder(
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90),
        ),
        ImageFormat::Bmp => rgb.write_with_encoder(image::codecs::bmp::BmpEncoder::new(&mut out)),
    };
    if let Err(e) = result {
        eprintln!("deckly: key image encoding failed: {e}");
    }
    out
}

fn turn_image(img: RgbaImage, turn: Turn) -> RgbaImage {
    match turn {
        Turn::None => img,
        Turn::Quarter => imageops::rotate90(&img),
        Turn::Half => imageops::rotate180(&img),
        Turn::ThreeQuarters => imageops::rotate270(&img),
    }
}

/// Applies a model's upload rotation.
fn turn(img: RgbaImage, rotation: Rotation) -> RgbaImage {
    turn_image(
        img,
        match rotation {
            Rotation::None => Turn::None,
            Rotation::Half => Turn::Half,
            Rotation::Clockwise => Turn::Quarter,
            Rotation::CounterClockwise => Turn::ThreeQuarters,
        },
    )
}

/// Loads a screensaver picture and cuts it into one key-sized image per
/// key, row by row as the user sees the keys.
pub fn screensaver_keys(path: &Path, model: &DeviceModel) -> Result<Vec<RgbaImage>, String> {
    let img = decode_file(path)?;
    Ok(cut_keys(&img, model))
}

/// Cuts a panel-sized picture into key images. A picture of another size is
/// stretched to the panel first.
pub fn cut_keys(img: &RgbaImage, model: &DeviceModel) -> Vec<RgbaImage> {
    let (w, h) = model.panel_size();
    let resized;
    let panel = if img.dimensions() == (w, h) {
        img
    } else {
        resized = imageops::resize(img, w, h, imageops::FilterType::Triangle);
        &resized
    };
    model
        .key_rects()
        .iter()
        .map(|r| imageops::crop_imm(panel, r.x, r.y, r.size, r.size).to_image())
        .collect()
}

/// Highest JPEG quality tried for a standby image, the lowest, and the step
/// between tries (as the official app does).
const STANDBY_QUALITY: (u8, u8, u8) = (98, 71, 4);

/// Encodes a standby picture for a model's screen: stretched to the screen,
/// turned like key images (not by the user's orientation), and JPEG with
/// the best quality that fits the model's limit.
pub fn standby_jpeg(img: &RgbaImage, model: &DeviceModel) -> Result<Vec<u8>, String> {
    let screen = model
        .screen
        .ok_or_else(|| format!("{} has no standby image", model.name))?;
    let sized = imageops::resize(
        img,
        screen.width,
        screen.height,
        imageops::FilterType::Triangle,
    );
    let rgb = image::DynamicImage::ImageRgba8(turn(sized, model.rotation)).into_rgb8();
    let (top, bottom, step) = STANDBY_QUALITY;
    let mut quality = top;
    while quality >= bottom {
        let mut out = Vec::new();
        rgb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut out, quality,
        ))
        .map_err(|e| e.to_string())?;
        if out.len() <= screen.standby_max_bytes as usize {
            return Ok(out);
        }
        quality -= step;
    }
    Err("the image does not fit on the device even at low quality".into())
}

/// Deckly's own standby picture. The screen shows only through the key
/// windows, so each key gets a rounded tile coloured along the logo's
/// blue-to-violet gradient: the whole device becomes the logo's key grid.
pub fn default_standby(model: &DeviceModel) -> RgbaImage {
    let (w, h) = model.panel_size();
    let mut img = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 255]));
    let columns = usize::from(model.columns.max(1));
    let steps = (usize::from(model.columns) + usize::from(model.rows)).saturating_sub(2);
    for (i, r) in model.key_rects().iter().enumerate() {
        let t = (i / columns + i % columns) as f32 / steps.max(1) as f32;
        let [cr, cg, cb] = crate::logo::cell_color(t);
        let inset = r.size as f32 * 0.14;
        let side = r.size as f32 - 2.0 * inset;
        for y in r.y..(r.y + r.size).min(h) {
            for x in r.x..(r.x + r.size).min(w) {
                let cover = crate::logo::coverage(
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    r.x as f32 + inset,
                    r.y as f32 + inset,
                    side,
                    side * 0.23,
                    1.0,
                );
                let a = (cover * 255.0).round() as u16;
                let blend = |c: u8| ((u16::from(c) * a) / 255) as u8;
                img.put_pixel(x, y, Rgba([blend(cr), blend(cg), blend(cb), 255]));
            }
        }
    }
    img
}

/// Decodes any supported picture file within the size limits.
pub fn decode_file(path: &Path) -> Result<RgbaImage, String> {
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
    Ok(reader.decode().map_err(|e| e.to_string())?.into_rgba8())
}

fn decode_image(path: &Path) -> Result<RgbaImage, String> {
    let img = decode_file(path)?;
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

fn draw_title(img: &mut RgbaImage, font: &FontArc, title: &str, style: &TitleStyle, size: u32) {
    let k = size as f32 / 72.0;
    let scale = PxScale::from(f32::from(style.size) * 1.2 * k);
    let sf = font.as_scaled(scale);
    let lines: Vec<&str> = title.lines().take(MAX_TITLE_LINES).collect();
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
            let data = r.device_image(Some(&key), model, Orientation::Standard);
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
        let mut corner = |kind, orientation| {
            let model = DeviceModel::of(kind);
            let img = image::load_from_memory(&r.device_image(Some(&key), model, orientation))
                .unwrap()
                .into_luma8();
            let s = model.key_image_size - 1;
            let bright = |x, y| img.get_pixel(x, y)[0] > 128;
            [bright(0, 0), bright(s, 0), bright(s, s), bright(0, s)]
        };
        use deckly_core::DeviceKind as K;
        // Corners in order: top left, top right, bottom right, bottom left.
        let standard = Orientation::Standard;
        assert_eq!(corner(K::Plus, standard), [true, false, false, false]);
        assert_eq!(corner(K::Mk2, standard), [false, false, true, false]);
        assert_eq!(corner(K::Mini, standard), [false, true, false, false]);
        assert_eq!(corner(K::PlusXl, standard), [false, false, false, true]);
        // Upside down adds half a turn on top of the model's own.
        let flipped = Orientation::UpsideDown;
        assert_eq!(corner(K::Plus, flipped), [false, false, true, false]);
        assert_eq!(corner(K::Mk2, flipped), [true, false, false, false]);
        assert_eq!(corner(K::Mini, flipped), [false, false, false, true]);
        // Icon turns: 90° turns content a quarter counter-clockwise, 270° a
        // quarter clockwise, on top of the MK.2's half turn.
        assert_eq!(
            corner(K::Mk2, Orientation::Rotate90),
            [false, true, false, false]
        );
        assert_eq!(
            corner(K::Mk2, Orientation::Rotate180),
            [true, false, false, false]
        );
        assert_eq!(
            corner(K::Mk2, Orientation::Rotate270),
            [false, false, false, true]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn screensaver_is_cut_at_key_positions() {
        let model = DeviceModel::of(deckly_core::DeviceKind::Mk2);
        let mut panel = RgbaImage::from_pixel(480, 272, Rgba([0, 0, 0, 255]));
        // Mark the top-left pixel of the last key's window.
        panel.put_pixel(408, 200, Rgba([255, 0, 0, 255]));
        let keys = cut_keys(&panel, model);
        assert_eq!(keys.len(), 15);
        assert!(keys.iter().all(|k| k.dimensions() == (72, 72)));
        assert_eq!(keys[14].get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
        assert_eq!(keys[0].get_pixel(0, 0), &Rgba([0, 0, 0, 255]));
        // Any picture size is stretched to the panel.
        let small = RgbaImage::new(10, 10);
        assert_eq!(cut_keys(&small, model).len(), 15);
        let missing = screensaver_keys(Path::new("does/not/exist.png"), model);
        assert!(missing.is_err());
    }

    #[test]
    fn default_standby_lights_every_key() {
        let mk2 = DeviceModel::of(deckly_core::DeviceKind::Mk2);
        let img = default_standby(mk2);
        let keys = cut_keys(&img, mk2);
        // Each key shows a tile in its middle and black at its corner.
        for key in &keys {
            assert_ne!(key.get_pixel(36, 36), &Rgba([0, 0, 0, 255]));
            assert_eq!(key.get_pixel(0, 0), &Rgba([0, 0, 0, 255]));
        }
        // Blue at the top left, violet at the bottom right, like the logo.
        assert_eq!(keys[0].get_pixel(36, 36), &Rgba([0x3D, 0x8B, 0xFD, 255]));
        assert_eq!(keys[14].get_pixel(36, 36), &Rgba([0x8B, 0x5C, 0xF6, 255]));
    }

    #[test]
    fn standby_image_fits_the_screen() {
        let mk2 = DeviceModel::of(deckly_core::DeviceKind::Mk2);
        let jpeg = standby_jpeg(&default_standby(mk2), mk2).unwrap();
        assert!(jpeg.len() <= 194_560);
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (480, 272));
        // Noise needs lower quality but still fits.
        let mut noise = RgbaImage::new(480, 272);
        let mut seed = 1u32;
        for px in noise.pixels_mut() {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let [a, b, c, _] = seed.to_le_bytes();
            *px = Rgba([a, b, c, 255]);
        }
        assert!(standby_jpeg(&noise, mk2).unwrap().len() <= 194_560);
        let plus = DeviceModel::of(deckly_core::DeviceKind::Plus);
        assert!(standby_jpeg(&noise, plus).is_err());
    }

    #[test]
    fn hash_tracks_visible_changes() {
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        let a = r.key_hash(Some(&key));
        key.title = "x".into();
        let b = r.key_hash(Some(&key));
        assert_ne!(a, b);
        assert_ne!(r.key_hash(None), b);
        // Keys without their own style change with the global one...
        r.title_defaults.size = 20;
        let c = r.key_hash(Some(&key));
        assert_ne!(b, c);
        // ...keys with their own style do not.
        key.title_style = Some(TitleStyle::default());
        let d = r.key_hash(Some(&key));
        r.title_defaults.size = 9;
        assert_eq!(d, r.key_hash(Some(&key)));
    }
}

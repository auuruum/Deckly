//! Key image rendering, shared by the editor canvas and the device.
//!
//! A key is drawn from its configuration and the global title style: a
//! custom image or the action's icon, then the title on top. The same pixels go to the screen
//! (at a larger size) and to the device (as a JPEG at the key size).

use crate::actions;
use crate::plugins::{Mark, Overlay};
use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use deckly_core::{
    DeviceModel, ImageFormat, KeyConfig, KeyPress, Orientation, Rotation, TitleAlign, TitleStyle,
    Turn,
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

/// What a key shows that is not in its configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Live {
    /// The current page number, counted from 1.
    pub page: usize,
    /// The key is held down.
    pub pressed: bool,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            page: 1,
            pressed: false,
        }
    }
}

pub struct Renderer {
    /// Title style of keys that do not set their own.
    pub title_defaults: TitleStyle,
    /// How held keys are drawn. The device has no press effect of its own;
    /// the host sends a smaller picture.
    pub key_press: KeyPress,
    title_font: Option<FontArc>,
    icon_font: Option<FontArc>,
    images: HashMap<PathBuf, Option<RgbaImage>>,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            title_defaults: TitleStyle::default(),
            key_press: KeyPress::default(),
            title_font: load_title_font(),
            icon_font: FontArc::try_from_slice(egui_phosphor::Variant::Regular.font_bytes()).ok(),
            images: HashMap::new(),
        }
    }

    /// Forgets decoded custom images so changed files are read again.
    pub fn clear_image_cache(&mut self) {
        self.images.clear();
    }

    /// Draws a key. `overlay` is what its plugin shows, for plugin keys.
    pub fn render(
        &mut self,
        key: Option<&KeyConfig>,
        size: u32,
        live: Live,
        overlay: Option<&Overlay>,
    ) -> RgbaImage {
        let size = size.clamp(8, 1024);
        let Some(key) = key else {
            return RgbaImage::from_pixel(size, size, EMPTY);
        };
        let style = self.title_style(key);
        // The user's own image wins over the plugin's.
        let picture = key
            .image
            .as_deref()
            .and_then(|p| self.custom_image(p))
            .map(|src| fit_image(src, size))
            .or_else(|| overlay?.image.as_ref().map(|i| fit_image(&i.img, size)));
        let mut img = match picture {
            Some(img) => img,
            None => {
                let mut img = RgbaImage::from_pixel(size, size, ICON_BACKGROUND);
                if key.action == actions::PAGE_INDICATOR {
                    if let Some(font) = &self.title_font {
                        draw_number(&mut img, font, live.page);
                    }
                } else if let Some(font) = &self.icon_font {
                    draw_icon(&mut img, font, actions::key_icon(key), size as f32 * 0.5);
                }
                img
            }
        };
        let title = shown_title(key, overlay);
        if style.show
            && !title.trim().is_empty()
            && let Some(font) = &self.title_font
        {
            draw_title(&mut img, font, title, &style, size);
        }
        if let Some(mark) = overlay.and_then(|o| o.mark)
            && let Some(font) = &self.icon_font
        {
            draw_mark(&mut img, font, mark);
        }
        if let Some(percent) = self.pressed_size(live) {
            img = shrink(&img, percent);
        }
        img
    }

    /// The title style a key is drawn with.
    pub fn title_style(&self, key: &KeyConfig) -> TitleStyle {
        key.title_style.unwrap_or(self.title_defaults)
    }

    /// Changes whenever anything that affects a key's pixels changes.
    pub fn key_hash(&self, key: Option<&KeyConfig>, live: Live, overlay: Option<&Overlay>) -> u64 {
        let mut h = DefaultHasher::new();
        if let Some(key) = key {
            overlay.hash(&mut h);
            key.action.hash(&mut h);
            key.title.hash(&mut h);
            self.title_style(key).hash(&mut h);
            key.image.hash(&mut h);
            // Multimedia icons and the switch below depend on settings.
            if key.action == actions::MULTIMEDIA {
                actions::str_setting(&key.settings, "media").hash(&mut h);
            }
            if key.action == actions::PAGE_INDICATOR {
                live.page.hash(&mut h);
            }
            if key.action == actions::MULTI_ACTION_SWITCH {
                actions::switch_side(key).hash(&mut h);
            }
            self.pressed_size(live).hash(&mut h);
        }
        h.finish()
    }

    /// The size a key is drawn at in percent, when it is drawn smaller.
    fn pressed_size(&self, live: Live) -> Option<u8> {
        (live.pressed && self.key_press.shrink).then_some(self.key_press.size)
    }

    /// Key image for the device: its size, the model's and the user's
    /// orientation, and the model's file format.
    pub fn device_image(
        &mut self,
        key: Option<&KeyConfig>,
        live: Live,
        overlay: Option<&Overlay>,
        model: &DeviceModel,
        orientation: Orientation,
    ) -> Vec<u8> {
        let img = self.render(key, model.key_image_size, live, overlay);
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

/// Scales a custom image to a key. Transparent pixels are flattened onto the
/// key's black first: the device has no alpha, and scaling the colour of
/// transparent pixels (often white or gray) would leave halos on the edges.
fn fit_image(src: &RgbaImage, size: u32) -> RgbaImage {
    let mut flat = src.clone();
    for px in flat.pixels_mut() {
        let a = u32::from(px.0[3]);
        for c in &mut px.0[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
        px.0[3] = 255;
    }
    // Lanczos keeps fine edges sharp when a large picture shrinks to a key.
    imageops::resize(&flat, size, size, imageops::FilterType::Lanczos3)
}

/// A held key: the whole picture, title included, at `percent` of its size
/// and centred on black.
fn shrink(img: &RgbaImage, percent: u8) -> RgbaImage {
    let size = img.width();
    let small = (size * u32::from(percent) / 100).clamp(1, size);
    let scaled = imageops::resize(img, small, small, imageops::FilterType::Triangle);
    let mut out = RgbaImage::from_pixel(size, size, EMPTY);
    let offset = i64::from((size - small) / 2);
    imageops::overlay(&mut out, &scaled, offset, offset);
    out
}

/// The user's title, else the one the key's plugin set.
fn shown_title<'a>(key: &'a KeyConfig, overlay: Option<&'a Overlay>) -> &'a str {
    if key.title.trim().is_empty() {
        overlay.and_then(|o| o.title.as_deref()).unwrap_or("")
    } else {
        &key.title
    }
}

/// A plugin's short feedback: a warning sign or a check mark over the key.
fn draw_mark(img: &mut RgbaImage, font: &FontArc, mark: Mark) {
    for px in img.pixels_mut() {
        for c in &mut px.0[..3] {
            *c /= 3;
        }
    }
    let (glyph, color) = match mark {
        Mark::Alert => (egui_phosphor::regular::WARNING, [0xF0, 0xB4, 0x3C]),
        Mark::Ok => (egui_phosphor::regular::CHECK_CIRCLE, [0x4C, 0xC3, 0x5E]),
    };
    let size = img.width() as f32 * 0.6;
    draw_icon_colored(img, font, glyph, size, color);
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

/// Base colour of the standby picture where neither light reaches.
const STANDBY_BASE: [f32; 3] = [8.0, 8.0, 12.0];
/// Where the two standby lights sit, as fractions of the panel.
const STANDBY_LIGHTS: [(f32, f32); 2] = [(0.075, 0.05), (0.925, 0.95)];
/// Radius of each light, in half panel heights.
const STANDBY_LIGHT_RADIUS: f32 = 1.05;

/// Deckly's own standby picture, an aurora: blue light from the top left
/// and violet from the bottom right, the logo's colours, meeting in indigo
/// over a near-black screen. The screen shows only through the key windows,
/// so it is one smooth picture with no centre that any grid can frame.
pub fn default_standby(model: &DeviceModel) -> RgbaImage {
    let (w, h) = model.panel_size();
    let unit = (h as f32 / 2.0).max(1.0);
    let lights = STANDBY_LIGHTS.map(|(fx, fy)| (fx * w as f32, fy * h as f32));
    let colors = [crate::logo::FROM, crate::logo::TO].map(|c| c.map(f32::from));
    RgbaImage::from_fn(w, h, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let mut light = [0.0f32; 3];
        for ((lx, ly), color) in lights.iter().zip(&colors) {
            // Distances in half panel heights keep each light round.
            let d2 = ((px - lx) / unit).powi(2) + ((py - ly) / unit).powi(2);
            let strength = (-d2 / (STANDBY_LIGHT_RADIUS * STANDBY_LIGHT_RADIUS)).exp();
            for (l, c) in light.iter_mut().zip(color) {
                *l += c * strength;
            }
        }
        // Soft tone curve so the overlap mixes instead of clipping.
        let mut out = [0u8; 4];
        for ((o, base), l) in out.iter_mut().zip(STANDBY_BASE).zip(light) {
            let v = base + 255.0 * (1.0 - (-l / 255.0 * 1.1).exp());
            *o = v.round().clamp(0.0, 255.0) as u8;
        }
        out[3] = 255;
        Rgba(out)
    })
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

fn draw_icon(img: &mut RgbaImage, font: &FontArc, glyph: &str, px: f32) {
    draw_icon_colored(img, font, glyph, px, ICON_COLOR);
}

fn draw_icon_colored(img: &mut RgbaImage, font: &FontArc, glyph: &str, px: f32, color: [u8; 3]) {
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
    let oy = ((h - b.height()) / 2.0).round() as i32;
    outline.draw(|x, y, cov| blend(img, ox + x as i32, oy + y as i32, color, cov));
}

/// The page number, large and centred, for Page Indicator keys.
fn draw_number(img: &mut RgbaImage, font: &FontArc, number: usize) {
    let size = img.width() as f32;
    let text = number.to_string();
    let scale = PxScale::from(size * if text.len() > 2 { 0.36 } else { 0.48 });
    let sf = font.as_scaled(scale);
    // Centre the digits' height (ascent only; digits have no descent).
    let baseline = (size + sf.ascent() * 0.72) / 2.0;
    draw_line(img, font, scale, &text, (0.0, baseline), ICON_COLOR);
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
    fn transparent_pixels_become_black() {
        let mut src = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 0]));
        for y in 24..40 {
            for x in 24..40 {
                src.put_pixel(x, y, Rgba([0, 200, 0, 255]));
            }
        }
        let out = fit_image(&src, 32);
        assert_eq!(out.get_pixel(1, 1).0, [0, 0, 0, 255]);
        assert_eq!(out.get_pixel(16, 16).0, [0, 200, 0, 255]);
    }

    #[test]
    fn renders_requested_sizes() {
        let mut r = Renderer::new();
        assert_eq!(
            r.render(None, 72, Live::default(), None).dimensions(),
            (72, 72)
        );
        let mut key = KeyConfig::new(actions::WEBSITE, json!({}));
        key.title = "Hello\nWorld".into();
        assert_eq!(
            r.render(Some(&key), 96, Live::default(), None).dimensions(),
            (96, 96)
        );
        assert_eq!(
            r.render(Some(&key), 0, Live::default(), None).dimensions(),
            (8, 8)
        );
    }

    #[test]
    fn missing_custom_image_falls_back_to_icon() {
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.image = Some("does/not/exist.png".into());
        let img = r.render(Some(&key), 72, Live::default(), None);
        assert_eq!(img.get_pixel(0, 0), &ICON_BACKGROUND);
    }

    #[test]
    fn device_images_fit_every_model() {
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.title = "Title".into();
        for model in DeviceModel::ALL {
            let data = r.device_image(
                Some(&key),
                Live::default(),
                None,
                model,
                Orientation::Standard,
            );
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
            let img = image::load_from_memory(&r.device_image(
                Some(&key),
                Live::default(),
                None,
                model,
                orientation,
            ))
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
    fn default_standby_is_an_aurora_on_every_model() {
        for model in DeviceModel::ALL {
            let img = default_standby(model);
            assert_eq!(img.dimensions(), model.panel_size(), "{}", model.name);
            let keys = cut_keys(&img, model);
            let mid = model.key_image_size / 2;
            let first = keys[0].get_pixel(mid, mid).0;
            let last = keys[keys.len() - 1].get_pixel(mid, mid).0;
            // Blue leads at the top left, violet at the bottom right.
            assert!(first[2] > first[0] && first[1] > first[0], "{}", model.name);
            assert!(last[0] > last[1] && last[2] > last[0], "{}", model.name);
            // No key window is left pitch black.
            for key in &keys {
                assert!(key.pixels().all(|p| p.0[2] >= 12), "{}", model.name);
            }
        }
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
    fn page_indicator_shows_the_live_page() {
        let mut r = Renderer::new();
        let key = KeyConfig::new(actions::PAGE_INDICATOR, json!({}));
        let one = Live::default();
        let two = Live { page: 2, ..one };
        assert_ne!(
            r.key_hash(Some(&key), one, None),
            r.key_hash(Some(&key), two, None)
        );
        assert_ne!(
            r.render(Some(&key), 72, one, None),
            r.render(Some(&key), 72, two, None)
        );
        // A three-digit page still fits.
        assert_eq!(
            r.render(Some(&key), 72, Live { page: 100, ..one }, None)
                .width(),
            72
        );
        // Other keys do not depend on the page.
        let other = KeyConfig::new(actions::HOTKEY, json!({}));
        assert_eq!(
            r.key_hash(Some(&other), one, None),
            r.key_hash(Some(&other), two, None)
        );
    }

    #[test]
    fn held_keys_are_drawn_smaller() {
        let mut r = Renderer::new();
        let path = std::env::temp_dir().join(format!("deckly-pressed-{}.png", std::process::id()));
        RgbaImage::from_pixel(80, 80, Rgba([255, 255, 255, 255]))
            .save(&path)
            .unwrap();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.image = Some(path.clone());
        let up = Live::default();
        let down = Live {
            pressed: true,
            ..up
        };
        assert_ne!(
            r.key_hash(Some(&key), up, None),
            r.key_hash(Some(&key), down, None)
        );
        let normal = r.render(Some(&key), 72, up, None);
        let held = r.render(Some(&key), 72, down, None);
        assert_eq!(held.dimensions(), (72, 72));
        // The picture fills the key normally and leaves a black frame held.
        assert_eq!(normal.get_pixel(1, 1).0, [255, 255, 255, 255]);
        assert_eq!(held.get_pixel(1, 1), &EMPTY);
        assert_eq!(held.get_pixel(36, 36).0, [255, 255, 255, 255]);
        // An empty key looks the same either way, so nothing is resent.
        assert_eq!(r.key_hash(None, up, None), r.key_hash(None, down, None));
        // A larger shrink leaves a wider frame.
        let frame = |img: &RgbaImage| {
            (0..36)
                .take_while(|&x| img.get_pixel(x, 36) == &EMPTY)
                .count()
        };
        let before = frame(&held);
        r.key_press.size = 60;
        assert_ne!(
            r.key_hash(Some(&key), down, None),
            r.key_hash(Some(&key), up, None)
        );
        assert!(frame(&r.render(Some(&key), 72, down, None)) > before);
        // With the effect off a held key looks like any other.
        r.key_press.shrink = false;
        assert_eq!(
            r.key_hash(Some(&key), up, None),
            r.key_hash(Some(&key), down, None)
        );
        assert_eq!(r.render(Some(&key), 72, down, None), normal);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn hash_tracks_visible_changes() {
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        let a = r.key_hash(Some(&key), Live::default(), None);
        key.title = "x".into();
        let b = r.key_hash(Some(&key), Live::default(), None);
        assert_ne!(a, b);
        assert_ne!(r.key_hash(None, Live::default(), None), b);
        // Keys without their own style change with the global one...
        r.title_defaults.size = 20;
        let c = r.key_hash(Some(&key), Live::default(), None);
        assert_ne!(b, c);
        // ...keys with their own style do not.
        key.title_style = Some(TitleStyle::default());
        let d = r.key_hash(Some(&key), Live::default(), None);
        r.title_defaults.size = 9;
        assert_eq!(d, r.key_hash(Some(&key), Live::default(), None));
    }
}

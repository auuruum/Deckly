//! Key image rendering, shared by the editor canvas and the device.
//!
//! A key is drawn from its configuration and the global title style: a
//! custom image or the action's icon, then the title on top. The same pixels go to the screen
//! (at a larger size) and to the device (as a JPEG at the key size).

use crate::actions;
use crate::animation::{self, Animation};
use crate::fonts::{self, Face};
use crate::plugins::{self, Mark, Overlay, PageTile};
use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use deckly_core::{
    DeviceModel, ImageFormat, KeyConfig, KeyPress, Orientation, Rotation, TitleAlign, TitleStyle,
    Turn, Wallpaper,
};
use image::{ImageReader, Limits, Rgba, RgbaImage, imageops};
use std::collections::HashMap;
use std::f32::consts::TAU;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const EMPTY: Rgba<u8> = Rgba([0, 0, 0, 255]);
const ICON_BACKGROUND: Rgba<u8> = Rgba([0, 0, 0, 255]);
const ICON_COLOR: [u8; 3] = [0xE6, 0xE7, 0xEA];
const RING_TRACK: [u8; 3] = [0x48, 0x4A, 0x50];
const MAX_IMAGE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TITLE_LINES: usize = 5;
/// Wallpapers are kept at most this large: more than any device panel or
/// the editor's largest grid needs.
pub const MAX_WALLPAPER_SIDE: u32 = 2048;
/// Decoded wallpapers kept; more start the cache over.
const MAX_WALLPAPERS: usize = 8;

/// Where a key sits under its page's wallpaper.
#[derive(Clone, Copy)]
pub struct Backdrop<'a> {
    pub wallpaper: &'a Wallpaper,
    pub model: &'a DeviceModel,
    /// The key, row by row as the user sees the keys.
    pub index: u8,
}

/// What a key shows that is not in its configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Live {
    /// The current page number, counted from 1.
    pub page: usize,
    /// The key is held down.
    pub pressed: bool,
    /// How far the folder's Auto Exit countdown is, in percent.
    pub exit: Option<u8>,
    /// The frame an animated key image shows; see [`Renderer::animate`].
    pub frame: u32,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            page: 1,
            pressed: false,
            exit: None,
            frame: 0,
        }
    }
}

/// A custom picture as read from its file.
enum Picture {
    Still(RgbaImage),
    Animated(Arc<Animation>),
    /// The file is there but cannot be shown; the key shows a warning.
    Broken,
    /// No such file: the key shows its action's icon.
    Missing,
}

impl Picture {
    fn load(path: &Path) -> Self {
        let broken = |e: String| {
            eprintln!("deckly: cannot load key image {}: {e}", path.display());
            if path.exists() {
                Picture::Broken
            } else {
                Picture::Missing
            }
        };
        if animation::may_animate(path) {
            match animation::load(path) {
                Ok(Some(animation)) => return Picture::Animated(animation),
                Ok(None) => {}
                // The first picture may still be readable.
                Err(e) => {
                    return decode_image(path).map_or_else(|_| broken(e), Picture::Still);
                }
            }
        }
        decode_image(path).map_or_else(broken, Picture::Still)
    }

    /// The picture to show at `frame`.
    fn at(&self, frame: u32) -> Option<&RgbaImage> {
        match self {
            Picture::Still(img) => Some(img),
            Picture::Animated(a) => a.frame(frame as usize),
            Picture::Broken | Picture::Missing => None,
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
    images: HashMap<PathBuf, Picture>,
    wallpapers: HashMap<PathBuf, Option<Arc<RgbaImage>>>,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            title_defaults: TitleStyle::default(),
            key_press: KeyPress::default(),
            title_font: load_title_font(),
            icon_font: FontArc::try_from_slice(egui_phosphor::Variant::Regular.font_bytes()).ok(),
            images: HashMap::new(),
            wallpapers: HashMap::new(),
        }
    }

    /// Forgets decoded custom images so changed files are read again.
    pub fn clear_image_cache(&mut self) {
        self.images.clear();
        self.wallpapers.clear();
        animation::clear_cache();
    }

    /// Sets the frame an animated key shows at `now` and returns when the
    /// next one is due. Keys that do not move get frame 0 and no time.
    pub fn animate(
        &mut self,
        key: Option<&KeyConfig>,
        live: Live,
        now: Instant,
    ) -> (Live, Option<Instant>) {
        let path = key.and_then(|k| k.image.as_deref());
        match path.map(|p| self.picture(p)) {
            Some(Picture::Animated(a)) => {
                let (frame, next) = a.at(now.saturating_duration_since(animation::clock()));
                let frame = u32::try_from(frame).unwrap_or(0);
                (Live { frame, ..live }, Some(now + next))
            }
            _ => (Live { frame: 0, ..live }, None),
        }
    }

    /// Draws a key. `overlay` is what its plugin shows, for plugin keys.
    pub fn render(
        &mut self,
        key: Option<&KeyConfig>,
        size: u32,
        live: Live,
        overlay: Option<&Overlay>,
    ) -> RgbaImage {
        self.render_on(key, size, live, overlay, None)
    }

    /// Draws a key over its piece of the page's wallpaper, if it has one.
    pub fn render_on(
        &mut self,
        key: Option<&KeyConfig>,
        size: u32,
        live: Live,
        overlay: Option<&Overlay>,
        backdrop: Option<Backdrop<'_>>,
    ) -> RgbaImage {
        let size = size.clamp(8, 1024);
        let tile = backdrop.and_then(|b| self.wallpaper_tile(b, size));
        let Some(key) = key else {
            return tile.unwrap_or_else(|| RgbaImage::from_pixel(size, size, EMPTY));
        };
        let style = self.title_style(key);
        let broken = key
            .image
            .as_deref()
            .is_some_and(|p| matches!(self.picture(p), Picture::Broken));
        let under = tile.as_ref();
        // The user's own image wins over the plugin's.
        let picture = key
            .image
            .as_deref()
            .and_then(|p| self.picture(p).at(live.frame))
            .map(|src| fit_over(src, size, under))
            .or_else(|| {
                overlay?
                    .image
                    .as_ref()
                    .map(|i| fit_over(&i.img, size, under))
            })
            .or_else(|| Some(self.page_picture(overlay?.page.as_ref()?, size)));
        let mut img = match picture {
            Some(img) => img,
            None => {
                let mut img = tile
                    .clone()
                    .unwrap_or_else(|| RgbaImage::from_pixel(size, size, ICON_BACKGROUND));
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
            && let Some(face) = fonts::face(&style.font, style.bold, style.italic)
        {
            draw_title(&mut img, &face, title, &style, size);
        }
        if key.action == actions::PARENT_FOLDER
            && let Some(percent) = live.exit
        {
            draw_ring(&mut img, percent);
        }
        let mark = overlay
            .and_then(|o| o.mark)
            .or(broken.then_some(Mark::Alert));
        if let Some(mark) = mark
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
        key.title_style
            .clone()
            .unwrap_or_else(|| self.title_defaults.clone())
    }

    /// Changes whenever anything that affects a key's pixels changes.
    pub fn key_hash(&self, key: Option<&KeyConfig>, live: Live, overlay: Option<&Overlay>) -> u64 {
        self.key_hash_on(key, live, overlay, None)
    }

    /// [`Self::key_hash`] for a key over a wallpaper.
    pub fn key_hash_on(
        &self,
        key: Option<&KeyConfig>,
        live: Live,
        overlay: Option<&Overlay>,
        backdrop: Option<Backdrop<'_>>,
    ) -> u64 {
        let mut h = DefaultHasher::new();
        if let Some(b) = backdrop {
            b.wallpaper.hash(&mut h);
            (b.model.columns, b.model.rows, b.model.key_image_size).hash(&mut h);
            b.model.panel_size().hash(&mut h);
            b.index.hash(&mut h);
        }
        if let Some(key) = key {
            overlay.hash(&mut h);
            key.action.hash(&mut h);
            key.title.hash(&mut h);
            self.title_style(key).hash(&mut h);
            key.image.hash(&mut h);
            live.frame.hash(&mut h);
            // Multimedia icons and the switch below depend on settings.
            if key.action == actions::MULTIMEDIA {
                actions::str_setting(&key.settings, "media").hash(&mut h);
            }
            if key.action == actions::PAGE_INDICATOR {
                live.page.hash(&mut h);
            }
            if key.action == actions::PARENT_FOLDER {
                live.exit.hash(&mut h);
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
    #[cfg(test)]
    pub fn device_image(
        &mut self,
        key: Option<&KeyConfig>,
        live: Live,
        overlay: Option<&Overlay>,
        model: &DeviceModel,
        orientation: Orientation,
    ) -> Vec<u8> {
        self.device_image_on(key, live, overlay, model, orientation, None)
    }

    /// [`Self::device_image`] for a key over a wallpaper.
    pub fn device_image_on(
        &mut self,
        key: Option<&KeyConfig>,
        live: Live,
        overlay: Option<&Overlay>,
        model: &DeviceModel,
        orientation: Orientation,
        backdrop: Option<Backdrop<'_>>,
    ) -> Vec<u8> {
        let img = self.render_on(key, model.key_image_size, live, overlay, backdrop);
        encode_key(img, model, orientation)
    }

    /// A page's wallpaper picture, decoded once.
    pub fn wallpaper(&mut self, path: &Path) -> Option<Arc<RgbaImage>> {
        if !self.wallpapers.contains_key(path) && self.wallpapers.len() >= MAX_WALLPAPERS {
            self.wallpapers.clear();
        }
        self.wallpapers
            .entry(path.to_owned())
            .or_insert_with(|| match decode_wallpaper(path) {
                Ok(img) => Some(Arc::new(img)),
                Err(e) => {
                    eprintln!("deckly: cannot load wallpaper {}: {e}", path.display());
                    None
                }
            })
            .clone()
    }

    fn wallpaper_tile(&mut self, backdrop: Backdrop<'_>, size: u32) -> Option<RgbaImage> {
        let src = self.wallpaper(&backdrop.wallpaper.image)?;
        wallpaper_tile(
            &src,
            backdrop.wallpaper,
            backdrop.model,
            backdrop.index,
            size,
        )
    }

    fn page_picture(&mut self, page: &PageTile, size: u32) -> RgbaImage {
        let icon = page
            .icon
            .as_deref()
            .and_then(|path| self.custom_image(path))
            .cloned();
        page_tile(icon.as_ref(), page.number, self.title_font.as_ref(), size)
    }

    fn picture(&mut self, path: &Path) -> &Picture {
        self.images
            .entry(path.to_owned())
            .or_insert_with(|| Picture::load(path))
    }

    /// A picture that does not move, such as a page icon: the first frame
    /// of an animation.
    fn custom_image(&mut self, path: &Path) -> Option<&RgbaImage> {
        self.picture(path).at(0)
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

/// [`fit_image`] over a background: transparent parts of the picture show
/// `under` instead of black.
fn fit_over(src: &RgbaImage, size: u32, under: Option<&RgbaImage>) -> RgbaImage {
    let Some(under) = under.filter(|u| u.dimensions() == (size, size)) else {
        return fit_image(src, size);
    };
    // Premultiplied, so hidden colours of transparent pixels do not bleed.
    let mut premultiplied = src.clone();
    for px in premultiplied.pixels_mut() {
        let a = u32::from(px.0[3]);
        for c in &mut px.0[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
    let scaled = imageops::resize(&premultiplied, size, size, imageops::FilterType::Lanczos3);
    let mut out = under.clone();
    for (o, s) in out.pixels_mut().zip(scaled.pixels()) {
        let keep = 255 - u32::from(s.0[3]);
        for (dst, &src) in o.0[..3].iter_mut().zip(&s.0[..3]) {
            let under = (u32::from(*dst) * keep + 127) / 255;
            *dst = (u32::from(src) + under).min(255) as u8;
        }
        o.0[3] = 255;
    }
    out
}

/// The part of a picture of `src` pixels a wallpaper shows across a panel
/// of `panel` pixels: left, top, width and height, in picture pixels.
pub fn wallpaper_view(src: (u32, u32), wallpaper: &Wallpaper, panel: (u32, u32)) -> [f32; 4] {
    let (sw, sh) = (src.0.max(1) as f32, src.1.max(1) as f32);
    let (pw, ph) = (panel.0.max(1) as f32, panel.1.max(1) as f32);
    // At the lowest zoom the picture just covers the panel.
    let cover = (pw / sw).max(ph / sh);
    let zoom = f32::from(
        wallpaper
            .zoom
            .clamp(Wallpaper::MIN_ZOOM, Wallpaper::MAX_ZOOM),
    ) / 100.0;
    let scale = cover * zoom;
    let (w, h) = ((pw / scale).min(sw), (ph / scale).min(sh));
    let end = f32::from(Wallpaper::END);
    let cx = f32::from(wallpaper.x.min(Wallpaper::END)) / end * sw;
    let cy = f32::from(wallpaper.y.min(Wallpaper::END)) / end * sh;
    let left = (cx - w / 2.0).clamp(0.0, sw - w);
    let top = (cy - h / 2.0).clamp(0.0, sh - h);
    [left, top, w, h]
}

/// One key's piece of a wallpaper at `size` pixels. The panel includes the
/// gaps between keys, so the picture runs on behind the frame between them.
pub fn wallpaper_tile(
    src: &RgbaImage,
    wallpaper: &Wallpaper,
    model: &DeviceModel,
    index: u8,
    size: u32,
) -> Option<RgbaImage> {
    let rect = *model.key_rects().get(usize::from(index))?;
    let panel = model.panel_size();
    let [left, top, w, h] = wallpaper_view(src.dimensions(), wallpaper, panel);
    let (fx, fy) = (w / panel.0.max(1) as f32, h / panel.1.max(1) as f32);
    let (x0, y0) = (left + rect.x as f32 * fx, top + rect.y as f32 * fy);
    let (x1, y1) = (x0 + rect.size as f32 * fx, y0 + rect.size as f32 * fy);
    let (sw, sh) = src.dimensions();
    let (cx, cy) = (x0.floor() as u32, y0.floor() as u32);
    if cx >= sw || cy >= sh {
        return None;
    }
    let cw = (x1.ceil() as u32).min(sw).saturating_sub(cx).max(1);
    let ch = (y1.ceil() as u32).min(sh).saturating_sub(cy).max(1);
    let piece = imageops::crop_imm(src, cx, cy, cw, ch).to_image();
    let size = size.clamp(1, 1024);
    Some(imageops::resize(
        &piece,
        size,
        size,
        imageops::FilterType::Triangle,
    ))
}

/// The whole panel of a wallpaper at `width` pixels, for the editor.
pub fn wallpaper_panel(
    src: &RgbaImage,
    wallpaper: &Wallpaper,
    model: &DeviceModel,
    width: u32,
) -> RgbaImage {
    let panel = model.panel_size();
    let width = width.clamp(1, MAX_WALLPAPER_SIDE);
    let height = ((width as f32 * panel.1 as f32 / panel.0.max(1) as f32).round() as u32).max(1);
    let [left, top, w, h] = wallpaper_view(src.dimensions(), wallpaper, panel);
    let (cx, cy) = (left.floor() as u32, top.floor() as u32);
    let cw = ((left + w).ceil() as u32)
        .min(src.width())
        .saturating_sub(cx)
        .max(1);
    let ch = ((top + h).ceil() as u32)
        .min(src.height())
        .saturating_sub(cy)
        .max(1);
    let piece = imageops::crop_imm(src, cx, cy, cw, ch).to_image();
    imageops::resize(&piece, width, height, imageops::FilterType::Triangle)
}

/// Reads a wallpaper and keeps it no larger than [`MAX_WALLPAPER_SIDE`].
pub fn decode_wallpaper(path: &Path) -> Result<RgbaImage, String> {
    let img = plugins::decode_picture(path)?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("the picture is empty".into());
    }
    let longest = w.max(h);
    if longest <= MAX_WALLPAPER_SIDE {
        return Ok(img);
    }
    let scale = MAX_WALLPAPER_SIDE as f32 / longest as f32;
    let (nw, nh) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    Ok(imageops::resize(
        &img,
        nw,
        nh,
        imageops::FilterType::Triangle,
    ))
}

/// A Go to Page key's picture: a white tile in the middle of the key with
/// the page's icon on it, or its number. Icons made of white lines turn black
/// to show on it. Sizes are the official app's, for a 72 px key.
fn page_tile(
    icon: Option<&RgbaImage>,
    number: usize,
    font: Option<&FontArc>,
    size: u32,
) -> RgbaImage {
    let unit = size as f32 / 72.0;
    let mut img = RgbaImage::from_pixel(size, size, ICON_BACKGROUND);
    let tile = ((28.0 * unit).round() as u32).clamp(1, size);
    let corner = (size - tile) / 2;
    let side = tile as f32;
    let radius = 3.0 * unit;
    for y in 0..tile {
        for x in 0..tile {
            // How far past the straight edges, towards a rounded corner.
            let past = |v: u32| (radius - v as f32 - 0.5).max(v as f32 + 0.5 - side + radius);
            let (dx, dy) = (past(x).max(0.0), past(y).max(0.0));
            let cover = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            blend(
                &mut img,
                (corner + x) as i32,
                (corner + y) as i32,
                [255; 3],
                cover,
            );
        }
    }
    let inner = ((22.0 * unit).round() as u32).clamp(1, tile);
    if let Some(icon) = icon {
        let mut scaled = imageops::resize(icon, inner, inner, imageops::FilterType::Lanczos3);
        let white_lines = icon
            .pixels()
            .all(|p| p.0[3] < 32 || p.0[..3].iter().all(|&c| c >= 200));
        if white_lines {
            for px in scaled.pixels_mut() {
                px.0[..3].fill(0);
            }
        }
        let at = i64::from((size - inner) / 2);
        imageops::overlay(&mut img, &scaled, at, at);
    } else if let Some(font) = font {
        let text = number.to_string();
        let px = inner as f32 * if text.len() > 2 { 0.56 } else { 0.8 };
        draw_centred(&mut img, font, &text, px, [0; 3]);
    }
    img
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

/// The Auto Exit countdown: a ring around the key's picture that fills
/// clockwise from the top.
fn draw_ring(img: &mut RgbaImage, percent: u8) {
    let size = img.width() as f32;
    let (center, radius, half_width) = (size / 2.0, size * 0.4, size * 0.03);
    let filled = f32::from(percent.min(100)) / 100.0 * TAU;
    for y in 0..img.height() {
        for x in 0..img.width() {
            let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
            let cover = half_width + 0.5 - ((dx * dx + dy * dy).sqrt() - radius).abs();
            if cover > 0.0 {
                let angle = dx.atan2(-dy).rem_euclid(TAU);
                let color = if angle <= filled {
                    ICON_COLOR
                } else {
                    RING_TRACK
                };
                blend(img, x as i32, y as i32, color, cover);
            }
        }
    }
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
    let img = plugins::decode_picture(path)?;
    // Square crop from the center, like "fill".
    let (w, h) = img.dimensions();
    let side = w.min(h);
    Ok(imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image())
}

/// The default title font, bold, as titles were drawn before they had a
/// font of their own.
pub(crate) fn load_title_font() -> Option<FontArc> {
    fonts::face("", true, false).map(|f| f.font)
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
    let text = number.to_string();
    let px = img.width() as f32 * if text.len() > 2 { 0.36 } else { 0.48 };
    draw_centred(img, font, &text, px, ICON_COLOR);
}

/// Digits in the middle of the image.
fn draw_centred(img: &mut RgbaImage, font: &FontArc, text: &str, px: f32, color: [u8; 3]) {
    let scale = PxScale::from(px);
    let sf = font.as_scaled(scale);
    // Centre the digits' height (ascent only; digits have no descent).
    let baseline = (img.width() as f32 + sf.ascent() * 0.72) / 2.0;
    draw_line(img, font, scale, text, (0.0, baseline), color);
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

fn draw_title(img: &mut RgbaImage, face: &Face, title: &str, style: &TitleStyle, size: u32) {
    let font = &face.font;
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
    let ink = Ink {
        embolden: if face.embolden {
            (scale.y / 24.0).max(0.5)
        } else {
            0.0
        },
        slant: if face.slant { SLANT } else { 0.0 },
        underline: style.underline,
    };
    let o = k.max(1.0) as i32;
    let mut mask = Coverage::new(img.width(), img.height());
    for (i, line) in lines.iter().enumerate() {
        let baseline = top + sf.ascent() + line_height * i as f32;
        mask.clear();
        mask.line(font, scale, line, baseline, ink);
        // A dark outline keeps titles readable on any image.
        for (dx, dy) in [(-o, 0), (o, 0), (0, -o), (0, o)] {
            mask.blend_onto(img, dx, dy, [0, 0, 0]);
        }
        mask.blend_onto(img, 0, 0, style.color);
    }
}

/// How far a slanted title leans: horizontal pixels per pixel of height.
const SLANT: f32 = 0.21;

/// What a title adds to its font's glyphs.
#[derive(Clone, Copy, Default)]
struct Ink {
    /// Extra stroke width in pixels, for a font without a bold face.
    embolden: f32,
    /// Shear for a font without an italic face.
    slant: f32,
    underline: bool,
}

/// How much of each pixel a line of text covers, so that its outline,
/// bolder strokes and underline are drawn once each.
struct Coverage {
    width: u32,
    height: u32,
    cover: Vec<f32>,
}

impl Coverage {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            cover: vec![0.0; width as usize * height as usize],
        }
    }

    fn clear(&mut self) {
        self.cover.fill(0.0);
    }

    fn put(&mut self, x: i32, y: i32, cover: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let i = y as usize * self.width as usize + x as usize;
        if let Some(v) = self.cover.get_mut(i) {
            *v = v.max(cover.clamp(0.0, 1.0));
        }
    }

    /// One line of text, centred across the image.
    fn line(&mut self, font: &FontArc, scale: PxScale, line: &str, baseline: f32, ink: Ink) {
        let sf = font.as_scaled(scale);
        let width = line_width(font, scale, line) + ink.embolden;
        let start = (self.width as f32 - width) / 2.0;
        let mut x = start;
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
                outline.draw(|gx, gy, cov| self.put(bx + gx as i32, by + gy as i32, cov));
            }
            x += sf.h_advance(id);
            prev = Some(id);
        }
        self.embolden(ink.embolden);
        // The shear turns around the middle of lowercase letters.
        self.shear(ink.slant, baseline - sf.ascent() * 0.35);
        if ink.underline {
            let thickness = (scale.y * 0.06).max(1.0);
            let top = baseline + (scale.y * 0.1).max(1.0);
            let (first, last) = (start.floor() as i32, (start + width).ceil() as i32);
            for y in top.floor() as i32..(top + thickness).ceil() as i32 {
                let row = (y as f32 + 1.0).min(top + thickness) - (y as f32).max(top);
                for x in first..last {
                    let column = (x as f32 + 1.0).min(start + width) - (x as f32).max(start);
                    self.put(x, y, row.clamp(0.0, 1.0) * column.clamp(0.0, 1.0));
                }
            }
        }
    }

    /// Widens strokes to the right by `amount` pixels, in steps of at most
    /// one pixel so that fractions stay smooth.
    fn embolden(&mut self, amount: f32) {
        if amount <= 0.0 {
            return;
        }
        let steps = amount.ceil().max(1.0);
        let step = amount / steps;
        let w = self.width as usize;
        for _ in 0..steps as u32 {
            for row in self.cover.chunks_mut(w.max(1)) {
                for x in (1..row.len()).rev() {
                    let moved = row[x] * (1.0 - step) + row[x - 1] * step;
                    row[x] = row[x].max(moved);
                }
            }
        }
    }

    /// Leans the text right by `slant` pixels per pixel above `pivot`.
    fn shear(&mut self, slant: f32, pivot: f32) {
        if slant == 0.0 {
            return;
        }
        let w = self.width as usize;
        let mut source = vec![0.0; w];
        for (y, row) in self.cover.chunks_mut(w.max(1)).enumerate() {
            let shift = (pivot - (y as f32 + 0.5)) * slant;
            let whole = shift.floor();
            let part = shift - whole;
            source.copy_from_slice(row);
            for (x, out) in row.iter_mut().enumerate() {
                let at = |i: f32| {
                    let i = x as f32 - whole - i;
                    if i >= 0.0 && i < w as f32 {
                        source[i as usize]
                    } else {
                        0.0
                    }
                };
                *out = at(0.0) * (1.0 - part) + at(1.0) * part;
            }
        }
    }

    /// Paints `color` where the text is, moved by `dx`, `dy`.
    fn blend_onto(&self, img: &mut RgbaImage, dx: i32, dy: i32, color: [u8; 3]) {
        for (i, &cover) in self.cover.iter().enumerate() {
            if cover > 0.0 {
                let x = (i % self.width as usize) as i32 + dx;
                let y = (i / self.width as usize) as i32 + dy;
                blend(img, x, y, color, cover);
            }
        }
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

    fn titled(style: TitleStyle) -> KeyConfig {
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.title = "Agh".into();
        key.title_style = Some(TitleStyle {
            align: TitleAlign::Middle,
            ..style
        });
        key
    }

    fn lit(img: &RgbaImage) -> usize {
        img.pixels().filter(|p| p.0[0] > 128).count()
    }

    #[test]
    fn font_options_change_the_pixels_and_the_hash() {
        let mut r = Renderer::new();
        let plain = titled(TitleStyle::default());
        let base = r.render(Some(&plain), 144, Live::default(), None);
        let base_hash = r.key_hash(Some(&plain), Live::default(), None);
        let variants = [
            TitleStyle {
                bold: false,
                ..TitleStyle::default()
            },
            TitleStyle {
                italic: true,
                ..TitleStyle::default()
            },
            TitleStyle {
                underline: true,
                ..TitleStyle::default()
            },
        ];
        for style in variants {
            let key = titled(style.clone());
            assert_ne!(r.render(Some(&key), 144, Live::default(), None), base);
            assert_ne!(r.key_hash(Some(&key), Live::default(), None), base_hash);
        }
        let underlined = titled(TitleStyle {
            underline: true,
            ..TitleStyle::default()
        });
        let img = r.render(Some(&underlined), 144, Live::default(), None);
        assert!(lit(&img) > lit(&base));
    }

    #[test]
    fn a_missing_font_family_draws_with_the_default_font() {
        let mut r = Renderer::new();
        let default = titled(TitleStyle::default());
        let missing = titled(TitleStyle {
            font: "No Such Font Family 1234".into(),
            ..TitleStyle::default()
        });
        assert_eq!(
            r.render(Some(&missing), 72, Live::default(), None),
            r.render(Some(&default), 72, Live::default(), None)
        );
        // Still a different style, so choosing the font re-renders the key.
        assert_ne!(
            r.key_hash(Some(&missing), Live::default(), None),
            r.key_hash(Some(&default), Live::default(), None)
        );
    }

    #[test]
    fn synthetic_bold_and_slant_stay_inside_the_key() {
        let face = fonts::face("", false, false).unwrap();
        let synthetic = Face {
            embolden: true,
            slant: true,
            ..face.clone()
        };
        let style = TitleStyle {
            size: TitleStyle::MAX_SIZE,
            underline: true,
            ..TitleStyle::default()
        };
        let long = "WWWWWWWWWWWWWWWWWWWW\nW\nW\nW\nW\nW\nW";
        for size in [8, 72, 1024] {
            let mut plain = RgbaImage::from_pixel(size, size, EMPTY);
            draw_title(&mut plain, &face, long, &style, size);
            let mut bold = RgbaImage::from_pixel(size, size, EMPTY);
            draw_title(&mut bold, &synthetic, long, &style, size);
            assert_eq!(bold.dimensions(), (size, size));
            if size >= 72 {
                assert!(lit(&bold) > lit(&plain));
            }
        }
    }

    #[test]
    fn animated_keys_show_the_frame_of_the_moment() {
        use crate::animation::testing;
        let path = testing::temp("key.gif");
        testing::gif(&path, 32, &[([255, 0, 0], 100), ([0, 0, 255], 100)]);
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.image = Some(path.clone());
        let start = animation::clock();
        let (first, next) = r.animate(Some(&key), Live::default(), start);
        assert_eq!(first.frame, 0);
        assert_eq!(next, Some(start + std::time::Duration::from_millis(100)));
        let later = start + std::time::Duration::from_millis(150);
        let (second, next) = r.animate(Some(&key), Live::default(), later);
        assert_eq!(second.frame, 1);
        assert_eq!(next, Some(start + std::time::Duration::from_millis(200)));
        assert_ne!(
            r.key_hash(Some(&key), first, None),
            r.key_hash(Some(&key), second, None)
        );
        let red = r.render(Some(&key), 72, first, None);
        let blue = r.render(Some(&key), 72, second, None);
        assert_eq!(red.get_pixel(4, 4).0, [255, 0, 0, 255]);
        assert_eq!(blue.get_pixel(4, 4).0, [0, 0, 255, 255]);
        for model in DeviceModel::ALL {
            let data = r.device_image(Some(&key), second, None, model, Orientation::Standard);
            let decoded = image::load_from_memory(&data).unwrap();
            assert_eq!(decoded.width(), model.key_image_size);
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn keys_that_do_not_move_ask_for_no_wakeups() {
        let mut r = Renderer::new();
        let still = KeyConfig::new(actions::HOTKEY, json!({}));
        let live = Live {
            frame: 7,
            ..Live::default()
        };
        let now = Instant::now();
        assert_eq!(r.animate(Some(&still), live, now), (Live::default(), None));
        assert_eq!(r.animate(None, live, now).1, None);
    }

    #[test]
    fn a_broken_picture_shows_a_warning() {
        use crate::animation::testing;
        let path = testing::temp("broken.gif");
        std::fs::write(&path, b"GIF89a broken").unwrap();
        let mut r = Renderer::new();
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.image = Some(path.clone());
        let (live, next) = r.animate(Some(&key), Live::default(), Instant::now());
        assert_eq!(next, None);
        let img = r.render(Some(&key), 72, live, None);
        let amber = img
            .pixels()
            .filter(|p| p.0[0] > 200 && p.0[1] > 140 && p.0[2] < 100)
            .count();
        assert!(amber > 20, "no warning sign drawn");
        // A missing file is not broken: the action's icon, no warning.
        key.image = Some(testing::temp("missing.gif"));
        let img = r.render(Some(&key), 72, live, None);
        assert_eq!(
            img.pixels()
                .filter(|p| p.0[0] > 200 && p.0[1] > 140 && p.0[2] < 100)
                .count(),
            0
        );
        let _ = std::fs::remove_file(path);
    }

    /// A picture whose red channel counts across and green down.
    fn ramp(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            Rgba([
                (x * 255 / w.max(2)) as u8,
                (y * 255 / h.max(2)) as u8,
                0,
                255,
            ])
        })
    }

    #[test]
    fn the_wallpaper_view_covers_the_panel_and_stays_inside_the_picture() {
        let mut wall = Wallpaper::new("w.png".into());
        // A square picture on a wide panel: full width, a band of height.
        let [l, t, w, h] = wallpaper_view((1000, 1000), &wall, (480, 272));
        assert_eq!((l, w), (0.0, 1000.0));
        assert!((h - 1000.0 * 272.0 / 480.0).abs() < 0.01);
        assert!((t - (1000.0 - h) / 2.0).abs() < 0.01);
        wall.zoom = 200;
        let [_, _, w2, h2] = wallpaper_view((1000, 1000), &wall, (480, 272));
        assert!((w2 - 500.0).abs() < 0.01 && (h2 - h / 2.0).abs() < 0.01);
        // Far corners stop at the picture's edges.
        wall.x = 0;
        wall.y = Wallpaper::END;
        let [l, t, w, h] = wallpaper_view((1000, 1000), &wall, (480, 272));
        assert_eq!(l, 0.0);
        assert!((t + h - 1000.0).abs() < 0.01 && w <= 1000.0);
        // Out-of-range values and empty sizes do not escape either.
        let wild = Wallpaper {
            zoom: 0,
            x: u16::MAX,
            y: u16::MAX,
            ..Wallpaper::new("w.png".into())
        };
        let [l, t, w, h] = wallpaper_view((0, 0), &wild, (0, 0));
        assert!(l >= 0.0 && t >= 0.0 && l + w <= 1.0 && t + h <= 1.0);
    }

    #[test]
    fn wallpapers_split_across_the_keys_of_every_model() {
        let src = ramp(960, 540);
        let wall = Wallpaper::new("w.png".into());
        for model in DeviceModel::ALL {
            let count = model.key_count();
            let tiles: Vec<RgbaImage> = (0..count)
                .map(|i| wallpaper_tile(&src, &wall, model, i, 72).unwrap())
                .collect();
            assert!(
                tiles.iter().all(|t| t.dimensions() == (72, 72)),
                "{}",
                model.name
            );
            assert!(wallpaper_tile(&src, &wall, model, count, 72).is_none());
            let at = |i: u8| tiles[usize::from(i)].get_pixel(36, 36).0;
            let cols = model.columns;
            for i in 0..count {
                // Left to right the picture runs on; top to bottom too.
                if i % cols + 1 < cols {
                    assert!(at(i)[0] < at(i + 1)[0], "{} key {i}", model.name);
                }
                if i + cols < count && model.rows > 1 {
                    assert!(at(i)[1] < at(i + cols)[1], "{} key {i}", model.name);
                }
            }
        }
    }

    #[test]
    fn keys_draw_over_their_piece_of_the_wallpaper() {
        let path = std::env::temp_dir().join(format!("deckly-wall-{}.png", uuid::Uuid::new_v4()));
        RgbaImage::from_pixel(400, 300, Rgba([20, 160, 90, 255]))
            .save(&path)
            .unwrap();
        let wall = Wallpaper::new(path.clone());
        let model = DeviceModel::of(deckly_core::DeviceKind::Mk2);
        let backdrop = |index| {
            Some(Backdrop {
                wallpaper: &wall,
                model,
                index,
            })
        };
        let mut r = Renderer::new();
        // Empty keys and action icons show it; the title still draws.
        let empty = r.render_on(None, 72, Live::default(), None, backdrop(0));
        assert_eq!(empty.get_pixel(2, 2).0, [20, 160, 90, 255]);
        let mut key = KeyConfig::new(actions::HOTKEY, json!({}));
        key.title = "Hi".into();
        let drawn = r.render_on(Some(&key), 72, Live::default(), None, backdrop(1));
        assert_eq!(drawn.get_pixel(2, 2).0, [20, 160, 90, 255]);
        assert_ne!(drawn, empty);
        // A see-through key picture lets it through; an opaque one covers it.
        let half = std::env::temp_dir().join(format!("deckly-half-{}.png", uuid::Uuid::new_v4()));
        let mut picture = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 0]));
        for y in 0..64 {
            for x in 32..64 {
                picture.put_pixel(x, y, Rgba([200, 0, 0, 255]));
            }
        }
        picture.save(&half).unwrap();
        key.title.clear();
        key.image = Some(half.clone());
        let mixed = r.render_on(Some(&key), 72, Live::default(), None, backdrop(2));
        assert_eq!(mixed.get_pixel(4, 36).0, [20, 160, 90, 255]);
        assert_eq!(mixed.get_pixel(68, 36).0, [200, 0, 0, 255]);
        // The piece, the page's wallpaper and its place all change the hash.
        let plain = r.key_hash(Some(&key), Live::default(), None);
        let first = r.key_hash_on(Some(&key), Live::default(), None, backdrop(2));
        let other = r.key_hash_on(Some(&key), Live::default(), None, backdrop(3));
        assert!(plain != first && first != other);
        let moved = Wallpaper {
            x: 0,
            ..wall.clone()
        };
        let moved_hash = r.key_hash_on(
            Some(&key),
            Live::default(),
            None,
            Some(Backdrop {
                wallpaper: &moved,
                model,
                index: 2,
            }),
        );
        assert_ne!(moved_hash, first);
        // Device images keep their size and format.
        let data = r.device_image_on(
            None,
            Live::default(),
            None,
            model,
            Orientation::Standard,
            backdrop(14),
        );
        let decoded = image::load_from_memory(&data).unwrap();
        assert_eq!(decoded.width(), model.key_image_size);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(half);
    }

    #[test]
    fn a_missing_wallpaper_leaves_the_keys_as_they_were() {
        let wall = Wallpaper::new("does/not/exist.png".into());
        let model = DeviceModel::of(deckly_core::DeviceKind::Mk2);
        let backdrop = Some(Backdrop {
            wallpaper: &wall,
            model,
            index: 0,
        });
        let mut r = Renderer::new();
        let key = KeyConfig::new(actions::HOTKEY, json!({}));
        assert_eq!(
            r.render_on(Some(&key), 72, Live::default(), None, backdrop),
            r.render(Some(&key), 72, Live::default(), None)
        );
        assert_eq!(
            r.render_on(None, 72, Live::default(), None, backdrop),
            r.render(None, 72, Live::default(), None)
        );
    }

    #[test]
    fn large_wallpapers_are_kept_small() {
        let path = std::env::temp_dir().join(format!("deckly-big-{}.png", uuid::Uuid::new_v4()));
        RgbaImage::from_pixel(4000, 100, Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let img = decode_wallpaper(&path).unwrap();
        assert_eq!(img.width(), MAX_WALLPAPER_SIDE);
        assert_eq!(img.height(), 51);
        let panel = wallpaper_panel(
            &img,
            &Wallpaper::new(path.clone()),
            &DeviceModel::ALL[0],
            380,
        );
        assert_eq!(panel.width(), 380);
        let _ = std::fs::remove_file(path);
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
    fn resetting_custom_image_restores_builtin_and_live_plugin_pixels() {
        let path = std::env::temp_dir().join(format!("deckly-reset-{}.png", uuid::Uuid::new_v4()));
        RgbaImage::from_pixel(288, 288, Rgba([150, 30, 40, 255]))
            .save(&path)
            .unwrap();
        let mut renderer = Renderer::new();
        for action in [actions::HOTKEY, "test.plugin.action"] {
            let mut key = KeyConfig::new(action, json!({"keep":"settings"}));
            let plugin = (action == "test.plugin.action").then(|| Overlay {
                image: Some(std::sync::Arc::new(plugins::KeyImage {
                    id: 17,
                    img: RgbaImage::from_pixel(72, 72, Rgba([10, 60, 200, 255])),
                })),
                ..Default::default()
            });
            let default = renderer.render(Some(&key), 72, Live::default(), plugin.as_ref());
            let default_hash = renderer.key_hash(Some(&key), Live::default(), plugin.as_ref());
            key.image = Some(path.clone());
            let custom = renderer.render(Some(&key), 72, Live::default(), plugin.as_ref());
            assert_ne!(custom, default);
            assert_ne!(
                renderer.key_hash(Some(&key), Live::default(), plugin.as_ref()),
                default_hash
            );
            key.image = None;
            assert_eq!(
                renderer.render(Some(&key), 72, Live::default(), plugin.as_ref()),
                default
            );
            assert_eq!(
                renderer.key_hash(Some(&key), Live::default(), plugin.as_ref()),
                default_hash
            );
            assert_eq!(key.settings, json!({"keep":"settings"}));
        }
        let _ = std::fs::remove_file(path);
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
    fn a_page_icon_sits_dark_on_a_white_tile() {
        let mut icon = RgbaImage::from_pixel(32, 32, Rgba([255, 255, 255, 0]));
        for x in 0..32 {
            for y in 12..20 {
                icon.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let img = page_tile(Some(&icon), 1, None, 72);
        assert_eq!(*img.get_pixel(2, 2), ICON_BACKGROUND);
        // The tile above the icon's bar, and the bar itself, turned black.
        assert_eq!(*img.get_pixel(36, 24), Rgba([255, 255, 255, 255]));
        assert!(img.get_pixel(36, 36).0[0] < 64);
        // A picture in colour keeps them.
        let photo = RgbaImage::from_pixel(32, 32, Rgba([200, 30, 30, 255]));
        assert!(page_tile(Some(&photo), 1, None, 72).get_pixel(36, 36).0[0] > 150);
    }

    #[test]
    fn a_page_without_a_usable_icon_shows_its_number() {
        let mut r = Renderer::new();
        let key = KeyConfig::new(actions::GO_TO_PAGE, json!({"page": 2}));
        let page = |number, icon: Option<&str>| Overlay {
            page: Some(PageTile {
                number,
                icon: icon.map(PathBuf::from),
            }),
            ..Overlay::default()
        };
        let mut draw = |overlay: &Overlay| r.render(Some(&key), 72, Live::default(), Some(overlay));
        let two = draw(&page(2, None));
        assert_eq!(*two.get_pixel(25, 25), Rgba([255, 255, 255, 255]));
        assert_ne!(two, draw(&page(3, None)));
        assert_eq!(two, draw(&page(2, Some("no/such/icon.svg"))));
    }

    #[test]
    fn a_key_with_its_own_image_shows_no_tile() {
        let mut r = Renderer::new();
        let dir = std::env::temp_dir().join(format!("deckly-tile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("own.png");
        RgbaImage::from_pixel(8, 8, Rgba([10, 200, 10, 255]))
            .save(&file)
            .unwrap();
        let mut key = KeyConfig::new(actions::GO_TO_PAGE, json!({"page": 2}));
        key.image = Some(file);
        let overlay = Overlay {
            page: Some(PageTile {
                number: 2,
                icon: None,
            }),
            ..Overlay::default()
        };
        let img = r.render(Some(&key), 72, Live::default(), Some(&overlay));
        assert!(img.get_pixel(36, 36).0[1] > 150);
        std::fs::remove_dir_all(&dir).unwrap();
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
    fn the_exit_key_shows_its_countdown_as_a_ring() {
        let mut r = Renderer::new();
        let key = KeyConfig::new(actions::PARENT_FOLDER, json!({}));
        let other = KeyConfig::new(actions::HOTKEY, json!({}));
        let at = |percent| Live {
            exit: Some(percent),
            ..Live::default()
        };
        assert_ne!(
            r.key_hash(Some(&key), at(40), None),
            r.key_hash(Some(&key), at(41), None)
        );
        assert_ne!(
            r.key_hash(Some(&key), Live::default(), None),
            r.key_hash(Some(&key), at(0), None)
        );
        let bright = |img: &RgbaImage, x, y| img.get_pixel(x, y).0[0] > 128;
        // It fills clockwise from the top, so half of it is the right side.
        let half = r.render(Some(&key), 72, at(50), None);
        assert!(bright(&half, 64, 36));
        assert!(!bright(&half, 7, 36));
        assert!(bright(&r.render(Some(&key), 72, at(100), None), 7, 36));
        // Other keys do not depend on the countdown.
        assert_eq!(
            r.key_hash(Some(&other), Live::default(), None),
            r.key_hash(Some(&other), at(50), None)
        );
        assert_eq!(
            r.render(Some(&other), 72, Live::default(), None),
            r.render(Some(&other), 72, at(50), None)
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

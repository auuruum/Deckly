//! Bounded, platform-independent raster composition. Drafts are temporary;
//! applying one stores a PNG in the existing serializable key image field.

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use image::{ImageReader, Limits, Pixel, Rgba, RgbaImage, imageops};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

pub const SIZE: u32 = 288;
pub const MAX_LAYERS: usize = 8;
pub const MAX_TEXT: usize = 100;
const MAX_FILE: u64 = 16 * 1024 * 1024;

#[derive(Clone)]
pub enum Content {
    Icon(&'static str, &'static str),
    Text(String),
    Picture(String, Arc<RgbaImage>),
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Icon(name, glyph), Self::Icon(other_name, other_glyph)) => {
                name == other_name && glyph == other_glyph
            }
            (Self::Text(text), Self::Text(other_text)) => text == other_text,
            // Pictures are immutable. Snapshots share their pixels, so a
            // draft comparison never needs to scan image buffers per frame.
            (Self::Picture(name, image), Self::Picture(other_name, other_image)) => {
                name == other_name && Arc::ptr_eq(image, other_image)
            }
            _ => false,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Layer {
    pub content: Content,
    /// Centre and bounding box size as fractions of the square canvas.
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub color: [u8; 3],
}

impl Layer {
    pub fn new(content: Content) -> Self {
        Self {
            content,
            x: 0.5,
            y: 0.5,
            size: 0.65,
            color: [230, 231, 234],
        }
    }

    pub fn name(&self) -> &str {
        match &self.content {
            Content::Icon(name, _) => name,
            Content::Text(text) => text,
            Content::Picture(name, _) => name,
        }
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct Composition {
    /// Pure black by default, matching built-in keys. A near-black color
    /// leaves a visible background around the glyph on the hardware.
    pub background: [u8; 3],
    /// Back to front.
    pub layers: Vec<Layer>,
}

impl Composition {
    pub fn add(&mut self, layer: Layer) -> Result<usize, String> {
        if self.layers.len() >= MAX_LAYERS {
            return Err(format!("An image can have at most {MAX_LAYERS} layers."));
        }
        self.layers.push(layer);
        Ok(self.layers.len() - 1)
    }

    pub fn render(&self, fonts: &Fonts) -> Result<RgbaImage, String> {
        if self.layers.len() > MAX_LAYERS {
            return Err("too many layers".into());
        }
        let [r, g, b] = self.background;
        let mut out = RgbaImage::from_pixel(SIZE, SIZE, Rgba([r, g, b, 255]));
        for layer in &self.layers {
            if ![layer.x, layer.y, layer.size].iter().all(|v| v.is_finite()) {
                return Err("invalid layer position or size".into());
            }
            let raster = match &layer.content {
                Content::Icon(_, glyph) => lettering(&fonts.icon, glyph, layer.color)?,
                Content::Text(text) => {
                    let font = fonts.text.as_ref().ok_or("no text font is available")?;
                    lettering(font, text, layer.color)?
                }
                Content::Picture(_, image) => {
                    if image.width().max(image.height()) > 512 || image.is_empty() {
                        return Err("invalid picture size".into());
                    }
                    image.as_ref().clone()
                }
            };
            let side = (layer.size.clamp(0.05, 1.5) * SIZE as f32).round() as u32;
            let (w, h) = fitted(raster.dimensions(), side);
            // Premultiply before filtering so hidden RGB in transparent logos
            // cannot bleed a white fringe into the result.
            let raster = resize_alpha(&raster, w, h);
            let x = (layer.x.clamp(0.0, 1.0) * SIZE as f32 - w as f32 / 2.0).round();
            let y = (layer.y.clamp(0.0, 1.0) * SIZE as f32 - h as f32 / 2.0).round();
            imageops::overlay(&mut out, &raster, x as i64, y as i64);
            // The background is opaque. Integer alpha compositing may round
            // 255 down to 254; keep every intermediate canvas opaque too.
            for p in out.pixels_mut() {
                p.0[3] = 255;
            }
        }
        Ok(out)
    }
}

pub struct Fonts {
    icon: FontArc,
    text: Option<FontArc>,
}

impl Fonts {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            icon: FontArc::try_from_slice(egui_phosphor::Variant::Regular.font_bytes())
                .map_err(|e| e.to_string())?,
            text: crate::render::load_title_font(),
        })
    }
}

pub fn fitted((w, h): (u32, u32), side: u32) -> (u32, u32) {
    let scale = side as f32 / w.max(h).max(1) as f32;
    (
        (w as f32 * scale).round().max(1.0) as u32,
        (h as f32 * scale).round().max(1.0) as u32,
    )
}

fn resize_alpha(src: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let mut premultiplied = src.clone();
    for p in premultiplied.pixels_mut() {
        let alpha = u32::from(p.0[3]);
        for c in &mut p.0[..3] {
            *c = ((u32::from(*c) * alpha + 127) / 255) as u8;
        }
    }
    let mut resized = imageops::resize(&premultiplied, w, h, imageops::FilterType::Triangle);
    for p in resized.pixels_mut() {
        let alpha = u32::from(p.0[3]);
        for c in &mut p.0[..3] {
            *c = (u32::from(*c) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
    resized
}

/// Imports only a single frame. File bytes, decoder allocations, dimensions
/// and the retained layer raster are all bounded before reaching the UI.
pub fn load_picture(path: &Path) -> Result<RgbaImage, String> {
    if !std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("choose a regular image file".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("choose a regular image file".into());
    }
    if metadata.len() > MAX_FILE {
        return Err("image file is larger than 16 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("image file is larger than 16 MiB".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
    let (w, h) = fitted(
        image.dimensions(),
        image.width().max(image.height()).min(512),
    );
    Ok(resize_alpha(&image, w, h))
}

fn lettering(font: &FontArc, text: &str, color: [u8; 3]) -> Result<RgbaImage, String> {
    if text.chars().count() > MAX_TEXT {
        return Err("text is too long".into());
    }
    let lines: Vec<_> = text.lines().take(4).collect();
    if text.lines().count() > 4 {
        return Err("text can have at most four lines".into());
    }
    let mut outlines = Vec::new();
    let scale = PxScale::from(64.0);
    let scaled = font.as_scaled(scale);
    let width = |line: &str| {
        line.chars()
            .map(|c| scaled.h_advance(scaled.glyph_id(c)))
            .sum::<f32>()
    };
    let max_width = lines.iter().map(|l| width(l)).fold(0.0, f32::max);
    let mut min = point(f32::INFINITY, f32::INFINITY);
    let mut max = point(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (row, line) in lines.iter().enumerate() {
        let mut x = (max_width - width(line)) / 2.0;
        let baseline = scaled.ascent() + row as f32 * 76.0;
        for c in line.chars() {
            let id = scaled.glyph_id(c);
            if id.0 == 0 && !c.is_whitespace() {
                return Err("this font cannot draw one of the characters".into());
            }
            if let Some(outline) =
                font.outline_glyph(id.with_scale_and_position(scale, point(x, baseline)))
            {
                let bounds = outline.px_bounds();
                min.x = min.x.min(bounds.min.x);
                min.y = min.y.min(bounds.min.y);
                max.x = max.x.max(bounds.max.x);
                max.y = max.y.max(bounds.max.y);
                outlines.push(outline);
            }
            x += scaled.h_advance(id);
        }
    }
    if outlines.is_empty() {
        return Ok(RgbaImage::new(1, 1));
    }
    // 100 glyphs at a fixed 64 px font remain below these dimensions.
    let w = ((max.x - min.x).ceil() as u32).clamp(1, 16384);
    let h = ((max.y - min.y).ceil() as u32).clamp(1, 512);
    let mut image = RgbaImage::new(w, h);
    for outline in outlines {
        let bounds = outline.px_bounds();
        let dx = (bounds.min.x - min.x).round() as u32;
        let dy = (bounds.min.y - min.y).round() as u32;
        outline.draw(|x, y, coverage| {
            if let Some(p) = image.get_pixel_mut_checked(x + dx, y + dy) {
                let [r, g, b] = color;
                p.blend(&Rgba([r, g, b, (coverage * 255.0).round() as u8]));
            }
        });
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(color: [u8; 4]) -> Layer {
        let mut layer = Layer::new(Content::Picture(
            "test".into(),
            Arc::new(RgbaImage::from_pixel(20, 10, Rgba(color))),
        ));
        layer.size = 0.5;
        layer
    }

    #[test]
    fn default_icon_background_matches_builtin_previews_png_and_device_output() {
        use crate::render::{Live, Renderer};
        use deckly_core::{DeviceModel, KeyConfig, Orientation};

        let mut draft = Composition::default();
        draft
            .add(Layer::new(Content::Icon(
                "Arrows out line horizontal",
                egui_phosphor::regular::ARROWS_OUT_LINE_HORIZONTAL,
            )))
            .unwrap();
        let image = draft.render(&Fonts::new().unwrap()).unwrap();
        let key = KeyConfig::new(crate::actions::TEXT, serde_json::json!({}));
        let mut renderer = Renderer::new();
        let default = renderer.render(Some(&key), 72, Live::default(), None);
        assert_eq!(image.get_pixel(0, 0), default.get_pixel(0, 0));
        assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 255]);

        let path = std::env::temp_dir().join(format!(
            "deckly-black-background-{}.png",
            uuid::Uuid::new_v4()
        ));
        image.save(&path).unwrap();
        assert_eq!(crate::render::decode_file(&path).unwrap(), image);
        let mut key = key;
        key.image = Some(path.clone());
        for size in [72, 96, SIZE] {
            let preview = renderer.render(Some(&key), size, Live::default(), None);
            assert_eq!(preview.get_pixel(0, 0).0, [0, 0, 0, 255]);
        }
        for model in DeviceModel::ALL {
            let bytes = renderer.device_image(
                Some(&key),
                Live::default(),
                None,
                model,
                Orientation::Standard,
            );
            let decoded = image::load_from_memory(&bytes).unwrap().into_rgba8();
            // JPEG can introduce a small rounding error; BMP remains exact.
            assert!(
                decoded.get_pixel(0, 0).0[..3].iter().all(|&c| c <= 2),
                "{}",
                model.name
            );
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn composition_preserves_aspect_ratio_alpha_and_layer_order() {
        let mut draft = Composition {
            background: [10, 20, 30],
            ..Default::default()
        };
        draft.add(picture([200, 0, 0, 255])).unwrap();
        draft.add(picture([0, 200, 0, 128])).unwrap();
        let fonts = Fonts::new().unwrap();
        let image = draft.render(&fonts).unwrap();
        assert_eq!(image.dimensions(), (SIZE, SIZE));
        assert_eq!(image.get_pixel(144, 100).0, [10, 20, 30, 255]);
        let centre = image.get_pixel(144, 144).0;
        assert!((98..=101).contains(&centre[0]));
        assert!((99..=102).contains(&centre[1]));
        assert_eq!(centre[3], 255);
        draft.layers.swap(0, 1);
        assert_eq!(
            draft.render(&fonts).unwrap().get_pixel(144, 144).0,
            [200, 0, 0, 255]
        );
    }

    #[test]
    fn moving_and_clipping_are_bounded() {
        let mut draft = Composition::default();
        let mut layer = picture([255, 0, 0, 255]);
        layer.x = 0.0;
        layer.y = 0.0;
        draft.add(layer).unwrap();
        let fonts = Fonts::new().unwrap();
        let image = draft.render(&fonts).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_ne!(image.get_pixel(144, 144).0, [255, 0, 0, 255]);
        draft.layers[0].size = f32::NAN;
        assert!(draft.render(&fonts).is_err());
        draft.layers[0].size = f32::MAX;
        assert_eq!(draft.render(&fonts).unwrap().dimensions(), (SIZE, SIZE));
    }

    #[test]
    fn caps_layers_text_and_invalid_files() {
        let mut draft = Composition::default();
        for _ in 0..MAX_LAYERS {
            draft.add(picture([0; 4])).unwrap();
        }
        assert!(draft.add(picture([0; 4])).is_err());
        let fonts = Fonts::new().unwrap();
        assert!(
            lettering(
                fonts.text.as_ref().unwrap(),
                &"a".repeat(MAX_TEXT + 1),
                [255; 3]
            )
            .is_err()
        );
        assert!(load_picture(Path::new("/no-such-deckly-image.png")).is_err());
    }

    #[test]
    fn catalog_glyphs_and_multiline_text_render() {
        let fonts = Fonts::new().unwrap();
        for (_, glyph) in egui_phosphor::regular::ICONS {
            let image = lettering(&fonts.icon, glyph, [255; 3]).unwrap();
            assert!(image.pixels().any(|p| p.0[3] != 0));
        }
        let image = lettering(fonts.text.as_ref().unwrap(), "Bim\nBim", [20, 60, 100]).unwrap();
        assert!(image.height() > 76);
    }

    #[test]
    fn transparent_rgb_does_not_leave_a_fringe() {
        let mut image = RgbaImage::from_pixel(2, 1, Rgba([255, 255, 255, 0]));
        image.put_pixel(0, 0, Rgba([200, 0, 0, 255]));
        let scaled = resize_alpha(&image, 8, 4);
        assert!(scaled.pixels().all(|p| p.0[1] == 0 && p.0[2] == 0));
    }

    #[test]
    fn imports_bound_files_and_keep_their_own_pixels() {
        let dir = std::env::temp_dir().join(format!("deckly-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("logo.png");
        RgbaImage::from_pixel(1024, 512, Rgba([40, 80, 120, 128]))
            .save(&path)
            .unwrap();
        let imported = load_picture(&path).unwrap();
        assert_eq!(imported.dimensions(), (512, 256));
        std::fs::remove_file(&path).unwrap();
        let mut draft = Composition::default();
        draft
            .add(Layer::new(Content::Picture(
                "Logo".into(),
                Arc::new(imported),
            )))
            .unwrap();
        assert!(draft.render(&Fonts::new().unwrap()).is_ok());
        std::fs::write(&path, b"not an image").unwrap();
        assert!(load_picture(&path).is_err());
        File::create(&path).unwrap().set_len(MAX_FILE + 1).unwrap();
        assert!(load_picture(&path).unwrap_err().contains("16 MiB"));
        RgbaImage::new(4097, 1).save(&path).unwrap();
        assert!(load_picture(&path).is_err());
        assert!(load_picture(&dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

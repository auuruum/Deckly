//! The Customize Image window: place a picture over a device's keys by
//! zooming and dragging it, then hand back a picture the size of the
//! device's panel. Used for screensavers and standby images. The layout
//! follows the official window
//! (docs/clean-room/2026-10-26-orientation-screensaver-standby.md).

use crate::logo;
use crate::prefs::{button, checkbox};
use crate::render;
use crate::theme::*;
use deckly_core::DeviceModel;
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, Rect, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2, pos2, vec2,
};
use egui_phosphor::regular as icon;
use image::{Rgba, RgbaImage, imageops};
use std::path::Path;

const SIZE: Vec2 = vec2(800.0, 690.0);
const TOP_BAR: f32 = 40.0;
const BOTTOM_BAR: f32 = 40.0;
const FRAME_WIDTH: f32 = 520.0;
const FRAME_MAX_HEIGHT: f32 = 420.0;
/// Picture edges this close to a frame edge snap to it, in window points.
const SNAP: f32 = 8.0;
/// Pictures are shown at most this large; the result uses the full picture.
const MAX_TEXTURE: u32 = 4096;
const BAR: Color32 = Color32::from_rgb(0x2A, 0x2A, 0x2A);
const BAR_LINE: Color32 = Color32::from_rgb(0x28, 0x28, 0x28);
const DROP_ZONE: Color32 = Color32::from_rgb(0x21, 0x21, 0x21);
const HINT: Color32 = Color32::from_rgb(0x4B, 0x4B, 0x4B);
const KEY_OUTLINE: Color32 = Color32::from_rgba_premultiplied(0x80, 0x80, 0x80, 0x80);

/// What the picture is for, with the serial of the device it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Purpose {
    Screensaver(String),
    Standby(String),
}

/// Where the picture sits on the panel: its top-left corner in panel pixels
/// and how many panel pixels one picture pixel covers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
}

impl Placement {
    /// Covers the whole panel, centred, like "fill".
    pub fn fill(picture: (u32, u32), panel: (u32, u32)) -> Self {
        let (sx, sy) = ratios(picture, panel);
        Self::centred(picture, panel, sx.max(sy))
    }

    fn centred(picture: (u32, u32), panel: (u32, u32), scale: f32) -> Self {
        Self {
            x: (panel.0 as f32 - picture.0 as f32 * scale) / 2.0,
            y: (panel.1 as f32 - picture.1 as f32 * scale) / 2.0,
            scale,
        }
    }

    /// Zooms to `scale`, keeping the panel point `anchor` still.
    fn zoom(&mut self, scale: f32, (ax, ay): (f32, f32)) {
        let f = scale / self.scale;
        self.x = ax - (ax - self.x) * f;
        self.y = ay - (ay - self.y) * f;
        self.scale = scale;
    }

    /// Moves picture edges within `tolerance` panel pixels onto the panel's
    /// edges.
    fn snap(&mut self, picture: (u32, u32), panel: (u32, u32), tolerance: f32) {
        let snap_axis = |pos: &mut f32, len: f32, extent: f32| {
            if pos.abs() < tolerance {
                *pos = 0.0;
            } else if (*pos + len - extent).abs() < tolerance {
                *pos = extent - len;
            }
        };
        snap_axis(&mut self.x, picture.0 as f32 * self.scale, panel.0 as f32);
        snap_axis(&mut self.y, picture.1 as f32 * self.scale, panel.1 as f32);
    }
}

/// Panel size divided by picture size, per axis.
fn ratios(picture: (u32, u32), panel: (u32, u32)) -> (f32, f32) {
    (
        panel.0 as f32 / picture.0.max(1) as f32,
        panel.1 as f32 / picture.1.max(1) as f32,
    )
}

/// Zoom limits: from a quarter of "fit inside" to eight times "fill".
fn scale_range(picture: (u32, u32), panel: (u32, u32)) -> (f32, f32) {
    let (sx, sy) = ratios(picture, panel);
    (sx.min(sy) / 4.0, (sx.max(sy) * 8.0).max(4.0))
}

/// Draws the placed picture onto a black panel of `panel` pixels.
pub fn render_crop(img: &RgbaImage, p: Placement, panel: (u32, u32)) -> RgbaImage {
    let (pw, ph) = panel;
    let mut out = RgbaImage::from_pixel(pw, ph, Rgba([0, 0, 0, 255]));
    let (iw, ih) = img.dimensions();
    if iw == 0 || ih == 0 || !p.scale.is_finite() || p.scale <= 0.0 {
        return out;
    }
    // The visible part in panel pixels...
    let left = p.x.max(0.0);
    let top = p.y.max(0.0);
    let right = (p.x + iw as f32 * p.scale).min(pw as f32);
    let bottom = (p.y + ih as f32 * p.scale).min(ph as f32);
    let (dw, dh) = ((right - left).round(), (bottom - top).round());
    if dw < 1.0 || dh < 1.0 {
        return out;
    }
    // ...and in picture pixels.
    let to_src = |v: f32, origin: f32| (v - origin) / p.scale;
    let sx = (to_src(left, p.x).floor().max(0.0) as u32).min(iw - 1);
    let sy = (to_src(top, p.y).floor().max(0.0) as u32).min(ih - 1);
    let ex = (to_src(right, p.x).ceil() as u32).clamp(sx + 1, iw);
    let ey = (to_src(bottom, p.y).ceil() as u32).clamp(sy + 1, ih);
    let part = imageops::crop_imm(img, sx, sy, ex - sx, ey - sy).to_image();
    let part = imageops::resize(&part, dw as u32, dh as u32, imageops::FilterType::Triangle);
    // Transparent parts of the picture show black.
    imageops::overlay(&mut out, &part, left.round() as i64, top.round() as i64);
    out
}

struct Picture {
    image: RgbaImage,
    texture: TextureHandle,
}

struct Job {
    purpose: Purpose,
    model: &'static DeviceModel,
    picture: Option<Picture>,
    place: Placement,
    snap: bool,
    show_keys: bool,
    error: Option<String>,
}

enum Action {
    Set,
    Cancel,
}

#[derive(Default)]
pub struct Cropper {
    job: Option<Job>,
}

impl Cropper {
    /// Opens the window for a new picture, or brings it to the front.
    pub fn open(&mut self, ctx: &egui::Context, purpose: Purpose, model: &'static DeviceModel) {
        if self.job.is_some() {
            ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Focus);
        }
        self.job = Some(Job {
            purpose,
            model,
            picture: None,
            place: Placement {
                x: 0.0,
                y: 0.0,
                scale: 1.0,
            },
            snap: true,
            show_keys: true,
            error: None,
        });
    }

    /// Shows the window while open. Returns the finished panel picture
    /// once "Set Image" is pressed.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<(Purpose, RgbaImage)> {
        let job = self.job.as_mut()?;
        let builder = egui::ViewportBuilder::default()
            .with_title("Customize Image - Deckly")
            .with_icon(logo::icon(64))
            .with_inner_size(SIZE)
            .with_min_inner_size(SIZE)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_minimize_button(false)
            .with_drag_and_drop(true);
        let action = ctx.show_viewport_immediate(viewport_id(), builder, |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                return Some(Action::Cancel);
            }
            egui::CentralPanel::no_frame()
                .show(ui, |ui| job.contents(ui))
                .inner
        });
        match action? {
            Action::Cancel => {
                self.job = None;
                None
            }
            Action::Set => {
                let job = self.job.take()?;
                let picture = job.picture?;
                let panel = job.model.panel_size();
                Some((job.purpose, render_crop(&picture.image, job.place, panel)))
            }
        }
    }
}

impl Job {
    fn contents(&mut self, ui: &mut Ui) -> Option<Action> {
        let o = ui.max_rect().min;
        let full = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect_filled(full, CornerRadius::ZERO, CANVAS);
        let top = Rect::from_min_size(o, vec2(full.width(), TOP_BAR));
        let bottom = Rect::from_min_max(pos2(full.left(), full.bottom() - BOTTOM_BAR), full.max);
        let body = Rect::from_min_max(top.left_bottom(), bottom.right_top());
        let panel = self.model.panel_size();

        self.load_dropped(ui);
        let frame = frame_rect(body, panel);
        // Window points per panel pixel.
        let k = frame.width() / panel.0.max(1) as f32;
        self.drag_and_zoom(ui, body, frame, k);
        self.draw_body(ui, body, frame, k);

        // Top bar: snap, key overlay, zoom, fit buttons.
        painter.rect_filled(top, CornerRadius::ZERO, BAR);
        painter.hline(top.x_range(), top.bottom(), Stroke::new(1.0, BAR_LINE));
        checkbox(
            ui,
            Rect::from_min_size(o + vec2(17.0, 10.0), vec2(120.0, 20.0)),
            &mut self.snap,
            "Snap to edges",
        );
        let has_picture = self.picture.is_some();
        let keys_icon = if self.show_keys {
            icon::EYE
        } else {
            icon::EYE_SLASH
        };
        if tool_button(ui, o + vec2(488.0, 8.0), keys_icon, true)
            .on_hover_text("Show/Hide keys")
            .clicked()
        {
            self.show_keys = !self.show_keys;
        }
        let size = self.picture.as_ref().map(|p| p.image.dimensions());
        if let Some(size) = size {
            let (lo, hi) = scale_range(size, panel);
            let mut scale = self.place.scale;
            let slider = Rect::from_min_size(o + vec2(522.0, 10.0), vec2(150.0, 20.0));
            let resp = ui
                .scope_builder(egui::UiBuilder::new().max_rect(slider), |ui| {
                    ui.spacing_mut().slider_width = slider.width();
                    ui.add(
                        egui::Slider::new(&mut scale, lo..=hi)
                            .logarithmic(true)
                            .show_value(false),
                    )
                })
                .inner;
            if resp.changed() {
                let centre = (panel.0 as f32 / 2.0, panel.1 as f32 / 2.0);
                self.place.zoom(scale, centre);
            }
        }
        // Align to width, to height, original size.
        let fits = [
            (697.0, icon::ARROWS_OUT_LINE_HORIZONTAL, "Align to width"),
            (731.0, icon::ARROWS_OUT_LINE_VERTICAL, "Align to height"),
            (765.0, icon::NUMBER_SQUARE_ONE, "Original size"),
        ];
        for (x, glyph, tip) in fits {
            let clicked = tool_button(ui, o + vec2(x, 8.0), glyph, size.is_some())
                .on_hover_text(tip)
                .clicked();
            if let (true, Some(size)) = (clicked, size) {
                let (sx, sy) = ratios(size, panel);
                let scale = match glyph {
                    icon::ARROWS_OUT_LINE_HORIZONTAL => sx,
                    icon::ARROWS_OUT_LINE_VERTICAL => sy,
                    _ => 1.0,
                };
                self.place = Placement::centred(size, panel, scale);
            }
        }

        // Bottom bar.
        painter.rect_filled(bottom, CornerRadius::ZERO, BAR);
        painter.hline(bottom.x_range(), bottom.top(), Stroke::new(1.0, BAR_LINE));
        let by = bottom.top() + 9.0;
        if button(
            ui,
            Rect::from_min_size(pos2(o.x + 16.0, by), vec2(108.0, 22.0)),
            "Image from file…",
        )
        .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "bmp"])
                .pick_file()
        {
            self.load(ui.ctx(), &path);
        }
        if let Some(e) = &self.error {
            painter.text(
                pos2(o.x + 136.0, by + 11.0),
                Align2::LEFT_CENTER,
                e,
                font(12.0),
                WARNING,
            );
        }
        let set = ui
            .add_enabled_ui(has_picture, |ui| {
                button(
                    ui,
                    Rect::from_min_size(pos2(full.right() - 152.0, by), vec2(62.0, 22.0)),
                    "Set Image",
                )
            })
            .inner;
        if set.clicked() {
            return Some(Action::Set);
        }
        if button(
            ui,
            Rect::from_min_size(pos2(full.right() - 80.0, by), vec2(64.0, 22.0)),
            "Cancel",
        )
        .clicked()
        {
            return Some(Action::Cancel);
        }
        None
    }

    fn draw_body(&self, ui: &Ui, body: Rect, frame: Rect, k: f32) {
        let painter = ui.painter();
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        painter.rect_filled(frame, CornerRadius::same(10), DROP_ZONE);
        match &self.picture {
            None => {
                painter.text(
                    frame.center(),
                    Align2::CENTER_CENTER,
                    "Drop Image here",
                    font(17.0),
                    HINT,
                );
            }
            Some(pic) => {
                let (w, h) = pic.image.dimensions();
                let rect = Rect::from_min_size(
                    frame.min + vec2(self.place.x, self.place.y) * k,
                    vec2(w as f32, h as f32) * self.place.scale * k,
                );
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                // The part outside the frame is shown faded.
                painter.with_clip_rect(body).image(
                    pic.texture.id(),
                    rect,
                    uv,
                    Color32::from_white_alpha(60),
                );
                painter
                    .with_clip_rect(frame)
                    .image(pic.texture.id(), rect, uv, Color32::WHITE);
            }
        }
        if self.show_keys {
            for r in self.model.key_rects() {
                let key = Rect::from_min_size(
                    frame.min + vec2(r.x as f32, r.y as f32) * k,
                    Vec2::splat(r.size as f32 * k),
                );
                painter.rect_stroke(
                    key,
                    CornerRadius::same(6),
                    Stroke::new(1.0, KEY_OUTLINE),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if hovering {
            painter.rect_stroke(
                frame,
                CornerRadius::same(10),
                Stroke::new(2.0, ACCENT),
                egui::StrokeKind::Inside,
            );
        }
    }

    fn drag_and_zoom(&mut self, ui: &Ui, body: Rect, frame: Rect, k: f32) {
        let Some(size) = self.picture.as_ref().map(|p| p.image.dimensions()) else {
            return;
        };
        let panel = self.model.panel_size();
        let resp = ui.interact(body, ui.id().with("cropper-body"), Sense::drag());
        if resp.dragged() {
            let d = resp.drag_delta() / k;
            self.place.x += d.x;
            self.place.y += d.y;
        }
        if resp.drag_stopped() && self.snap {
            self.place.snap(size, panel, SNAP / k);
        }
        if resp.hovered() {
            let scroll = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let (lo, hi) = scale_range(size, panel);
                let scale = (self.place.scale * (scroll * 0.002).exp()).clamp(lo, hi);
                let anchor = resp
                    .hover_pos()
                    .map(|p| ((p - frame.min) / k).into())
                    .unwrap_or((panel.0 as f32 / 2.0, panel.1 as f32 / 2.0));
                self.place.zoom(scale, anchor);
            }
        }
    }

    fn load_dropped(&mut self, ui: &Ui) {
        let dropped = ui
            .ctx()
            .input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            self.load(ui.ctx(), &path);
        }
    }

    fn load(&mut self, ctx: &egui::Context, path: &Path) {
        match render::decode_file(path) {
            Ok(image) => {
                let shown = if image.width().max(image.height()) > MAX_TEXTURE {
                    let f = MAX_TEXTURE as f32 / image.width().max(image.height()) as f32;
                    let (w, h) = (
                        ((image.width() as f32 * f) as u32).max(1),
                        ((image.height() as f32 * f) as u32).max(1),
                    );
                    imageops::resize(&image, w, h, imageops::FilterType::Triangle)
                } else {
                    image.clone()
                };
                let size = [shown.width() as usize, shown.height() as usize];
                let texture = ctx.load_texture(
                    "cropper-picture",
                    ColorImage::from_rgba_unmultiplied(size, shown.as_raw()),
                    TextureOptions::LINEAR,
                );
                self.place = Placement::fill(image.dimensions(), self.model.panel_size());
                self.picture = Some(Picture { image, texture });
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Cannot open the image: {e}")),
        }
    }
}

/// The frame showing the panel, centred in the body at a fixed width (or
/// height for tall panels).
fn frame_rect(body: Rect, (pw, ph): (u32, u32)) -> Rect {
    let aspect = ph.max(1) as f32 / pw.max(1) as f32;
    let mut size = vec2(FRAME_WIDTH, FRAME_WIDTH * aspect);
    if size.y > FRAME_MAX_HEIGHT {
        size = vec2(FRAME_MAX_HEIGHT / aspect, FRAME_MAX_HEIGHT);
    }
    Rect::from_center_size(body.center(), size)
}

fn tool_button(ui: &mut Ui, at: egui::Pos2, glyph: &str, enabled: bool) -> egui::Response {
    let rect = Rect::from_min_size(at, Vec2::splat(24.0));
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, ui.id().with(("cropper-tool", glyph)), sense);
    let color = if !enabled {
        TEXT_FAINT
    } else if resp.hovered() {
        TEXT
    } else {
        TEXT_DIM
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        font(20.0),
        color,
    );
    resp
}

fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("deckly-customize-image")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_covers_the_panel() {
        let p = Placement::fill((1000, 1000), (480, 272));
        assert_eq!(p.scale, 0.48);
        assert_eq!((p.x, p.y), (0.0, -104.0));
        let p = Placement::fill((100, 50), (480, 272));
        assert!(p.scale * 100.0 >= 480.0 && p.scale * 50.0 >= 272.0);
    }

    #[test]
    fn crop_has_panel_size_and_places_the_picture() {
        let mut img = RgbaImage::from_pixel(100, 100, Rgba([255, 255, 255, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let panel = (480, 272);
        // Unscaled at (10, 20): the rest of the panel stays black.
        let out = render_crop(
            &img,
            Placement {
                x: 10.0,
                y: 20.0,
                scale: 1.0,
            },
            panel,
        );
        assert_eq!(out.dimensions(), panel);
        assert_eq!(out.get_pixel(0, 0), &Rgba([0, 0, 0, 255]));
        assert_eq!(out.get_pixel(10, 20), &Rgba([255, 0, 0, 255]));
        assert_eq!(out.get_pixel(60, 60), &Rgba([255, 255, 255, 255]));
        assert_eq!(out.get_pixel(200, 200), &Rgba([0, 0, 0, 255]));
        // Partly off the panel to the top left.
        let out = render_crop(
            &img,
            Placement {
                x: -50.0,
                y: -50.0,
                scale: 2.0,
            },
            panel,
        );
        assert_eq!(out.get_pixel(0, 0), &Rgba([255, 255, 255, 255]));
        assert_eq!(out.get_pixel(160, 160), &Rgba([0, 0, 0, 255]));
    }

    #[test]
    fn crop_survives_odd_placements() {
        let img = RgbaImage::new(10, 10);
        let panel = (480, 272);
        for p in [
            Placement {
                x: 1e9,
                y: 0.0,
                scale: 1.0,
            },
            Placement {
                x: 0.0,
                y: 0.0,
                scale: 0.0,
            },
            Placement {
                x: 0.0,
                y: 0.0,
                scale: f32::NAN,
            },
            Placement {
                x: -1e9,
                y: -1e9,
                scale: 1e-9,
            },
        ] {
            assert_eq!(render_crop(&img, p, panel).dimensions(), panel);
        }
        assert_eq!(
            render_crop(&RgbaImage::new(0, 0), Placement::fill((0, 0), panel), panel).dimensions(),
            panel
        );
    }

    #[test]
    fn snapping_pulls_edges_to_the_frame() {
        let mut p = Placement {
            x: 3.0,
            y: -2.0,
            scale: 1.0,
        };
        p.snap((500, 300), (480, 272), 5.0);
        assert_eq!((p.x, p.y), (0.0, 0.0));
        // Right edge: 500 wide picture at -17 ends at 483, near 480.
        let mut p = Placement {
            x: -17.0,
            y: -100.0,
            scale: 1.0,
        };
        p.snap((500, 300), (480, 272), 5.0);
        assert_eq!(p.x, -20.0);
        assert_eq!(p.y, -100.0);
    }

    #[test]
    fn zoom_keeps_the_anchor() {
        let mut p = Placement {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
        };
        p.zoom(2.0, (100.0, 50.0));
        assert_eq!((p.x, p.y, p.scale), (-100.0, -50.0, 2.0));
    }
}

//! The Wallpaper window: choose a picture for the profile, page or folder
//! and place it across the keys by dragging and zooming
//! (docs/clean-room/2026-11-04-key-wallpaper.md).

use crate::render::{self, Renderer};
use crate::store;
use crate::theme::*;
use deckly_core::{DeviceModel, Page, Wallpaper};
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2, pos2, vec2,
};
use egui_phosphor::regular as icon;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Width of the picture of the keys, in points.
const PREVIEW_WIDTH: f32 = 380.0;
/// Zoom change for one notch of the mouse wheel, in percent.
const WHEEL_ZOOM: f32 = 0.25;
const GAP_SHADE: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 150);

pub struct WallpaperEditor {
    pub profile: String,
    pub page: String,
    editing_profile: bool,
    job: Option<Receiver<Result<PathBuf, String>>>,
    error: Option<String>,
    preview: Option<(u64, TextureHandle)>,
    /// Counts presses, so that each drag or slide is one undo step.
    gesture: u64,
}

impl WallpaperEditor {
    pub fn new(profile: &str, page: &str) -> Self {
        Self {
            profile: profile.to_owned(),
            page: page.to_owned(),
            editing_profile: false,
            job: None,
            error: None,
            preview: None,
            gesture: 0,
        }
    }

    /// Shows the window over `page`. Returns `None` once it is closed,
    /// else the undo group of this frame's changes.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        profile_wallpaper: &mut Option<Wallpaper>,
        page: &mut Page,
        scope: &str,
        model: &DeviceModel,
        renderer: &mut Renderer,
    ) -> Option<u64> {
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.gesture = self.gesture.wrapping_add(1);
        }
        if let Some(wallpaper) = self.receive() {
            if self.editing_profile {
                *profile_wallpaper = Some(wallpaper);
            } else {
                page.wallpaper = Some(wallpaper);
                page.wallpaper_disabled = false;
            }
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new("Wallpaper")
            .id(egui::Id::new(("wallpaper", &self.page)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(PREVIEW_WIDTH);
                let busy = self.job.is_some();
                ui.add_enabled_ui(!busy, |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.editing_profile, false, scope);
                        ui.selectable_value(&mut self.editing_profile, true, "Profile");
                    });
                });
                let (shown, wallpaper) = if self.editing_profile {
                    (profile_wallpaper.clone(), &mut *profile_wallpaper)
                } else {
                    (
                        page.effective_wallpaper(profile_wallpaper.as_ref())
                            .cloned(),
                        &mut page.wallpaper,
                    )
                };
                self.preview(ui, shown.as_ref(), wallpaper.as_mut(), model, renderer);
                ui.add_space(8.0);
                if let Some(wallpaper) = wallpaper {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Zoom").color(TEXT_DIM));
                        ui.spacing_mut().slider_width = PREVIEW_WIDTH - 110.0;
                        ui.add(
                            egui::Slider::new(
                                &mut wallpaper.zoom,
                                Wallpaper::MIN_ZOOM..=Wallpaper::MAX_ZOOM,
                            )
                            .suffix("%"),
                        );
                    });
                    ui.label(
                        RichText::new(
                            "Drag the picture to move it. Key pictures cover it; their \
                             transparent parts let it through.",
                        )
                        .color(TEXT_FAINT),
                    );
                }
                let status = if self.editing_profile {
                    "Applies to pages and folders that use the profile wallpaper."
                } else if page.wallpaper_disabled {
                    "Wallpaper is off here."
                } else if page.wallpaper.is_some() {
                    "This layout uses its own wallpaper."
                } else if shown.is_some() {
                    "This layout uses the profile wallpaper."
                } else {
                    "This layout follows the profile, which has no wallpaper yet."
                };
                ui.label(RichText::new(status).color(TEXT_DIM));
                if !self.editing_profile {
                    ui.horizontal(|ui| {
                        let inherits = !page.wallpaper_disabled && page.wallpaper.is_none();
                        if ui
                            .add_enabled(!busy && !inherits, egui::Button::new("Use Profile"))
                            .clicked()
                        {
                            page.wallpaper = None;
                            page.wallpaper_disabled = false;
                            self.error = None;
                        }
                        if ui
                            .add_enabled(
                                !busy && !page.wallpaper_disabled,
                                egui::Button::new("No Wallpaper"),
                            )
                            .clicked()
                        {
                            page.wallpaper = None;
                            page.wallpaper_disabled = true;
                            self.error = None;
                        }
                    });
                }
                if let Some(error) = &self.error {
                    ui.label(RichText::new(format!("{}  {error}", icon::WARNING)).color(WARNING));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let choose = ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(format!("{}  Choose Image…", icon::IMAGE)),
                        )
                        .clicked();
                    if choose {
                        self.choose(ctx);
                    }
                    if busy {
                        ui.spinner();
                    }
                    if self.editing_profile
                        && ui
                            .add_enabled(
                                profile_wallpaper.is_some() && !busy,
                                egui::Button::new(format!("{}  Remove", icon::TRASH)),
                            )
                            .clicked()
                    {
                        *profile_wallpaper = None;
                        self.error = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = ui.button("Done").clicked();
                    });
                });
            });
        if !open || close {
            return None;
        }
        Some(egui::Id::new(("wallpaper-edit", &self.page, self.gesture)).value())
    }

    /// The keys as the device lays them out, with the wallpaper behind them.
    fn preview(
        &mut self,
        ui: &mut egui::Ui,
        shown: Option<&Wallpaper>,
        editable: Option<&mut Wallpaper>,
        model: &DeviceModel,
        renderer: &mut Renderer,
    ) {
        let (pw, ph) = model.panel_size();
        let height = PREVIEW_WIDTH * ph as f32 / pw.max(1) as f32;
        let (rect, resp) =
            ui.allocate_exact_size(vec2(PREVIEW_WIDTH, height), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(4), KEY_EMPTY);
        let picture = shown.and_then(|w| Some((w, renderer.wallpaper(&w.image)?)));
        if let Some((wallpaper, src)) = &picture {
            let px = (PREVIEW_WIDTH * ui.ctx().pixels_per_point()).round() as u32;
            let mut h = DefaultHasher::new();
            (wallpaper, px, pw, ph).hash(&mut h);
            let hash = h.finish();
            let texture = match &self.preview {
                Some((shown, texture)) if *shown == hash => texture.id(),
                _ => {
                    let img = render::wallpaper_panel(src, wallpaper, model, px);
                    let size = [img.width() as usize, img.height() as usize];
                    let color = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    let texture = ui
                        .ctx()
                        .load_texture("wallpaper", color, TextureOptions::LINEAR);
                    let id = texture.id();
                    self.preview = Some((hash, texture));
                    id
                }
            };
            painter.image(
                texture,
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else if shown.is_some() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "The picture cannot be read",
                egui::FontId::proportional(13.0),
                WARNING,
            );
        }
        // Shade what lies between the keys and outline the keys.
        let k = PREVIEW_WIDTH / pw.max(1) as f32;
        let keys: Vec<Rect> = model
            .key_rects()
            .iter()
            .map(|r| {
                Rect::from_min_size(
                    rect.min + vec2(r.x as f32, r.y as f32) * k,
                    Vec2::splat(r.size as f32 * k),
                )
            })
            .collect();
        if picture.is_some() {
            shade_outside(&painter, rect, &keys);
        }
        for key in &keys {
            painter.rect_stroke(
                *key,
                CornerRadius::same(3),
                Stroke::new(1.0, KEY_BORDER),
                egui::StrokeKind::Outside,
            );
        }
        let Some((_, src)) = picture else {
            return;
        };
        let Some(wallpaper) = editable else {
            return;
        };
        if resp.dragged() {
            let delta = resp.drag_delta();
            let [_, _, w, h] = render::wallpaper_view(src.dimensions(), wallpaper, (pw, ph));
            // Dragging moves the picture, so the view moves the other way:
            // `span` preview points show `shown` of `whole` picture pixels.
            let end = f32::from(Wallpaper::END);
            let shift = |at: u16, by: f32, shown: f32, span: f32, whole: u32| {
                let moved = -by / span.max(1.0) * shown / whole.max(1) as f32 * end;
                (f32::from(at) + moved).round().clamp(0.0, end) as u16
            };
            let (sw, sh) = src.dimensions();
            wallpaper.x = shift(wallpaper.x, delta.x, w, PREVIEW_WIDTH, sw);
            wallpaper.y = shift(wallpaper.y, delta.y, h, height, sh);
            clamp_to_view(wallpaper, src.dimensions(), (pw, ph));
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let zoom = f32::from(wallpaper.zoom) * (1.0 + WHEEL_ZOOM * scroll / 50.0);
                wallpaper.zoom = zoom.round().clamp(
                    f32::from(Wallpaper::MIN_ZOOM),
                    f32::from(Wallpaper::MAX_ZOOM),
                ) as u16;
                clamp_to_view(wallpaper, src.dimensions(), (pw, ph));
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    fn choose(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Images",
                &["png", "jpg", "jpeg", "webp", "gif", "bmp", "svg"],
            )
            .pick_file()
        else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let profile = self.profile.clone();
        let repaint = ctx.clone();
        // Large pictures take a moment; the window stays responsive.
        let spawned = std::thread::Builder::new()
            .name("deckly-wallpaper".into())
            .spawn(move || {
                let saved = render::decode_wallpaper(&path)
                    .and_then(|img| store::save_wallpaper(&profile, &img));
                let _ = tx.send(saved);
                repaint.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.job = Some(rx);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Cannot read the picture: {e}")),
        }
    }

    fn receive(&mut self) -> Option<Wallpaper> {
        let job = self.job.as_ref()?;
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("reading the picture stopped".into()),
        };
        self.job = None;
        match result {
            Ok(path) => {
                self.error = None;
                Some(Wallpaper::new(path))
            }
            Err(e) => {
                self.error = Some(format!("Cannot use this picture: {e}"));
                None
            }
        }
    }
}

/// Moves the view back inside the picture, as the renderer shows it, so a
/// later drag starts from what is on screen.
fn clamp_to_view(wallpaper: &mut Wallpaper, src: (u32, u32), panel: (u32, u32)) {
    let [left, top, w, h] = render::wallpaper_view(src, wallpaper, panel);
    let end = f32::from(Wallpaper::END);
    let middle = |start: f32, shown: f32, whole: u32| {
        ((start + shown / 2.0) / whole.max(1) as f32 * end)
            .round()
            .clamp(0.0, end) as u16
    };
    wallpaper.x = middle(left, w, src.0);
    wallpaper.y = middle(top, h, src.1);
}

/// Darkens the panel outside the keys, where the device's frame is.
fn shade_outside(painter: &egui::Painter, panel: Rect, keys: &[Rect]) {
    let mut rows: Vec<(f32, f32)> = keys.iter().map(|k| (k.top(), k.bottom())).collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.dedup_by(|a, b| (a.0 - b.0).abs() < 0.5);
    let mut y = panel.top();
    for &(top, bottom) in &rows {
        if top > y {
            painter.rect_filled(
                Rect::from_x_y_ranges(panel.x_range(), y..=top),
                0,
                GAP_SHADE,
            );
        }
        let mut x = panel.left();
        let mut row: Vec<&Rect> = keys
            .iter()
            .filter(|k| (k.top() - top).abs() < 0.5)
            .collect();
        row.sort_by(|a, b| a.left().total_cmp(&b.left()));
        for key in row {
            if key.left() > x {
                painter.rect_filled(
                    Rect::from_x_y_ranges(x..=key.left(), top..=bottom),
                    0,
                    GAP_SHADE,
                );
            }
            x = key.right();
        }
        if panel.right() > x {
            painter.rect_filled(
                Rect::from_x_y_ranges(x..=panel.right(), top..=bottom),
                0,
                GAP_SHADE,
            );
        }
        y = bottom;
    }
    if panel.bottom() > y {
        painter.rect_filled(
            Rect::from_x_y_ranges(panel.x_range(), y..=panel.bottom()),
            0,
            GAP_SHADE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_is_brought_back_inside_the_picture() {
        // A square picture on a wide panel fills its width at 100 %, so it
        // cannot move sideways, only up and down.
        let mut wall = Wallpaper {
            x: 0,
            y: 0,
            ..Wallpaper::new("w.png".into())
        };
        clamp_to_view(&mut wall, (1000, 1000), (480, 272));
        assert_eq!(wall.x, Wallpaper::END / 2);
        let [_, top, _, h] = render::wallpaper_view((1000, 1000), &wall, (480, 272));
        assert_eq!(top, 0.0);
        assert_eq!(wall.y, ((h / 2.0) / 1000.0 * 1000.0).round() as u16);
        // Clamping again changes nothing.
        let before = wall.clone();
        clamp_to_view(&mut wall, (1000, 1000), (480, 272));
        assert_eq!(wall, before);
    }
}

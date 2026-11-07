//! The Wallpaper window: choose a picture for the profile, page or folder
//! and place it across the keys by dragging and zooming
//! (docs/clean-room/2026-11-04-key-wallpaper.md).

use crate::render::{self, Renderer};
use crate::store;
use crate::theme::*;
use deckly_core::{
    DeviceModel, Page, Wallpaper, WallpaperChoice, WallpaperLevel, resolve_wallpaper,
};
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

/// The levels a page's or folder's wallpaper comes from, as the window
/// edits them. The global wallpaper is only shown here; Preferences > Style
/// edits it.
pub struct Layers<'a> {
    pub global: Option<&'a Wallpaper>,
    pub profile: &'a mut Option<Wallpaper>,
    pub profile_disabled: &'a mut bool,
    pub page: &'a mut Page,
}

/// Where chosen pictures are kept.
enum Owner {
    /// In the profile's images folder.
    Profile(String),
    /// In Deckly's wallpapers folder, for the global wallpaper.
    Global,
}

pub struct WallpaperEditor {
    owner: Owner,
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
            owner: Owner::Profile(profile.to_owned()),
            profile: profile.to_owned(),
            page: page.to_owned(),
            editing_profile: false,
            job: None,
            error: None,
            preview: None,
            gesture: 0,
        }
    }

    /// An editor for the global wallpaper of Preferences > Style.
    pub fn global() -> Self {
        Self {
            owner: Owner::Global,
            ..Self::new("", "")
        }
    }

    /// Shows the window over `layers.page`. Returns `None` once it is
    /// closed, else the undo group of this frame's changes.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        layers: Layers<'_>,
        scope: &str,
        model: &DeviceModel,
        renderer: &mut Renderer,
    ) -> Option<u64> {
        let Layers {
            global,
            profile: profile_wallpaper,
            profile_disabled,
            page,
        } = layers;
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.gesture = self.gesture.wrapping_add(1);
        }
        if let Some(wallpaper) = self.receive() {
            if self.editing_profile {
                *profile_wallpaper = Some(wallpaper);
                *profile_disabled = false;
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
                let profile_choice =
                    WallpaperChoice::of(profile_wallpaper.as_ref(), *profile_disabled);
                let (shown, wallpaper) = if self.editing_profile {
                    let shown = resolve_wallpaper(global, profile_choice, WallpaperChoice::Inherit);
                    (shown.map(|(w, l)| (w.clone(), l)), &mut *profile_wallpaper)
                } else {
                    let shown = resolve_wallpaper(global, profile_choice, page.wallpaper_choice());
                    (shown.map(|(w, l)| (w.clone(), l)), &mut page.wallpaper)
                };
                let level = shown.as_ref().map(|(_, level)| *level);
                let shown = shown.map(|(w, _)| w);
                self.preview(ui, shown.as_ref(), wallpaper.as_mut(), model, renderer);
                ui.add_space(8.0);
                if let Some(wallpaper) = wallpaper {
                    zoom_row(ui, wallpaper);
                }
                let status = if self.editing_profile {
                    profile_status(*profile_disabled, level)
                } else {
                    local_status(page.wallpaper_disabled, level)
                };
                ui.label(RichText::new(status).color(TEXT_DIM));
                ui.horizontal(|ui| {
                    let (inherits, off, inherit_label) = if self.editing_profile {
                        (
                            !*profile_disabled && profile_wallpaper.is_none(),
                            *profile_disabled,
                            "Use Global",
                        )
                    } else {
                        (
                            !page.wallpaper_disabled && page.wallpaper.is_none(),
                            page.wallpaper_disabled,
                            "Use Profile",
                        )
                    };
                    let inherit = ui
                        .add_enabled(!busy && !inherits, egui::Button::new(inherit_label))
                        .clicked();
                    let disable = ui
                        .add_enabled(!busy && !off, egui::Button::new("No Wallpaper"))
                        .clicked();
                    if inherit || disable {
                        if self.editing_profile {
                            *profile_wallpaper = None;
                            *profile_disabled = disable;
                        } else {
                            page.wallpaper = None;
                            page.wallpaper_disabled = disable;
                        }
                        self.error = None;
                    }
                });
                self.error_row(ui);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    self.choose_button(ui, ctx, busy);
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

    /// Shows the global wallpaper's window, with the keys of `model`.
    /// Returns whether it is still open.
    pub fn show_global(
        &mut self,
        ctx: &egui::Context,
        wallpaper: &mut Option<Wallpaper>,
        model: &mut &'static DeviceModel,
        renderer: &mut Renderer,
    ) -> bool {
        if let Some(chosen) = self.receive() {
            *wallpaper = Some(chosen);
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new("Global Wallpaper")
            .id(egui::Id::new("global-wallpaper"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(PREVIEW_WIDTH);
                let busy = self.job.is_some();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preview on").color(TEXT_DIM));
                    egui::ComboBox::from_id_salt("global-wallpaper-model")
                        .selected_text(model.name)
                        .show_ui(ui, |ui| {
                            for m in DeviceModel::ALL {
                                if ui
                                    .selectable_label(std::ptr::eq(*model, m), m.name)
                                    .clicked()
                                {
                                    *model = m;
                                }
                            }
                        });
                });
                ui.add_space(4.0);
                let shown = wallpaper.clone();
                self.preview(ui, shown.as_ref(), wallpaper.as_mut(), model, renderer);
                ui.add_space(8.0);
                if let Some(wallpaper) = wallpaper.as_mut() {
                    zoom_row(ui, wallpaper);
                }
                let status = if wallpaper.is_some() {
                    "Shown behind the keys of every profile, page and folder that does not \
                     choose its own wallpaper or turn it off. Each model shows the picture \
                     across its own keys."
                } else {
                    "No global wallpaper. Profiles, pages and folders can still have their own."
                };
                ui.label(RichText::new(status).color(TEXT_DIM));
                self.error_row(ui);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    self.choose_button(ui, ctx, busy);
                    if ui
                        .add_enabled(
                            wallpaper.is_some() && !busy,
                            egui::Button::new(format!("{}  Remove", icon::TRASH)),
                        )
                        .clicked()
                    {
                        *wallpaper = None;
                        self.error = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = ui.button("Done").clicked();
                    });
                });
            });
        open && !close
    }

    fn error_row(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.label(RichText::new(format!("{}  {error}", icon::WARNING)).color(WARNING));
        }
    }

    fn choose_button(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, busy: bool) {
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
        let owner = match &self.owner {
            Owner::Profile(id) => Some(id.clone()),
            Owner::Global => None,
        };
        let repaint = ctx.clone();
        // Large pictures take a moment; the window stays responsive.
        let spawned = std::thread::Builder::new()
            .name("deckly-wallpaper".into())
            .spawn(move || {
                let saved = render::decode_wallpaper(&path).and_then(|img| match &owner {
                    Some(profile) => store::save_wallpaper(profile, &img),
                    None => store::save_global_wallpaper(&img),
                });
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

fn zoom_row(ui: &mut egui::Ui, wallpaper: &mut Wallpaper) {
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

/// What the Profile tab says about the profile's wallpaper.
fn profile_status(disabled: bool, shown: Option<WallpaperLevel>) -> &'static str {
    match (disabled, shown) {
        (true, _) => {
            "Wallpaper is off for this profile. Pages and folders can still have their own."
        }
        (false, Some(WallpaperLevel::Global)) => {
            "This profile uses the global wallpaper from Preferences > Style."
        }
        (false, Some(_)) => "Applies to pages and folders that use the profile wallpaper.",
        (false, None) => {
            "This profile follows the global wallpaper, which is not set. \
             Choose a picture here or in Preferences > Style."
        }
    }
}

/// What the page or folder tab says about the wallpaper on the canvas.
fn local_status(disabled: bool, shown: Option<WallpaperLevel>) -> &'static str {
    match (disabled, shown) {
        (true, _) => "Wallpaper is off here.",
        (false, Some(WallpaperLevel::Local)) => "This layout uses its own wallpaper.",
        (false, Some(WallpaperLevel::Profile)) => "This layout uses the profile wallpaper.",
        (false, Some(WallpaperLevel::Global)) => {
            "This layout uses the global wallpaper through its profile."
        }
        (false, None) => "This layout follows the profile, which shows no wallpaper.",
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

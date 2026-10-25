//! The editor window.
//!
//! Layout follows the official app: device and profile bar on top, the key
//! canvas in the middle, the action list on the right and the property
//! inspector for the selected key at the bottom. The editor works on a copy
//! of the current profile each frame and writes it back when it changed.

use crate::actions::{self, ACTIONS, Category};
use crate::os::{Hotkey, MediaKey};
use crate::render::{self, Renderer};
use crate::runtime::{self, Msg, NOTICE_TIME, Shared};
use crate::store;
use crate::theme::*;
use deckly_core::{DeviceKind, KeyConfig, Page, Profile, TitleAlign};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, FontId, Layout, Rect, RichText, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2,
};
use egui_phosphor::regular as icon;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const SAVE_DELAY: Duration = Duration::from_millis(400);
const TITLE_SIZES: [u8; 12] = [7, 8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28];

#[derive(Clone, Copy, Debug)]
enum DragPayload {
    Action(&'static str),
    Key(u8),
}

pub struct App {
    state: Shared,
    runtime: Sender<Msg>,
    renderer: Renderer,
    textures: HashMap<(u64, u32), TextureHandle>,
    selected_device: Option<String>,
    offline_profile: Option<String>,
    selected_key: Option<u8>,
    search: String,
    collapsed: HashSet<&'static str>,
    recording_hotkey: bool,
    clipboard: Option<KeyConfig>,
    dirty: HashSet<String>,
    last_edit: Instant,
    settings_open: bool,
    rename: Option<(String, String)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply(&cc.egui_ctx);
        let (profiles, errors) = store::load_profiles();
        let mut state = runtime::State::new(profiles, store::load_settings());
        if !errors.is_empty() {
            for e in &errors {
                eprintln!("deckly: {e}");
            }
            state.notify(format!(
                "{} profile file(s) could not be loaded",
                errors.len()
            ));
        }
        let state: Shared = Arc::new(std::sync::Mutex::new(state));
        let ctx = cc.egui_ctx.clone();
        let runtime = runtime::start(state.clone(), Arc::new(move || ctx.request_repaint()));
        Self {
            state,
            runtime,
            renderer: Renderer::new(),
            textures: HashMap::new(),
            selected_device: None,
            offline_profile: None,
            selected_key: None,
            search: String::new(),
            collapsed: HashSet::new(),
            recording_hotkey: false,
            clipboard: None,
            dirty: HashSet::new(),
            last_edit: Instant::now(),
            settings_open: false,
            rename: None,
        }
    }

    /// Picks the device and profile the editor shows this frame. Returns the
    /// selected device's handle (if connected) and the profile id.
    fn resolve(&mut self) -> (Option<deckly_device::ConnectedDevice>, String) {
        let mut s = runtime::lock(&self.state);
        let device = self
            .selected_device
            .as_ref()
            .and_then(|serial| s.devices.iter().find(|d| &d.serial == serial))
            .or_else(|| s.devices.first())
            .cloned();
        self.selected_device = device.as_ref().map(|d| d.serial.clone());
        let profile = match &device {
            Some(d) => s.assigned(&d.serial).map(|p| p.id.clone()),
            None => self
                .offline_profile
                .as_ref()
                .filter(|id| s.profile(id).is_some())
                .cloned()
                .or_else(|| s.profiles.first().map(|p| p.id.clone())),
        };
        let id = profile.unwrap_or_else(|| {
            s.ensure_profile(device.as_ref().map_or(DeviceKind::Mk2, |d| d.model.kind))
        });
        if device.is_none() {
            self.offline_profile = Some(id.clone());
        }
        (device, id)
    }

    fn mark_dirty(&mut self, profile_id: &str) {
        self.dirty.insert(profile_id.to_owned());
        self.last_edit = Instant::now();
        let _ = self.runtime.send(Msg::Refresh);
    }

    fn save_pending(&mut self, force: bool) {
        if self.dirty.is_empty() || (!force && self.last_edit.elapsed() < SAVE_DELAY) {
            return;
        }
        let mut s = runtime::lock(&self.state);
        for id in self.dirty.drain() {
            let result = s.profile(&id).map(store::save_profile);
            if let Some(Err(e)) = result {
                s.notify(format!("Cannot save profile: {e}"));
            }
        }
    }

    fn texture(
        &mut self,
        ctx: &egui::Context,
        key: Option<&KeyConfig>,
        px: u32,
    ) -> egui::TextureId {
        let px = px.clamp(32, 288);
        let hash = render::key_hash(key);
        if let Some(t) = self.textures.get(&(hash, px)) {
            return t.id();
        }
        if self.textures.len() > 512 {
            self.textures.clear();
        }
        let img = self.renderer.render(key, px);
        let color = ColorImage::from_rgba_unmultiplied([px as usize, px as usize], img.as_raw());
        let tex = ctx.load_texture(format!("key-{hash:x}-{px}"), color, TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert((hash, px), tex);
        id
    }

    // ----- top bar -------------------------------------------------------

    fn toolbar(
        &mut self,
        ui: &mut Ui,
        device: Option<&deckly_device::ConnectedDevice>,
        profile: &mut Profile,
    ) {
        ui.horizontal_centered(|ui| {
            ui.add_space(12.0);
            let devices = runtime::lock(&self.state).devices.clone();
            match device {
                Some(d) => {
                    let label = format!("{}  {}", icon::SQUARES_FOUR, d.model.name);
                    egui::ComboBox::from_id_salt("device")
                        .selected_text(label)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for other in &devices {
                                let text = format!("{} ({})", other.model.name, other.serial);
                                if ui
                                    .selectable_label(other.serial == d.serial, text)
                                    .clicked()
                                {
                                    self.selected_device = Some(other.serial.clone());
                                    self.selected_key = None;
                                }
                            }
                        })
                        .response
                        .on_hover_text(format!(
                            "Serial {}\nFirmware {}",
                            d.serial,
                            d.firmware.as_deref().unwrap_or("unknown")
                        ));
                }
                None => {
                    ui.label(
                        RichText::new(format!("{}  No device connected", icon::PLUGS))
                            .color(TEXT_DIM),
                    );
                }
            }
            ui.add_space(8.0);
            self.profile_menu(ui, device, profile);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(12.0);
                if ui
                    .add(egui::Button::new(RichText::new(icon::GEAR).size(18.0)).frame(false))
                    .on_hover_text("Preferences")
                    .clicked()
                {
                    self.settings_open = true;
                }
            });
        });
    }

    fn profile_menu(
        &mut self,
        ui: &mut Ui,
        device: Option<&deckly_device::ConnectedDevice>,
        profile: &Profile,
    ) {
        let choices: Vec<(String, String)> = runtime::lock(&self.state)
            .profiles
            .iter()
            .filter(|p| p.device == profile.device)
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let mut pick: Option<String> = None;
        egui::ComboBox::from_id_salt("profile")
            .selected_text(format!("{}  {}", icon::STACK, profile.name))
            .width(220.0)
            .show_ui(ui, |ui| {
                for (id, name) in &choices {
                    if ui.selectable_label(*id == profile.id, name).clicked() {
                        pick = Some(id.clone());
                    }
                }
            });
        ui.menu_button(RichText::new(icon::DOTS_THREE).size(18.0), |ui| {
            if ui.button(format!("{}  New Profile", icon::PLUS)).clicked() {
                let p = Profile::new("New Profile", profile.device);
                pick = Some(p.id.clone());
                self.rename = Some((p.id.clone(), p.name.clone()));
                let mut s = runtime::lock(&self.state);
                if let Err(e) = store::save_profile(&p) {
                    s.notify(format!("Cannot save profile: {e}"));
                }
                s.profiles.push(p);
                ui.close();
            }
            if ui
                .button(format!("{}  Rename", icon::PENCIL_SIMPLE))
                .clicked()
            {
                self.rename = Some((profile.id.clone(), profile.name.clone()));
                ui.close();
            }
            let can_delete = choices.len() > 1;
            if ui
                .add_enabled(
                    can_delete,
                    egui::Button::new(format!("{}  Delete", icon::TRASH)),
                )
                .clicked()
            {
                let mut s = runtime::lock(&self.state);
                s.profiles.retain(|p| p.id != profile.id);
                if let Err(e) = store::delete_profile(&profile.id) {
                    s.notify(format!("Cannot delete profile: {e}"));
                }
                pick = choices
                    .iter()
                    .find(|(id, _)| *id != profile.id)
                    .map(|(id, _)| id.clone());
                ui.close();
            }
        });
        if let Some(id) = pick {
            self.switch_profile(device, id);
        }
    }

    fn switch_profile(&mut self, device: Option<&deckly_device::ConnectedDevice>, id: String) {
        self.selected_key = None;
        match device {
            Some(d) => {
                let mut s = runtime::lock(&self.state);
                s.settings.assignments.insert(d.serial.clone(), id);
                if let Err(e) = store::save_settings(&s.settings) {
                    s.notify(format!("Cannot save settings: {e}"));
                }
                drop(s);
                let _ = self.runtime.send(Msg::Refresh);
            }
            None => self.offline_profile = Some(id),
        }
    }

    // ----- action list ---------------------------------------------------

    fn action_list(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(format!("{}  Search", icon::MAGNIFYING_GLASS))
                    .desired_width(ui.available_width() - 10.0),
            );
        });
        ui.add_space(6.0);
        let query = self.search.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for category in Category::ALL {
                    let items: Vec<&actions::ActionDef> = ACTIONS
                        .iter()
                        .filter(|a| a.category == category)
                        .filter(|a| query.is_empty() || a.name.to_lowercase().contains(&query))
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    let collapsed = query.is_empty() && self.collapsed.contains(category.label());
                    let caret = if collapsed {
                        icon::CARET_RIGHT
                    } else {
                        icon::CARET_DOWN
                    };
                    let header = ui.add(
                        egui::Button::new(
                            RichText::new(format!(
                                "{caret}  {}  {}",
                                category.icon(),
                                category.label()
                            ))
                            .color(TEXT_DIM)
                            .size(13.0),
                        )
                        .frame(false)
                        .min_size(vec2(ui.available_width(), 28.0)),
                    );
                    if header.clicked() && !self.collapsed.remove(category.label()) {
                        self.collapsed.insert(category.label());
                    }
                    if collapsed {
                        continue;
                    }
                    for action in items {
                        action_row(ui, action);
                    }
                    ui.add_space(4.0);
                }
            });
    }

    // ----- key canvas ----------------------------------------------------

    fn canvas(
        &mut self,
        ui: &mut Ui,
        device: Option<&deckly_device::ConnectedDevice>,
        profile: &mut Profile,
        page: &mut usize,
    ) {
        let model = profile.model();
        let (cols, rows) = (f32::from(model.columns), f32::from(model.rows));
        let avail = ui.available_rect_before_wrap();
        let pager_h = 44.0;
        let pad = 22.0;
        let gap_ratio = 0.22;
        let fit_w = (avail.width() - 80.0 - 2.0 * pad) / (cols + (cols - 1.0) * gap_ratio);
        let fit_h =
            (avail.height() - pager_h - 60.0 - 2.0 * pad) / (rows + (rows - 1.0) * gap_ratio);
        let key = fit_w.min(fit_h).clamp(36.0, 110.0);
        let gap = key * gap_ratio;
        let grid = vec2(
            cols * key + (cols - 1.0) * gap,
            rows * key + (rows - 1.0) * gap,
        );
        let frame_size = grid + Vec2::splat(2.0 * pad);
        let center = avail.center() - vec2(0.0, pager_h / 2.0);
        let frame = Rect::from_center_size(center, frame_size);
        let painter = ui.painter().clone();
        painter.rect_filled(frame, CornerRadius::same(18), DEVICE);
        painter.rect_stroke(
            frame,
            CornerRadius::same(18),
            Stroke::new(1.0, DEVICE_EDGE),
            StrokeKind::Inside,
        );

        let ppp = ui.ctx().pixels_per_point();
        let tex_px = (key * ppp).round() as u32;
        let pressed: HashSet<u8> = match device {
            Some(d) => runtime::lock(&self.state)
                .pressed
                .iter()
                .filter(|(h, _)| *h == d.handle)
                .map(|&(_, k)| k)
                .collect(),
            None => HashSet::new(),
        };
        let mut changed = false;
        let Some(keys) = profile.pages.get_mut(*page).map(|p| &mut p.keys) else {
            return;
        };
        for index in 0..model.key_count() {
            let (c, r) = (
                f32::from(index % model.columns),
                f32::from(index / model.columns),
            );
            let min = frame.min + Vec2::splat(pad) + vec2(c * (key + gap), r * (key + gap));
            let mut rect = Rect::from_min_size(min, Vec2::splat(key));
            let response = ui.interact(rect, ui.id().with(("key", index)), Sense::click_and_drag());
            let config = keys.get(&index).cloned();
            if config.is_some() {
                response.dnd_set_drag_payload(DragPayload::Key(index));
            }
            if pressed.contains(&index) {
                rect = rect.shrink(key * 0.04);
            }
            let tex = self.texture(ui.ctx(), config.as_ref(), tex_px);
            let radius = CornerRadius::same((key * 0.14) as u8);
            painter.add(
                egui::epaint::RectShape::filled(rect, radius, Color32::WHITE)
                    .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
            );
            let hovered_drop = response.dnd_hover_payload::<DragPayload>().is_some();
            let stroke = if self.selected_key == Some(index) {
                Stroke::new(2.0, ACCENT)
            } else if hovered_drop {
                Stroke::new(2.0, ACCENT.gamma_multiply(0.6))
            } else if response.hovered() {
                Stroke::new(1.0, TEXT_FAINT)
            } else {
                Stroke::new(1.0, KEY_EDGE)
            };
            painter.rect_stroke(rect, radius, stroke, StrokeKind::Outside);

            if let Some(payload) = response.dnd_release_payload::<DragPayload>() {
                match *payload {
                    DragPayload::Action(uuid) => {
                        if let Some(def) = actions::find(uuid) {
                            keys.insert(index, actions::new_key(def));
                        }
                    }
                    DragPayload::Key(from) if from != index => {
                        let a = keys.remove(&from);
                        let b = keys.remove(&index);
                        if let Some(a) = a {
                            keys.insert(index, a);
                        }
                        if let Some(b) = b {
                            keys.insert(from, b);
                        }
                    }
                    DragPayload::Key(_) => {}
                }
                self.selected_key = Some(index);
                self.recording_hotkey = false;
                changed = true;
            }
            if response.clicked() {
                self.selected_key = Some(index);
                self.recording_hotkey = false;
            }
            response.context_menu(|ui| {
                if ui
                    .add_enabled(
                        config.is_some(),
                        egui::Button::new(format!("{}  Copy", icon::COPY)),
                    )
                    .clicked()
                {
                    self.clipboard = config.clone();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.clipboard.is_some(),
                        egui::Button::new(format!("{}  Paste", icon::CLIPBOARD)),
                    )
                    .clicked()
                {
                    if let Some(c) = self.clipboard.clone() {
                        keys.insert(index, c);
                        changed = true;
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(
                        config.is_some(),
                        egui::Button::new(format!("{}  Clear", icon::TRASH)),
                    )
                    .clicked()
                {
                    keys.remove(&index);
                    changed = true;
                    ui.close();
                }
            });
        }

        // Delete clears the selected key while no text field has focus.
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if !typing
            && let Some(index) = self.selected_key
            && ui.input(|i| i.key_pressed(egui::Key::Delete))
            && keys.remove(&index).is_some()
        {
            changed = true;
        }
        if changed {
            self.mark_dirty(&profile.id);
        }

        // Pager under the device.
        let pager = Rect::from_center_size(
            pos2(frame.center().x, frame.bottom() + pager_h / 2.0 + 10.0),
            vec2(frame.width().max(260.0), pager_h),
        );
        let pages = profile.pages.len();
        let mut new_page = *page;
        ui.scope_builder(egui::UiBuilder::new().max_rect(pager), |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let inner = 9.0 * 22.0 + 90.0;
                ui.add_space(((ui.available_width() - inner) / 2.0).max(0.0));
                if ui
                    .add_enabled(pages > 1, egui::Button::new(icon::CARET_LEFT).frame(false))
                    .clicked()
                {
                    new_page = page.checked_sub(1).unwrap_or(pages - 1);
                }
                for i in 0..pages.min(9) {
                    let (dot, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                    let color = if i == *page { TEXT } else { TEXT_FAINT };
                    ui.painter().circle_filled(
                        dot.center(),
                        if i == *page { 4.5 } else { 3.5 },
                        color,
                    );
                    if resp.on_hover_text(format!("Page {}", i + 1)).clicked() {
                        new_page = i;
                    }
                }
                if pages > 9 {
                    ui.label(RichText::new(format!("{}/{}", *page + 1, pages)).color(TEXT_DIM));
                }
                if ui
                    .add_enabled(pages > 1, egui::Button::new(icon::CARET_RIGHT).frame(false))
                    .clicked()
                {
                    new_page = (*page + 1) % pages;
                }
                ui.add_space(10.0);
                if ui
                    .add_enabled(
                        pages < deckly_core::profile::MAX_PAGES,
                        egui::Button::new(icon::PLUS).frame(false),
                    )
                    .on_hover_text("Add page")
                    .clicked()
                {
                    profile.pages.insert(*page + 1, Page::new());
                    new_page = *page + 1;
                    self.mark_dirty(&profile.id);
                }
                if ui
                    .add_enabled(pages > 1, egui::Button::new(icon::TRASH).frame(false))
                    .on_hover_text("Delete this page")
                    .clicked()
                {
                    profile.pages.remove(*page);
                    new_page = page.saturating_sub(1);
                    self.mark_dirty(&profile.id);
                }
            });
        });
        if new_page != *page {
            *page = new_page;
            self.selected_key = None;
            self.recording_hotkey = false;
        }

        if device.is_none() {
            painter.text(
                pos2(frame.center().x, frame.top() - 22.0),
                Align2::CENTER_CENTER,
                format!(
                    "{}  Editing offline: connect a {} to use this profile",
                    icon::PLUGS,
                    model.name
                ),
                FontId::proportional(13.0),
                TEXT_DIM,
            );
        }

        // Preview of what is being dragged.
        if let Some(payload) = egui::DragAndDrop::payload::<DragPayload>(ui.ctx())
            && let Some(pos) = ui.ctx().pointer_interact_pos()
        {
            let glyph = match *payload {
                DragPayload::Action(uuid) => {
                    actions::find(uuid).map_or(icon::PUZZLE_PIECE, |a| a.icon)
                }
                DragPayload::Key(_) => icon::SQUARE,
            };
            let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drag-preview"));
            let p = ui.ctx().layer_painter(layer);
            let r = Rect::from_center_size(pos + vec2(18.0, 18.0), Vec2::splat(36.0));
            p.rect_filled(r, CornerRadius::same(7), ICON_TILE);
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                glyph,
                FontId::proportional(20.0),
                TEXT,
            );
        }
    }

    // ----- property inspector --------------------------------------------

    fn inspector(&mut self, ui: &mut Ui, profile: &mut Profile, page: usize) {
        let profiles: Vec<(String, String)> = runtime::lock(&self.state)
            .profiles
            .iter()
            .filter(|p| p.device == profile.device)
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let page_count = profile.pages.len();
        let Some(index) = self.selected_key else {
            hint(
                ui,
                "Select a key to edit it, or drag an action from the list onto a key.",
            );
            return;
        };
        let Some(key) = profile
            .pages
            .get_mut(page)
            .and_then(|p| p.keys.get_mut(&index))
        else {
            hint(
                ui,
                "This key is empty. Drag an action from the list onto it.",
            );
            return;
        };
        let before = key.clone();
        let def = actions::find(&key.action);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let glyph = def.map_or(icon::PUZZLE_PIECE, |d| d.icon);
            ui.label(RichText::new(glyph).size(18.0).color(ACCENT));
            ui.label(
                RichText::new(def.map_or(key.action.as_str(), |d| d.name))
                    .size(15.0)
                    .strong(),
            );
            if let Some(d) = def {
                ui.label(RichText::new(d.tooltip).color(TEXT_DIM));
            }
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        let ppp = ui.ctx().pixels_per_point();
        ui.horizontal_top(|ui| {
            ui.add_space(16.0);
            // Key preview and image controls.
            ui.vertical(|ui| {
                ui.set_width(110.0);
                let tex = self.texture(ui.ctx(), Some(key), (96.0 * ppp) as u32);
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
                ui.painter().add(
                    egui::epaint::RectShape::filled(rect, CornerRadius::same(12), Color32::WHITE)
                        .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
                );
                ui.add_space(6.0);
                if ui.button(format!("{}  Image…", icon::IMAGE)).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg"])
                        .pick_file()
                {
                    key.image = Some(path);
                }
                if key.image.is_some() && ui.button("Reset image").clicked() {
                    key.image = None;
                }
            });
            ui.add_space(16.0);
            ui.vertical(|ui| {
                egui::Grid::new("key-form")
                    .num_columns(2)
                    .spacing(vec2(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Title").color(TEXT_DIM));
                        ui.vertical(|ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut key.title)
                                    .desired_rows(2)
                                    .desired_width(260.0),
                            );
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut key.title_style.show, "Show");
                                ui.color_edit_button_srgb(&mut key.title_style.color);
                                egui::ComboBox::from_id_salt("title-size")
                                    .selected_text(format!("{} pt", key.title_style.size))
                                    .width(70.0)
                                    .show_ui(ui, |ui| {
                                        for s in TITLE_SIZES {
                                            ui.selectable_value(
                                                &mut key.title_style.size,
                                                s,
                                                format!("{s} pt"),
                                            );
                                        }
                                    });
                                for align in TitleAlign::ALL {
                                    ui.selectable_value(
                                        &mut key.title_style.align,
                                        align,
                                        align.label(),
                                    );
                                }
                            });
                        });
                        ui.end_row();
                        self.action_form(ui, key, &profiles, page_count);
                    });
            });
        });
        if *key != before {
            self.mark_dirty(&profile.id);
        }
    }

    fn action_form(
        &mut self,
        ui: &mut Ui,
        key: &mut KeyConfig,
        profiles: &[(String, String)],
        pages: usize,
    ) {
        let s = &mut key.settings;
        let label = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).color(TEXT_DIM));
        };
        match key.action.as_str() {
            actions::HOTKEY => {
                label(ui, "Hotkey");
                ui.horizontal(|ui| {
                    let current = actions::hotkey_setting(s);
                    let text = if self.recording_hotkey {
                        "Press a key combination… (Esc cancels)".to_owned()
                    } else if current.key.is_empty() {
                        "Not set".to_owned()
                    } else {
                        actions::hotkey_label(&current)
                    };
                    ui.add(egui::Label::new(RichText::new(text).monospace()));
                    let button = if self.recording_hotkey {
                        "Cancel"
                    } else {
                        "Record"
                    };
                    if ui.button(button).clicked() {
                        self.recording_hotkey = !self.recording_hotkey;
                    }
                    if self.recording_hotkey
                        && let Some(hk) = capture_hotkey(ui.ctx())
                    {
                        if let Some(hk) = hk {
                            actions::set_hotkey(s, &hk);
                        }
                        self.recording_hotkey = false;
                    }
                });
                ui.end_row();
            }
            actions::OPEN => {
                label(ui, "App / File");
                ui.horizontal(|ui| {
                    let mut path = actions::str_setting(s, "path").to_owned();
                    if ui
                        .add(egui::TextEdit::singleline(&mut path).desired_width(260.0))
                        .changed()
                    {
                        actions::set_setting(s, "path", json!(path));
                    }
                    if ui.button("Choose…").clicked()
                        && let Some(p) = rfd::FileDialog::new().pick_file()
                    {
                        actions::set_setting(s, "path", json!(p.to_string_lossy()));
                    }
                });
                ui.end_row();
            }
            actions::WEBSITE => {
                label(ui, "URL");
                let mut url = actions::str_setting(s, "url").to_owned();
                if ui
                    .add(egui::TextEdit::singleline(&mut url).desired_width(320.0))
                    .changed()
                {
                    actions::set_setting(s, "url", json!(url));
                }
                ui.end_row();
            }
            actions::TEXT => {
                label(ui, "Text");
                ui.vertical(|ui| {
                    let mut text = actions::str_setting(s, "text").to_owned();
                    if ui
                        .add(
                            egui::TextEdit::multiline(&mut text)
                                .desired_rows(3)
                                .desired_width(320.0),
                        )
                        .changed()
                    {
                        actions::set_setting(s, "text", json!(text));
                    }
                    let mut enter = actions::bool_setting(s, "enter");
                    if ui.checkbox(&mut enter, "Press Enter at the end").changed() {
                        actions::set_setting(s, "enter", json!(enter));
                    }
                });
                ui.end_row();
            }
            actions::MULTIMEDIA => {
                label(ui, "Action");
                let current = actions::media_setting(s).unwrap_or(MediaKey::PlayPause);
                egui::ComboBox::from_id_salt("media")
                    .selected_text(current.label())
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for m in MediaKey::ALL {
                            if ui.selectable_label(m == current, m.label()).clicked() {
                                actions::set_setting(s, "media", json!(m.id()));
                            }
                        }
                    });
                ui.end_row();
            }
            actions::GO_TO_PAGE => {
                label(ui, "Page");
                let mut n = actions::int_setting(s, "page", 1).clamp(1, pages.max(1) as i64);
                if ui
                    .add(egui::DragValue::new(&mut n).range(1..=pages.max(1) as i64))
                    .changed()
                {
                    actions::set_setting(s, "page", json!(n));
                }
                ui.end_row();
            }
            actions::SWITCH_PROFILE => {
                label(ui, "Profile");
                let current = actions::str_setting(s, "profile").to_owned();
                let name = profiles
                    .iter()
                    .find(|(id, _)| *id == current)
                    .map_or("Choose a profile", |(_, n)| n.as_str());
                egui::ComboBox::from_id_salt("switch-profile")
                    .selected_text(name)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (id, n) in profiles {
                            if ui.selectable_label(*id == current, n).clicked() {
                                actions::set_setting(s, "profile", json!(id));
                            }
                        }
                    });
                ui.end_row();
            }
            actions::BRIGHTNESS => {
                label(ui, "Brightness");
                ui.horizontal(|ui| {
                    let mode = actions::str_setting(s, "mode").to_owned();
                    for (id, text) in [("up", "Increase"), ("down", "Decrease"), ("set", "Set to")]
                    {
                        if ui
                            .selectable_label(mode == id || (mode.is_empty() && id == "up"), text)
                            .clicked()
                        {
                            actions::set_setting(s, "mode", json!(id));
                        }
                    }
                    let mut v = actions::int_setting(s, "value", 10).clamp(0, 100);
                    if ui
                        .add(egui::Slider::new(&mut v, 0..=100).suffix("%"))
                        .changed()
                    {
                        actions::set_setting(s, "value", json!(v));
                    }
                });
                ui.end_row();
            }
            _ => {}
        }
    }

    // ----- windows -------------------------------------------------------

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Preferences")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let mut s = runtime::lock(&self.state);
                ui.label(RichText::new("Devices").strong());
                let mut b = s.settings.brightness;
                let r = ui.add(
                    egui::Slider::new(&mut b, 0..=100)
                        .text("Brightness")
                        .suffix("%"),
                );
                if r.changed() {
                    s.settings.brightness = b;
                    let _ = self.runtime.send(Msg::Brightness);
                }
                if (r.drag_stopped() || (r.changed() && !r.dragged()))
                    && let Err(e) = store::save_settings(&s.settings)
                {
                    s.notify(format!("Cannot save settings: {e}"));
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Storage").strong());
                if let Some(dir) = store::config_dir() {
                    ui.label(
                        RichText::new(dir.display().to_string())
                            .color(TEXT_DIM)
                            .small(),
                    );
                    if ui
                        .button(format!("{}  Open folder", icon::FOLDER))
                        .clicked()
                    {
                        let _ = std::fs::create_dir_all(&dir);
                        if let Err(e) = crate::os::open(&dir.to_string_lossy()) {
                            s.notify(e);
                        }
                    }
                }
                ui.add_space(10.0);
                ui.label(RichText::new("About").strong());
                ui.label(format!("Deckly {}", env!("CARGO_PKG_VERSION")));
                ui.label(
                    RichText::new(
                        "Unofficial. Not made, approved or supported by Elgato or Corsair.",
                    )
                    .color(TEXT_DIM)
                    .small(),
                );
            });
        self.settings_open = open;
    }

    fn rename_window(&mut self, ctx: &egui::Context) {
        let Some((id, mut name)) = self.rename.take() else {
            return;
        };
        let mut keep = true;
        let mut commit = false;
        egui::Window::new("Rename Profile")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(260.0));
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        keep = false;
                    }
                });
            });
        let trimmed = name.trim().chars().take(64).collect::<String>();
        if commit && !trimmed.is_empty() {
            if let Some(p) = runtime::lock(&self.state).profile_mut(&id) {
                p.name = trimmed;
            }
            self.mark_dirty(&id);
            keep = false;
        }
        if keep {
            self.rename = Some((id, name));
        }
    }

    fn notice(&mut self, ctx: &egui::Context) {
        let mut s = runtime::lock(&self.state);
        let Some((text, at)) = s.notice.clone() else {
            return;
        };
        let left = NOTICE_TIME.saturating_sub(at.elapsed());
        if left.is_zero() {
            s.notice = None;
            return;
        }
        drop(s);
        egui::Area::new(egui::Id::new("notice"))
            .anchor(Align2::CENTER_TOP, vec2(0.0, 64.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(TOOLBAR)
                    .stroke(Stroke::new(1.0, WARNING.gamma_multiply(0.6)))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{}  {text}", icon::WARNING)).color(WARNING),
                        );
                    });
            });
        ctx.request_repaint_after(left);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (device, profile_id) = self.resolve();
        let (mut profile, mut page) = {
            let s = runtime::lock(&self.state);
            match s.profile(&profile_id) {
                Some(p) => (p.clone(), s.page(p)),
                None => return,
            }
        };
        let original = profile.clone();
        let original_page = page;

        egui::Panel::top("toolbar")
            .exact_size(52.0)
            .frame(egui::Frame::new().fill(TOOLBAR))
            .show(ui, |ui| self.toolbar(ui, device.as_ref(), &mut profile));
        egui::Panel::right("actions")
            .exact_size(290.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL))
            .show(ui, |ui| self.action_list(ui));
        egui::Panel::bottom("inspector")
            .resizable(true)
            .default_size(250.0)
            .min_size(170.0)
            .frame(egui::Frame::new().fill(INSPECTOR))
            .show(ui, |ui| self.inspector(ui, &mut profile, page));
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), CornerRadius::ZERO, CANVAS);
            self.canvas(ui, device.as_ref(), &mut profile, &mut page);
        });

        // Write the edited copy back. Profile switches made this frame
        // already changed which profile is current; only write if it still
        // exists.
        if profile != original || page != original_page {
            let mut s = runtime::lock(&self.state);
            if let Some(p) = s.profile_mut(&profile.id) {
                *p = profile.clone();
            }
            s.set_page(&profile.id, page);
            drop(s);
            let _ = self.runtime.send(Msg::Refresh);
        }

        self.settings_window(&ctx);
        self.rename_window(&ctx);
        self.notice(&ctx);
        self.save_pending(false);
        if !self.dirty.is_empty() {
            ctx.request_repaint_after(SAVE_DELAY);
        }
    }

    fn on_exit(&mut self) {
        self.save_pending(true);
        let _ = self.runtime.send(Msg::Shutdown);
        // Give the writer threads a moment to send the reset.
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn action_row(ui: &mut Ui, action: &'static actions::ActionDef) {
    let id = egui::Id::new(("action", action.uuid));
    ui.dnd_drag_source(id, DragPayload::Action(action.uuid), |ui| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
        if resp.hovered() {
            ui.painter().rect_filled(
                rect.shrink2(vec2(6.0, 1.0)),
                CornerRadius::same(5),
                ROW_HOVER,
            );
        }
        let tile = Rect::from_min_size(
            pos2(rect.left() + 18.0, rect.center().y - 13.0),
            Vec2::splat(26.0),
        );
        ui.painter()
            .rect_filled(tile, CornerRadius::same(5), ICON_TILE);
        ui.painter().text(
            tile.center(),
            Align2::CENTER_CENTER,
            action.icon,
            FontId::proportional(16.0),
            TEXT,
        );
        ui.painter().text(
            pos2(tile.right() + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            action.name,
            FontId::proportional(14.0),
            TEXT,
        );
    })
    .response
    .on_hover_text(action.tooltip);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(RichText::new(text).color(TEXT_FAINT));
    });
}

/// Returns `Some(Some(hotkey))` when a combination was pressed,
/// `Some(None)` when recording was cancelled with Esc, `None` otherwise.
fn capture_hotkey(ctx: &egui::Context) -> Option<Option<Hotkey>> {
    ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if *key == egui::Key::Escape && modifiers.is_none() {
                    return Some(None);
                }
                Some(Some(Hotkey {
                    ctrl: modifiers.ctrl,
                    shift: modifiers.shift,
                    alt: modifiers.alt,
                    // `command` is Ctrl on Windows/Linux; only `mac_cmd` is ⌘.
                    meta: modifiers.mac_cmd,
                    key: key.name().to_owned(),
                }))
            }
            _ => None,
        })
    })
}

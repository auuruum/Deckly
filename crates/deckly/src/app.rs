//! The editor window.
//!
//! Layout follows the official app (docs/clean-room/2026-10-25-main-window.md):
//! the canvas with the device header, key grid and page strip on the left,
//! the property inspector under it, and the action list on the right. The editor works on a copy
//! of the current profile each frame and writes it back when it changed.

use crate::actions::{self, ACTIONS, Category};
use crate::os::{Hotkey, MediaKey};
use crate::prefs::{self, Preferences};
use crate::render::Renderer;
use crate::runtime::{self, Msg, NOTICE_TIME, Shared};
use crate::store;
use crate::theme::*;
use deckly_core::{DeviceKind, KeyConfig, Page, Profile, TitleAlign, TitleStyle};
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2, pos2, vec2,
};
use egui_phosphor::regular as icon;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const SAVE_DELAY: Duration = Duration::from_millis(400);
/// The official window gives the canvas about 58% of the left column.
const DEFAULT_SPLIT: f32 = 0.58;
const MIN_CANVAS: f32 = 260.0;
const MIN_INSPECTOR: f32 = 150.0;
const MAX_LIST_WIDTH: f32 = 560.0;
/// The canvas keeps at least this width when the action list is widened.
const MIN_CANVAS_WIDTH: f32 = 554.0;
const TITLE_SIZES: [u8; 12] = [7, 8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListTab {
    Keys,
    Plugins,
}

/// Where the header and key grid sit in the canvas, following the official
/// window's resize behavior (docs/clean-room/2026-10-25-main-window.md):
/// the header stays at the top and widens with the canvas up to a limit, the
/// grid and page strip are centred below it, and keys grow from 70 to 76 px
/// as the canvas gets taller.
struct CanvasGeometry {
    top: f32,
    /// Key size relative to the 70 px base size.
    scale: f32,
    key: f32,
    pitch: f32,
    grid: Rect,
    header_left: f32,
    header_right: f32,
}

impl CanvasGeometry {
    const HEADER_BOTTOM: f32 = 84.0;
    const PAGER_GAP: f32 = 44.0;
    const PAGER_HEIGHT: f32 = 20.0;

    fn new(canvas: Rect, columns: u8, rows: u8) -> Self {
        let t = ((canvas.height() - 428.0) / 102.0).clamp(0.0, 1.0);
        let (mut key, mut pitch) = (KEY_SIZE + 6.0 * t, KEY_PITCH + 10.0 * t);
        let span = |n: u8, key: f32, pitch: f32| f32::from(n.saturating_sub(1)) * pitch + key;
        // Shrink only when the grid does not fit (a dragged splitter).
        let block = |key: f32, pitch: f32| {
            span(rows, key, pitch) + Self::PAGER_GAP + Self::PAGER_HEIGHT / 2.0
        };
        let room_h = canvas.height() - Self::HEADER_BOTTOM - 8.0;
        let room_w = canvas.width() - 48.0;
        let fit = (room_h / block(key, pitch))
            .min(room_w / span(columns, key, pitch))
            .clamp(0.3, 1.0);
        key *= fit;
        pitch *= fit;
        let size = vec2(span(columns, key, pitch), span(rows, key, pitch));
        let region_top = canvas.top() + Self::HEADER_BOTTOM;
        let free = (canvas.bottom() - region_top - block(key, pitch)).max(0.0);
        let top = region_top + (free / 2.0 - 7.0).max(4.0);
        let grid = Rect::from_min_size(pos2(canvas.center().x - size.x / 2.0, top), size);
        let margin = (canvas.width() * 0.055).max(47.0);
        let half = ((canvas.width() - 2.0 * margin).min(1192.0) / 2.0).max(size.x / 2.0);
        Self {
            top: canvas.top(),
            scale: key / KEY_SIZE,
            key,
            pitch,
            grid,
            header_left: canvas.center().x - half,
            header_right: canvas.center().x + half,
        }
    }
}

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
    tab: ListTab,
    expanded: HashSet<&'static str>,
    recording_hotkey: bool,
    clipboard: Option<KeyConfig>,
    dirty: HashSet<String>,
    last_edit: Instant,
    prefs: Preferences,
    rename: Option<(String, String)>,
    /// Share of the left column given to the canvas.
    split: f32,
    /// Width of the action list; dragging its left edge changes it.
    list_width: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply(&cc.egui_ctx);
        let (profiles, errors) = store::load_profiles();
        let (settings, settings_error) = store::load_settings();
        let mut state = runtime::State::new(profiles, settings);
        if let Some(e) = settings_error {
            eprintln!("deckly: {e}");
            state.notify("Settings could not be loaded; using defaults");
        }
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
            tab: ListTab::Keys,
            expanded: HashSet::from([Category::System.label()]),
            recording_hotkey: false,
            clipboard: None,
            dirty: HashSet::new(),
            last_edit: Instant::now(),
            prefs: Preferences::new(),
            rename: None,
            split: DEFAULT_SPLIT,
            list_width: LIST_WIDTH,
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
        let hash = self.renderer.key_hash(key);
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

    // ----- device header -------------------------------------------------

    /// Device name and profile name above the key grid, each opening a menu,
    /// and the preferences button at the right end.
    fn device_header(
        &mut self,
        ui: &mut Ui,
        geo: &CanvasGeometry,
        device: Option<&deckly_device::ConnectedDevice>,
        profile: &Profile,
    ) {
        let painter = ui.painter().clone();
        let left = geo.header_left;
        let title_y = geo.top + 47.0;
        let profile_y = geo.top + 72.0;

        // Device name.
        let name = match device {
            Some(d) => runtime::lock(&self.state).device_name(d),
            None => profile.model().name.to_owned(),
        };
        let title = format!("{name} {}", icon::CARET_DOWN);
        let galley = painter.layout_no_wrap(title, bold(17.0), TEXT);
        let rect = Rect::from_min_size(pos2(left, title_y - galley.size().y / 2.0), galley.size());
        painter.galley(rect.min, galley, TEXT);
        let resp = ui.interact(rect, ui.id().with("device-menu"), Sense::click());
        if device.is_none() {
            painter.text(
                pos2(rect.right() + 10.0, title_y),
                Align2::LEFT_CENTER,
                format!("{}  not connected", icon::PLUGS),
                font(12.0),
                TEXT_FAINT,
            );
        }
        let devices: Vec<(deckly_device::ConnectedDevice, String)> = {
            let s = runtime::lock(&self.state);
            s.devices
                .iter()
                .map(|d| (d.clone(), s.device_name(d)))
                .collect()
        };
        egui::Popup::menu(&resp).show(|ui| {
            if devices.is_empty() {
                ui.label(RichText::new("No device connected").color(TEXT_DIM));
            }
            for (other, other_name) in &devices {
                let current = device.is_some_and(|d| d.serial == other.serial);
                let text = format!("{other_name}  ({})", other.serial);
                if ui.selectable_label(current, text).clicked() {
                    self.selected_device = Some(other.serial.clone());
                    self.selected_key = None;
                }
            }
            if let Some(d) = device {
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "Firmware {}",
                        d.firmware.as_deref().unwrap_or("unknown")
                    ))
                    .color(TEXT_DIM),
                );
            }
        });

        // Profile name.
        let text = format!("{} {}", profile.name, icon::CARET_DOWN);
        let galley = painter.layout_no_wrap(text, font(13.0), TEXT);
        let rect =
            Rect::from_min_size(pos2(left, profile_y - galley.size().y / 2.0), galley.size());
        painter.galley(rect.min, galley, TEXT);
        let resp = ui.interact(rect, ui.id().with("profile-menu"), Sense::click());
        self.profile_menu(&resp, device, profile);

        // Preferences.
        let gear =
            Rect::from_center_size(pos2(geo.header_right - 13.0, title_y), Vec2::splat(26.0));
        let resp = ui
            .interact(gear, ui.id().with("preferences"), Sense::click())
            .on_hover_text("Preferences");
        if resp.hovered() {
            painter.circle_filled(gear.center(), 13.0, PAGER);
        }
        painter.text(
            gear.center(),
            Align2::CENTER_CENTER,
            icon::GEAR,
            font(19.0),
            TEXT,
        );
        if resp.clicked() {
            self.prefs.open(ui.ctx(), device.map(|d| d.serial.as_str()));
        }
    }

    fn profile_menu(
        &mut self,
        button: &egui::Response,
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
        egui::Popup::menu(button).width(220.0).show(|ui| {
            for (id, name) in &choices {
                if ui.selectable_label(*id == profile.id, name).clicked() {
                    pick = Some(id.clone());
                }
            }
            ui.separator();
            if ui.button(format!("{}  New Profile", icon::PLUS)).clicked() {
                let id = runtime::lock(&self.state).add_profile("New Profile", profile.device);
                pick = Some(id.clone());
                self.rename = Some((id, "New Profile".to_owned()));
            }
            if ui
                .button(format!("{}  Rename", icon::PENCIL_SIMPLE))
                .clicked()
            {
                self.rename = Some((profile.id.clone(), profile.name.clone()));
            }
            if ui
                .add_enabled(
                    choices.len() > 1,
                    egui::Button::new(format!("{}  Delete", icon::TRASH)),
                )
                .clicked()
            {
                runtime::lock(&self.state).delete_profile(&profile.id);
                pick = choices
                    .iter()
                    .find(|(id, _)| *id != profile.id)
                    .map(|(id, _)| id.clone());
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
                s.save_settings();
                drop(s);
                let _ = self.runtime.send(Msg::Refresh);
            }
            None => self.offline_profile = Some(id),
        }
    }

    // ----- action list ---------------------------------------------------

    fn action_list(&mut self, ui: &mut Ui) {
        let full = ui.max_rect();
        ui.painter().rect_filled(full, CornerRadius::ZERO, LIST);
        let header = Rect::from_min_size(full.min, vec2(full.width(), 80.0));
        ui.painter()
            .rect_filled(header, CornerRadius::ZERO, LIST_HEADER);

        // Search field.
        let search = Rect::from_min_size(
            pos2(header.left() + 17.0, header.top() + 12.0),
            vec2(header.width() - 34.0, 25.0),
        );
        ui.painter()
            .rect_filled(search, CornerRadius::same(4), SEARCH);
        ui.painter().text(
            pos2(search.left() + 14.0, search.center().y),
            Align2::CENTER_CENTER,
            icon::MAGNIFYING_GLASS,
            font(13.0),
            TEXT_FAINT,
        );
        let edit = Rect::from_min_max(
            pos2(search.left() + 26.0, search.top() + 4.0),
            pos2(search.right() - 6.0, search.bottom() - 2.0),
        );
        ui.put(
            edit,
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(RichText::new("Search").color(TEXT_FAINT))
                .frame(egui::Frame::NONE)
                .font(font(13.0)),
        );

        // Keys / Plugins tabs.
        let tabs = Rect::from_min_size(
            pos2(header.left() + 17.0, header.top() + 49.0),
            vec2(header.width() - 34.0, 24.0),
        );
        let half = tabs.width() / 2.0;
        for (i, (tab, label)) in [
            (ListTab::Keys, format!("{}  Keys", icon::SQUARE)),
            (ListTab::Plugins, "Plugins".to_owned()),
        ]
        .into_iter()
        .enumerate()
        {
            let r = Rect::from_min_size(
                pos2(tabs.left() + half * i as f32, tabs.top()),
                vec2(half, tabs.height()),
            );
            let resp = ui.interact(r, ui.id().with(("tab", i)), Sense::click());
            let selected = self.tab == tab;
            if selected {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(12), TAB_SELECTED);
            }
            let color = if selected || resp.hovered() {
                TEXT
            } else {
                TEXT_DIM
            };
            ui.painter()
                .text(r.center(), Align2::CENTER_CENTER, label, font(13.0), color);
            if resp.clicked() {
                self.tab = tab;
            }
        }

        let body = Rect::from_min_max(pos2(full.left(), header.bottom() + 2.0), full.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| match self.tab {
            ListTab::Keys => self.key_actions(ui),
            ListTab::Plugins => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(icon::PUZZLE_PIECE)
                            .size(28.0)
                            .color(TEXT_FAINT),
                    );
                    ui.label(RichText::new("Plugins are not supported yet.").color(TEXT_DIM));
                });
            }
        });
    }

    fn key_actions(&mut self, ui: &mut Ui) {
        let query = self.search.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for category in Category::ALL {
                    let items: Vec<&'static actions::ActionDef> = ACTIONS
                        .iter()
                        .filter(|a| a.category == category)
                        .filter(|a| query.is_empty() || a.name.to_lowercase().contains(&query))
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    // Collapsed categories open while searching.
                    let open = !query.is_empty() || self.expanded.contains(category.label());
                    let (rect, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), CATEGORY_ROW),
                        Sense::click(),
                    );
                    let fill = if resp.hovered() {
                        LIST_HOVER
                    } else {
                        LIST_HEADER
                    };
                    let p = ui.painter();
                    p.rect_filled(rect, CornerRadius::ZERO, fill);
                    let y = rect.center().y;
                    let chevron = if open {
                        icon::CARET_DOWN
                    } else {
                        icon::CARET_RIGHT
                    };
                    p.text(
                        pos2(rect.left() + 23.0, y),
                        Align2::CENTER_CENTER,
                        chevron,
                        font(13.0),
                        CHEVRON,
                    );
                    p.text(
                        pos2(rect.left() + 55.0, y),
                        Align2::CENTER_CENTER,
                        category.icon(),
                        font(22.0),
                        TEXT,
                    );
                    p.text(
                        pos2(rect.left() + 76.0, y),
                        Align2::LEFT_CENTER,
                        category.label(),
                        bold(13.0),
                        TEXT,
                    );
                    if resp.clicked() && !self.expanded.remove(category.label()) {
                        self.expanded.insert(category.label());
                    }
                    if open {
                        for action in items {
                            action_row(ui, action);
                        }
                    }
                    ui.add_space(2.0);
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
        let geo = CanvasGeometry::new(ui.max_rect(), model.columns, model.rows);
        self.device_header(ui, &geo, device, profile);
        let painter = ui.painter().clone();
        let s = geo.scale;
        let key_size = geo.key;
        let border = (KEY_BORDER_WIDTH * s).max(2.0);
        let outer_radius = CornerRadius::same((11.0 * s) as u8);
        let inner_radius = CornerRadius::same((8.0 * s) as u8);
        let ppp = ui.ctx().pixels_per_point();
        let tex_px = ((key_size - 2.0 * border) * ppp).round() as u32;
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
            let min = geo.grid.min + vec2(c, r) * geo.pitch;
            let mut rect = Rect::from_min_size(min, Vec2::splat(key_size));
            let response = ui.interact(rect, ui.id().with(("key", index)), Sense::click_and_drag());
            let config = keys.get(&index).cloned();
            if config.is_some() {
                response.dnd_set_drag_payload(DragPayload::Key(index));
            }
            if pressed.contains(&index) {
                rect = rect.shrink(key_size * 0.04);
            }
            let hovered_drop = response.dnd_hover_payload::<DragPayload>().is_some();
            let border_color = if hovered_drop {
                KEY_DROP
            } else if self.selected_key == Some(index) {
                KEY_SELECTED
            } else if response.hovered() {
                TEXT_DIM
            } else {
                KEY_BORDER
            };
            painter.rect_filled(rect, outer_radius, border_color);
            let inner = rect.shrink(border);
            match &config {
                Some(key) => {
                    let tex = self.texture(ui.ctx(), Some(key), tex_px);
                    painter.add(
                        egui::epaint::RectShape::filled(inner, inner_radius, Color32::WHITE)
                            .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
                    );
                }
                None => {
                    painter.rect_filled(inner, inner_radius, KEY_EMPTY);
                }
            }

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

        self.pager(ui, &geo, profile, page);

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
            let r = Rect::from_center_size(pos + vec2(20.0, 20.0), Vec2::splat(40.0));
            p.rect_filled(r, CornerRadius::same(8), KEY_BORDER);
            p.rect_filled(r.shrink(2.0), CornerRadius::same(6), Color32::BLACK);
            p.text(r.center(), Align2::CENTER_CENTER, glyph, font(20.0), TEXT);
        }
    }

    /// Page chips under the grid: one rounded strip with the page numbers
    /// and a "+" button. Right-click a page to delete it.
    fn pager(
        &mut self,
        ui: &mut Ui,
        geo: &CanvasGeometry,
        profile: &mut Profile,
        page: &mut usize,
    ) {
        const CHIP: f32 = 30.0;
        const HEIGHT: f32 = 20.0;
        const WINDOW: usize = 10;
        let pages = profile.pages.len();
        // Show at most WINDOW numbers, keeping the current page visible.
        let first = page
            .saturating_sub(WINDOW / 2)
            .min(pages.saturating_sub(WINDOW));
        let shown = first..pages.min(first + WINDOW);
        let can_add = pages < deckly_core::profile::MAX_PAGES;
        let width = shown.len() as f32 * CHIP + if can_add { 26.0 } else { 0.0 } + 4.0;
        let strip = Rect::from_center_size(
            pos2(
                geo.grid.center().x,
                geo.grid.bottom() + CanvasGeometry::PAGER_GAP * geo.scale.min(1.0),
            ),
            vec2(width, HEIGHT),
        );
        let painter = ui.painter().clone();
        painter.rect_filled(strip, CornerRadius::same(10), PAGER);
        let mut new_page = *page;
        let mut delete = None;
        for (slot, i) in shown.enumerate() {
            let r = Rect::from_min_size(
                pos2(strip.left() + 2.0 + slot as f32 * CHIP, strip.top()),
                vec2(CHIP, HEIGHT),
            );
            let resp = ui.interact(r, ui.id().with(("page", i)), Sense::click());
            let current = i == *page;
            if current {
                painter.rect_filled(r.shrink2(vec2(1.0, 1.0)), CornerRadius::same(9), PAGER_CHIP);
            }
            let color = if current {
                PAGER_CHIP_TEXT
            } else if resp.hovered() {
                TEXT
            } else {
                TEXT_DIM
            };
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                (i + 1).to_string(),
                font(12.0),
                color,
            );
            if resp.clicked() {
                new_page = i;
            }
            resp.context_menu(|ui| {
                if ui
                    .add_enabled(
                        pages > 1,
                        egui::Button::new(format!("{}  Delete Page", icon::TRASH)),
                    )
                    .clicked()
                {
                    delete = Some(i);
                    ui.close();
                }
            });
        }
        if can_add {
            let r = Rect::from_min_max(
                pos2(strip.right() - 28.0, strip.top()),
                strip.right_bottom(),
            );
            let resp = ui
                .interact(r, ui.id().with("add-page"), Sense::click())
                .on_hover_text("Add page");
            let color = if resp.hovered() { TEXT } else { TEXT_DIM };
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                icon::PLUS,
                font(13.0),
                color,
            );
            if resp.clicked() {
                profile.pages.insert(*page + 1, Page::new());
                new_page = *page + 1;
                self.mark_dirty(&profile.id);
            }
        }
        if let Some(i) = delete
            && profile.pages.len() > 1
        {
            profile.pages.remove(i);
            new_page = if new_page > i || new_page >= profile.pages.len() {
                new_page.saturating_sub(1)
            } else {
                new_page
            };
            self.mark_dirty(&profile.id);
        }
        if new_page != *page || delete.is_some() {
            *page = new_page.min(profile.pages.len().saturating_sub(1));
            self.selected_key = None;
            self.recording_hotkey = false;
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
            hint(ui, "Select a key to edit its action.");
            return;
        };
        let Some(key) = profile
            .pages
            .get_mut(page)
            .and_then(|p| p.keys.get_mut(&index))
        else {
            hint(ui, "Drag an action from the list onto this key.");
            return;
        };
        let before = key.clone();
        let defaults = self.renderer.title_defaults;
        let def = actions::find(&key.action);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let glyph = def.map_or(icon::PUZZLE_PIECE, |d| d.icon);
            ui.label(RichText::new(glyph).size(18.0).color(ITEM_ICON));
            ui.label(RichText::new(def.map_or(key.action.as_str(), |d| d.name)).font(bold(14.0)));
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
                            title_style_row(ui, &mut key.title_style, defaults);
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
            actions::SLEEP => {
                ui.label("");
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(
                            "Open Preferences to customize Sleep settings and select a \
                             screensaver image.",
                        )
                        .color(TEXT_DIM),
                    );
                    ui.add_space(4.0);
                    if ui.button("Open Preferences…").clicked() {
                        self.prefs
                            .open_devices(ui.ctx(), self.selected_device.as_deref());
                    }
                });
                ui.end_row();
            }
            _ => {}
        }
    }

    // ----- windows -------------------------------------------------------

    fn preferences(&mut self, ctx: &egui::Context, device: Option<&str>, kind: DeviceKind) {
        let mut env = prefs::Env {
            state: &self.state,
            runtime: &self.runtime,
            renderer: &mut self.renderer,
            device,
            kind,
        };
        self.prefs.show(ctx, &mut env);
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
                    .fill(POPUP)
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
        self.renderer.title_defaults = runtime::lock(&self.state).settings.title_style;
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

        let window = ui.available_rect_before_wrap();
        let max_list = (window.width() - MIN_CANVAS_WIDTH).clamp(LIST_WIDTH, MAX_LIST_WIDTH);
        let list_width = self.list_width.clamp(LIST_WIDTH, max_list);
        egui::Panel::right("actions")
            .exact_size(list_width)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(LIST))
            .show(ui, |ui| self.action_list(ui));
        // Canvas and inspector share the left column like a splitter: the
        // inspector keeps its share of the height when the window resizes,
        // and dragging the divider changes that share.
        let column = ui.available_rect_before_wrap();
        let max_inspector = (column.height() - MIN_CANVAS).max(MIN_INSPECTOR);
        let inspector_h =
            (column.height() * (1.0 - self.split)).clamp(MIN_INSPECTOR, max_inspector);
        egui::Panel::bottom("inspector")
            .resizable(false)
            .exact_size(inspector_h)
            .frame(egui::Frame::new().fill(CANVAS))
            .show(ui, |ui| self.inspector(ui, &mut profile, page));
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), CornerRadius::ZERO, CANVAS);
            self.canvas(ui, device.as_ref(), &mut profile, &mut page);
        });
        let divider_y = column.bottom() - inspector_h;
        let handle = Rect::from_x_y_ranges(column.x_range(), divider_y - 4.0..=divider_y + 4.0);
        let resp = ui.interact(handle, ui.id().with("splitter"), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
            && column.height() > 0.0
        {
            let h = (column.bottom() - pos.y).clamp(MIN_INSPECTOR, max_inspector);
            self.split = 1.0 - h / column.height();
        }
        // The action list's left edge works the same way.
        let edge_x = window.right() - list_width;
        let handle = Rect::from_x_y_ranges(edge_x - 4.0..=edge_x + 4.0, window.y_range());
        let resp = ui.interact(handle, ui.id().with("list-edge"), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            self.list_width = (window.right() - pos.x).clamp(LIST_WIDTH, max_list);
        }

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

        self.preferences(
            &ctx,
            device.as_ref().map(|d| d.serial.as_str()),
            profile.device,
        );
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
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), ACTION_ROW), Sense::hover());
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, CornerRadius::ZERO, LIST_HOVER);
        }
        p.text(
            pos2(rect.left() + 55.0, rect.center().y),
            Align2::CENTER_CENTER,
            action.icon,
            font(19.0),
            ITEM_ICON,
        );
        let color = if resp.hovered() { TEXT } else { TEXT_DIM };
        p.text(
            pos2(rect.left() + 76.0, rect.center().y),
            Align2::LEFT_CENTER,
            action.name,
            font(13.0),
            color,
        );
    })
    .response
    .on_hover_text(action.tooltip);
}

/// Title show, colour, size and position. Editing makes the key keep its
/// own style; the reset button makes it follow the global style again.
fn title_style_row(ui: &mut Ui, style: &mut Option<TitleStyle>, defaults: TitleStyle) {
    let mut s = style.unwrap_or(defaults);
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.show, "Show");
        ui.color_edit_button_srgb(&mut s.color);
        egui::ComboBox::from_id_salt("title-size")
            .selected_text(format!("{} pt", s.size))
            .width(70.0)
            .show_ui(ui, |ui| {
                for size in TITLE_SIZES {
                    ui.selectable_value(&mut s.size, size, format!("{size} pt"));
                }
            });
        for align in TitleAlign::ALL {
            ui.selectable_value(&mut s.align, align, align.label());
        }
        if style.is_some()
            && ui
                .button(icon::ARROW_COUNTER_CLOCKWISE)
                .on_hover_text("Use the global title style from Preferences")
                .clicked()
        {
            *style = None;
            return;
        }
        if s != style.unwrap_or(defaults) {
            *style = Some(s);
        }
    });
}

fn hint(ui: &mut Ui, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(RichText::new(text).font(bold(12.0)).color(TEXT_DIM));
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

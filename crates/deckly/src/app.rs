//! The editor window.
//!
//! Layout follows the official app (docs/clean-room/2026-10-25-main-window.md):
//! the canvas with the device header, key grid and page strip on the left,
//! the property inspector under it, and the action list on the right. The editor works on a copy
//! of the current profile each frame and writes it back when it changed.

use crate::actions::{self, ACTIONS, Category};
use crate::clipboard::{self, KeyClipboard};
use crate::history::{Direction, History, Location};
use crate::os::{Hotkey, MediaKey};
use crate::plugins::{self, KeyImage, Overlay};
use crate::prefs::{self, Preferences};
use crate::render::{Live, Renderer};
use crate::runtime::{self, Msg, NOTICE_TIME, Shared};
use crate::store;
use crate::theme::*;
use deckly_core::profile::MAX_FOLDER_DEPTH;
use deckly_core::{DeviceKind, KeyConfig, Page, Profile, TitleStyle};
use deckly_plugin_host::{Host, Status};
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2, pos2, vec2,
};
use egui_phosphor::regular as icon;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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

/// A move between a page and its folders, applied after the frame's panels.
#[derive(Clone, Debug, PartialEq)]
enum FolderNav {
    Open(String),
    Up,
    /// Keep this many folders open (0 goes back to the page).
    Keep(usize),
}

/// What the canvas shows: a page, or a folder opened from it.
struct View {
    page: usize,
    /// Open folders, outermost first.
    path: Vec<String>,
    live: Live,
}

impl View {
    fn location(&self, key: Option<u8>) -> Location {
        Location {
            page: self.page,
            path: self.path.clone(),
            key,
        }
    }

    fn keys<'a>(&self, profile: &'a mut Profile) -> Option<&'a mut BTreeMap<u8, KeyConfig>> {
        match self.path.last() {
            Some(id) => profile.folder_mut(id).map(|f| &mut f.keys),
            None => profile.pages.get_mut(self.page).map(|p| &mut p.keys),
        }
    }
}

#[derive(Clone, Debug)]
enum DragPayload {
    /// A built-in or plugin action, by UUID.
    Action(String),
    Key(u8),
}

enum KeyCommand {
    Copy(u8),
    Paste(u8, KeyClipboard),
}

struct PendingPaste {
    profile: String,
    page: String,
    index: u8,
    original: KeyConfig,
    clipboard: KeyClipboard,
}

impl PendingPaste {
    fn matches(&self, profile: &Profile, page_id: &str) -> bool {
        self.profile == profile.id
            && self.page == page_id
            && clipboard::page(profile, page_id).and_then(|p| p.keys.get(&self.index))
                == Some(&self.original)
    }
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
    /// Open categories in the action list, by name.
    expanded: HashSet<String>,
    /// `None` when the plugin host could not start.
    host: Option<Host>,
    /// Textures of plugin images, by image id.
    images: HashMap<u64, TextureHandle>,
    /// Where this frame's inspector panel shows a plugin's settings page.
    plugin_page_area: Option<Rect>,
    /// The key whose settings page was last asked for, so it is asked once.
    plugin_page_requested: Option<(String, String)>,
    /// The placement last sent to the settings page (None: not yet sent).
    plugin_page_placed: Option<Option<[i32; 4]>>,
    /// The editor window was handed to the plugin host as the owner of
    /// settings pages.
    owner_set: bool,
    recording_hotkey: bool,
    clipboard: Option<KeyClipboard>,
    pending_paste: Option<PendingPaste>,
    history: History,
    history_shortcut: Option<Direction>,
    dirty: HashSet<String>,
    last_edit: Instant,
    prefs: Preferences,
    rename: Option<(String, String)>,
    folder_nav: Option<FolderNav>,
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
        let repaint: runtime::Repaint = Arc::new(move || ctx.request_repaint());
        let (host, host_events) = start_host();
        if host.is_none() {
            runtime::lock(&state).notify("Plugins could not be started");
        }
        let runtime = runtime::start(state.clone(), repaint.clone(), host.clone());
        if let (Some(host), Some(events)) = (&host, host_events) {
            plugins::start(
                state.clone(),
                runtime.clone(),
                host.clone(),
                events,
                repaint,
            );
        }
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
            expanded: HashSet::from([Category::System.label().to_owned()]),
            host,
            images: HashMap::new(),
            plugin_page_area: None,
            plugin_page_requested: None,
            plugin_page_placed: None,
            owner_set: false,
            recording_hotkey: false,
            clipboard: None,
            pending_paste: None,
            history: History::default(),
            history_shortcut: None,
            dirty: HashSet::new(),
            last_edit: Instant::now(),
            prefs: Preferences::new(),
            rename: None,
            folder_nav: None,
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
        live: Live,
        overlay: Option<&Overlay>,
    ) -> egui::TextureId {
        let px = px.clamp(32, 288);
        let hash = self.renderer.key_hash(key, live, overlay);
        if let Some(t) = self.textures.get(&(hash, px)) {
            return t.id();
        }
        if self.textures.len() > 512 {
            self.textures.clear();
        }
        let img = self.renderer.render(key, px, live, overlay);
        let color = ColorImage::from_rgba_unmultiplied([px as usize, px as usize], img.as_raw());
        let tex = ctx.load_texture(format!("key-{hash:x}-{px}"), color, TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert((hash, px), tex);
        id
    }

    /// A texture for a plugin image (action and category icons).
    fn image_texture(&mut self, ctx: &egui::Context, image: &KeyImage) -> egui::TextureId {
        if self.images.len() > 512 {
            self.images.clear();
        }
        self.images
            .entry(image.id)
            .or_insert_with(|| {
                let size = [image.img.width() as usize, image.img.height() as usize];
                let color = ColorImage::from_rgba_unmultiplied(size, image.img.as_raw());
                ctx.load_texture(
                    format!("plugin-{:x}", image.id),
                    color,
                    TextureOptions::LINEAR,
                )
            })
            .id()
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
            (ListTab::Keys, format!("{}  Keys", icon::HAND_TAP)),
            (ListTab::Plugins, format!("{}  Plugins", icon::PUZZLE_PIECE)),
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
            ListTab::Plugins => self.plugin_list(ui),
        });
    }

    /// Installed plugins with their status, and buttons to add more.
    fn plugin_list(&mut self, ui: &mut Ui) {
        let Some(host) = self.host.clone() else {
            hint(ui, "Plugins could not be started.");
            return;
        };
        let catalog: Vec<Arc<plugins::CatalogPlugin>> =
            runtime::lock(&self.state).catalog.plugins.clone();
        let query = self.search.trim().to_lowercase();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(17.0);
            if ui
                .button(format!("{}  Install…", icon::DOWNLOAD_SIMPLE))
                .on_hover_text(
                    "Install a .streamDeckPlugin or .zip file (or drop one on the window)",
                )
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Plugin", &["streamDeckPlugin", "zip"])
                    .pick_file()
            {
                self.install(vec![path]);
            }
            if ui
                .button(format!("{}  Add Folder…", icon::FOLDER_PLUS))
                .on_hover_text("Install a <plugin id>.sdPlugin folder, or a folder holding one")
                .clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                self.install(vec![path]);
            }
            if ui
                .button(icon::FOLDER_OPEN)
                .on_hover_text("Open the plugins folder")
                .clicked()
            {
                open_path(&self.state, &host.plugins_dir().to_string_lossy());
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if catalog.is_empty() {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(icon::PUZZLE_PIECE)
                                .size(28.0)
                                .color(TEXT_FAINT),
                        );
                        ui.label(RichText::new("No plugins installed.").color(TEXT_DIM));
                    });
                }
                for p in catalog {
                    if !query.is_empty() && !p.name.to_lowercase().contains(&query) {
                        continue;
                    }
                    self.plugin_row(ui, &host, &p);
                }
            });
    }

    fn plugin_row(&mut self, ui: &mut Ui, host: &Host, p: &plugins::CatalogPlugin) {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, LIST_HOVER);
        }
        let icon_rect =
            Rect::from_center_size(pos2(rect.left() + 38.0, rect.center().y), Vec2::splat(32.0));
        match &p.icon {
            Some(img) => {
                let tex = self.image_texture(ui.ctx(), img);
                ui.painter().image(
                    tex,
                    icon_rect,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                ui.painter().text(
                    icon_rect.center(),
                    Align2::CENTER_CENTER,
                    icon::PUZZLE_PIECE,
                    font(24.0),
                    ITEM_ICON,
                );
            }
        }
        let x = rect.left() + 66.0;
        let name = if p.version.is_empty() {
            p.name.clone()
        } else {
            format!("{}  {}", p.name, p.version)
        };
        ui.painter().text(
            pos2(x, rect.top() + 16.0),
            Align2::LEFT_CENTER,
            name,
            bold(13.0),
            TEXT,
        );
        let official_only = p.official_only && !matches!(p.status, Status::Failed(_));
        let (status, color) = if official_only {
            (OFFICIAL_ONLY.to_owned(), WARNING)
        } else {
            status_text(&p.status)
        };
        ui.painter().text(
            pos2(x, rect.top() + 35.0),
            Align2::LEFT_CENTER,
            status,
            font(11.0),
            color,
        );
        let resp = resp.on_hover_text(match &p.status {
            _ if official_only => OFFICIAL_ONLY_WHY.to_owned(),
            Status::Failed(why) | Status::Restarting(why) => why.clone(),
            _ if p.author.is_empty() => p.id.clone(),
            _ => format!("{}\nby {}", p.id, p.author),
        });
        let log = host.logs_dir().join(format!("{}.log", p.id));
        resp.context_menu(|ui| {
            if ui
                .button(format!("{}  Restart", icon::ARROW_CLOCKWISE))
                .clicked()
            {
                host.restart(&p.id);
                ui.close();
            }
            if ui
                .button(format!("{}  Open Log", icon::FILE_TEXT))
                .clicked()
            {
                open_path(&self.state, &log.to_string_lossy());
                ui.close();
            }
            if ui
                .button(format!("{}  Open Folder", icon::FOLDER_OPEN))
                .clicked()
            {
                open_path(&self.state, &p.dir.to_string_lossy());
                ui.close();
            }
            ui.separator();
            if ui.button(format!("{}  Uninstall", icon::TRASH)).clicked() {
                let host = host.clone();
                let state = self.state.clone();
                let (id, name) = (p.id.clone(), p.name.clone());
                std::thread::spawn(move || {
                    let text = match host.remove(&id) {
                        Ok(()) => format!("{name} was uninstalled"),
                        Err(e) => format!("Cannot uninstall {name}: {e}"),
                    };
                    runtime::lock(&state).notify(text);
                });
                ui.close();
            }
        });
    }

    /// Installs plugin archives or folders off the UI thread.
    fn install(&mut self, paths: Vec<PathBuf>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let state = self.state.clone();
        std::thread::spawn(move || {
            for path in paths {
                let text = match host.install(&path) {
                    Ok(id) => {
                        let found = host.plugins().into_iter().find(|p| p.id == id);
                        let official_only = found.as_ref().is_some_and(|p| {
                            deckly_plugin_host::manifest::needs_official_app(&p.dir)
                        });
                        let name = found
                            .and_then(|p| p.manifest.map(|m| m.name.clone()))
                            .unwrap_or(id);
                        if official_only {
                            format!("{name} was installed, but it works only in the official app")
                        } else {
                            format!("{name} was installed")
                        }
                    }
                    Err(e) => {
                        let file = path.file_name().map_or_else(
                            || path.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        format!("Cannot install {file}: {e}")
                    }
                };
                runtime::lock(&state).notify(text);
            }
        });
        self.tab = ListTab::Plugins;
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
                    let open = self.category_row(ui, category.label(), &query, |p, r| {
                        p.text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            category.icon(),
                            font(22.0),
                            TEXT,
                        );
                    });
                    if open {
                        for action in items {
                            let glyph = |p: &egui::Painter, r: Rect, color| {
                                p.text(
                                    r.center(),
                                    Align2::CENTER_CENTER,
                                    action.icon,
                                    font(19.0),
                                    color,
                                );
                            };
                            action_row(ui, action.uuid, action.name, action.tooltip, glyph);
                        }
                    }
                    ui.add_space(2.0);
                }
                // Plugin actions, one category per plugin.
                let catalog = runtime::lock(&self.state).catalog.plugins.clone();
                for plugin in catalog {
                    let items: Vec<&plugins::CatalogAction> = plugin
                        .actions
                        .iter()
                        .filter(|a| a.visible)
                        .filter(|a| query.is_empty() || a.name.to_lowercase().contains(&query))
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    let icon_tex = plugin
                        .category_icon
                        .as_ref()
                        .or(plugin.icon.as_ref())
                        .map(|img| self.image_texture(ui.ctx(), img));
                    let open = self.category_row(ui, &plugin.category, &query, |p, r| {
                        draw_list_icon(p, r.shrink(4.0), icon_tex, TEXT);
                    });
                    if open {
                        for action in items {
                            let tex = action
                                .icon
                                .as_ref()
                                .map(|img| self.image_texture(ui.ctx(), img));
                            let glyph = |p: &egui::Painter, r: Rect, color| {
                                draw_list_icon(p, r.shrink(5.0), tex, color);
                            };
                            action_row(ui, &action.uuid, &action.name, &action.tooltip, glyph);
                        }
                    }
                    ui.add_space(2.0);
                }
            });
    }

    /// A category header in the action list; returns whether it is open.
    /// Collapsed categories open while searching.
    fn category_row(
        &mut self,
        ui: &mut Ui,
        label: &str,
        query: &str,
        draw_icon: impl FnOnce(&egui::Painter, Rect),
    ) -> bool {
        let open = !query.is_empty() || self.expanded.contains(label);
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), CATEGORY_ROW), Sense::click());
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
        draw_icon(
            p,
            Rect::from_center_size(pos2(rect.left() + 55.0, y), Vec2::splat(24.0)),
        );
        p.text(
            pos2(rect.left() + 76.0, y),
            Align2::LEFT_CENTER,
            label,
            bold(13.0),
            TEXT,
        );
        if resp.clicked() && !self.expanded.remove(label) {
            self.expanded.insert(label.to_owned());
        }
        open
    }

    // ----- key canvas ----------------------------------------------------

    fn canvas(
        &mut self,
        ui: &mut Ui,
        device: Option<&deckly_device::ConnectedDevice>,
        profile: &mut Profile,
        view: &mut View,
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
        let mut command = None;
        let live = view.live;
        let page_id = view
            .path
            .last()
            .cloned()
            .or_else(|| profile.pages.get(view.page).map(|p| p.id.clone()))
            .unwrap_or_default();
        let overlays: HashMap<u8, Overlay> = match view.keys(profile) {
            Some(keys) => {
                let s = runtime::lock(&self.state);
                keys.iter()
                    .filter_map(|(&i, k)| Some((i, s.overlay(&plugins::context(&page_id, i), k)?)))
                    .collect()
            }
            None => HashMap::new(),
        };
        if overlays.values().any(|o| o.mark.is_some()) {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        let Some(keys) = view.keys(profile) else {
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
            // The parent folder key stays in its folder; it can only move.
            let placed = config
                .as_ref()
                .is_some_and(|k| k.action == actions::PARENT_FOLDER);
            if config.is_some() {
                response.dnd_set_drag_payload(DragPayload::Key(index));
            }
            // The live view shrinks a held key like the device does.
            let press = self.renderer.key_press;
            if press.shrink && pressed.contains(&index) {
                rect = rect.shrink(key_size * f32::from(100 - press.size) / 200.0);
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
                    let tex = self.texture(ui.ctx(), Some(key), tex_px, live, overlays.get(&index));
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
                match &*payload {
                    DragPayload::Action(_) if placed => {}
                    DragPayload::Action(uuid) => match actions::find(uuid) {
                        Some(def) => {
                            keys.insert(index, actions::new_key(def));
                        }
                        None => {
                            keys.insert(index, KeyConfig::new(uuid.as_str(), json!({})));
                        }
                    },
                    &DragPayload::Key(from) if from != index => {
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
                response.request_focus();
            }
            // Double-clicking a folder opens it, as in the official app.
            if response.double_clicked()
                && let Some(key) = &config
            {
                if placed {
                    self.folder_nav = Some(FolderNav::Up);
                } else if let Some(id) = key.folder().filter(|id| !id.is_empty()) {
                    self.folder_nav = Some(FolderNav::Open(id.to_owned()));
                }
            }
            response.context_menu(|ui| {
                if ui
                    .add_enabled(
                        config.is_some() && !placed,
                        egui::Button::new(format!("{}  Copy", icon::COPY)).shortcut_text(
                            ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
                                egui::Modifiers::COMMAND,
                                egui::Key::C,
                            )),
                        ),
                    )
                    .clicked()
                {
                    command = Some(KeyCommand::Copy(index));
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.clipboard.is_some() && !placed,
                        egui::Button::new(format!("{}  Paste", icon::CLIPBOARD)).shortcut_text(
                            ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
                                egui::Modifiers::COMMAND,
                                egui::Key::V,
                            )),
                        ),
                    )
                    .clicked()
                {
                    if let Some(c) = self.clipboard.clone() {
                        command = Some(KeyCommand::Paste(index, c));
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(
                        config.is_some() && !placed,
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

        // Grid focus is distinct from text fields, buttons and hotkey recording.
        let grid_focus = self.selected_key.map(|index| ui.id().with(("key", index)));
        let editor_shortcuts = !self.recording_hotkey
            && self.rename.is_none()
            && self.pending_paste.is_none()
            && !egui::Popup::is_any_open(ui.ctx())
            && ui.ctx().memory(|m| m.top_modal_layer().is_none());
        let shortcuts = editor_shortcuts
            && ui
                .ctx()
                .memory(|m| m.focused().is_none_or(|id| Some(id) == grid_focus));
        if let Some(direction) =
            history_shortcut(ui.ctx(), editor_shortcuts && !ui.ctx().text_edit_focused())
        {
            self.history_shortcut = Some(direction);
        }
        if shortcuts
            && let Some(index) = self.selected_key
            && ui.input(|i| i.key_pressed(egui::Key::Delete))
            && keys
                .get(&index)
                .is_some_and(|k| k.action != actions::PARENT_FOLDER)
            && keys.remove(&index).is_some()
        {
            changed = true;
        }
        if changed {
            self.mark_dirty(&profile.id);
        }
        if let Some(command) = command {
            self.key_command(ui.ctx(), profile, &page_id, view.path.len(), command);
        }
        // winit maps Ctrl (Windows/Linux) or Cmd (macOS) to these events,
        // including the physical-key fallback for non-Latin layouts.
        for event in clipboard_events(ui.ctx(), shortcuts) {
            let Some(index) = self.selected_key else {
                continue;
            };
            match event {
                egui::Event::Copy => self.key_command(
                    ui.ctx(),
                    profile,
                    &page_id,
                    view.path.len(),
                    KeyCommand::Copy(index),
                ),
                egui::Event::Paste(text) => match KeyClipboard::from_text(&text) {
                    Ok(Some(copy)) => self.key_command(
                        ui.ctx(),
                        profile,
                        &page_id,
                        view.path.len(),
                        KeyCommand::Paste(index, copy),
                    ),
                    Ok(None) => {}
                    Err(e) => runtime::lock(&self.state).notify(e),
                },
                _ => {}
            }
        }

        if view.path.is_empty() {
            self.pager(ui, &geo, profile, &mut view.page);
        } else {
            self.folder_strip(ui, &geo, profile, view);
        }

        // Preview of what is being dragged.
        if let Some(payload) = egui::DragAndDrop::payload::<DragPayload>(ui.ctx())
            && let Some(pos) = ui.ctx().pointer_interact_pos()
        {
            let glyph = match &*payload {
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

    /// Where the canvas is inside folders, in place of the page strip: the
    /// page the folders were opened from, then each folder. Clicking one
    /// goes back to it.
    fn folder_strip(&mut self, ui: &mut Ui, geo: &CanvasGeometry, profile: &Profile, view: &View) {
        const HEIGHT: f32 = 20.0;
        const PAD: f32 = 10.0;
        let mut crumbs = vec![format!(
            "{}  Page {}",
            icon::ARROW_BEND_LEFT_UP,
            view.page + 1
        )];
        let mut parent = profile.pages.get(view.page);
        for id in &view.path {
            let title = parent
                .and_then(|p| p.keys.values().find(|k| k.folder() == Some(id.as_str())))
                .and_then(|k| k.title.lines().map(str::trim).find(|l| !l.is_empty()))
                .map_or_else(|| "Folder".to_owned(), |t| t.chars().take(24).collect());
            crumbs.push(title);
            parent = profile.folder(id);
        }
        let painter = ui.painter().clone();
        let galleys: Vec<_> = crumbs
            .into_iter()
            .map(|c| painter.layout_no_wrap(c, font(12.0), TEXT))
            .collect();
        let sep = painter.layout_no_wrap(icon::CARET_RIGHT.to_owned(), font(10.0), TEXT_FAINT);
        let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 2.0 * PAD).collect();
        let width = widths.iter().sum::<f32>() + sep.size().x * (widths.len() - 1) as f32 + 4.0;
        let strip = Rect::from_center_size(
            pos2(
                geo.grid.center().x,
                geo.grid.bottom() + CanvasGeometry::PAGER_GAP * geo.scale.min(1.0),
            ),
            vec2(width, HEIGHT),
        );
        painter.rect_filled(strip, CornerRadius::same(10), PAGER);
        let last = galleys.len() - 1;
        let mut x = strip.left() + 2.0;
        for (i, (galley, w)) in galleys.into_iter().zip(widths).enumerate() {
            let r = Rect::from_min_size(pos2(x, strip.top()), vec2(w, HEIGHT));
            let resp = ui.interact(r, ui.id().with(("crumb", i)), Sense::click());
            let color = if i == last {
                painter.rect_filled(r.shrink2(vec2(1.0, 1.0)), CornerRadius::same(9), PAGER_CHIP);
                PAGER_CHIP_TEXT
            } else if resp.hovered() {
                TEXT
            } else {
                TEXT_DIM
            };
            let size = galley.size();
            painter.galley_with_override_text_color(r.center() - size / 2.0, galley, color);
            if resp.clicked() && i != last {
                self.folder_nav = Some(FolderNav::Keep(i));
            }
            x += w;
            if i != last {
                let at = pos2(x, strip.center().y - sep.size().y / 2.0);
                painter.galley(at, sep.clone(), TEXT_FAINT);
                x += sep.size().x;
            }
        }
    }

    // ----- property inspector --------------------------------------------

    fn inspector(&mut self, ui: &mut Ui, profile: &mut Profile, view: &View) {
        let panel = ui.max_rect();
        let page_id = view
            .path
            .last()
            .cloned()
            .or_else(|| profile.pages.get(view.page).map(|p| p.id.clone()))
            .unwrap_or_default();
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
        let Some(key) = view.keys(profile).and_then(|k| k.get_mut(&index)) else {
            hint(ui, "Drag an action from the list onto this key.");
            return;
        };
        let before = key.clone();
        let defaults = self.renderer.title_defaults;
        let def = actions::find(&key.action);
        let context = plugins::context(&page_id, index);
        let (overlay, plugin) = {
            let s = runtime::lock(&self.state);
            let plugin = s.catalog.action(&key.action).map(|(p, a)| PluginView {
                name: p.name.clone(),
                action: a.name.clone(),
                tooltip: a.tooltip.clone(),
                icon: a.icon.clone(),
                status: p.status.clone(),
                official_only: p.official_only,
                has_inspector: a.has_inspector,
                instance: s.instance(&context, key),
            });
            (s.overlay(&context, key), plugin)
        };
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            match &plugin {
                Some(p) => {
                    let tex = p.icon.as_ref().map(|img| self.image_texture(ui.ctx(), img));
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    draw_list_icon(ui.painter(), r, tex, ITEM_ICON);
                    ui.label(RichText::new(&p.action).font(bold(14.0)));
                    ui.label(RichText::new(format!("{}  {}", p.name, p.tooltip)).color(TEXT_DIM));
                }
                None => {
                    let glyph = def.map_or(icon::PUZZLE_PIECE, |d| d.icon);
                    ui.label(RichText::new(glyph).size(18.0).color(ITEM_ICON));
                    let name = def.map_or(key.action.as_str(), |d| d.name);
                    ui.label(RichText::new(name).font(bold(14.0)));
                    match def {
                        Some(d) => ui.label(RichText::new(d.tooltip).color(TEXT_DIM)),
                        None => ui.label(RichText::new("Plugin not installed").color(WARNING)),
                    };
                }
            }
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        let ppp = ui.ctx().pixels_per_point();
        let mut form = Rect::NOTHING;
        ui.horizontal_top(|ui| {
            ui.add_space(16.0);
            // Key preview and image controls.
            ui.vertical(|ui| {
                ui.set_width(110.0);
                let tex = self.texture(
                    ui.ctx(),
                    Some(key),
                    (96.0 * ppp) as u32,
                    view.live,
                    overlay.as_ref(),
                );
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
            form = ui
                .vertical(|ui| {
                    egui::Grid::new("key-form")
                        .num_columns(2)
                        .spacing(vec2(14.0, 10.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new("Title").color(TEXT_DIM));
                            ui.horizontal_top(|ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut key.title)
                                        .desired_rows(1)
                                        .desired_width(260.0),
                                );
                                title_style_button(ui, &mut key.title_style, defaults);
                            });
                            ui.end_row();
                            match &plugin {
                                Some(p) => self.plugin_form(ui, p),
                                None => self.action_form(ui, key, &profiles, page_count),
                            }
                        });
                })
                .response
                .rect;
        });
        // A plugin's settings page fills the rest of the panel, framed and
        // labelled so it is clear this part is the plugin's own web page.
        if let Some(p) = plugin
            .as_ref()
            .filter(|p| p.has_inspector && INLINE_PLUGIN_PAGES)
        {
            let area = Rect::from_min_max(
                pos2(form.left(), form.bottom() + 30.0),
                pos2(panel.right() - 16.0, panel.bottom() - 12.0),
            );
            self.plugin_page(ui, area, p);
        }
        if *key != before {
            self.mark_dirty(&profile.id);
        }
    }

    /// The inspector row of a plugin action. Its settings page, a web page
    /// of the plugin's, shows below the form where the editor can place it
    /// (`plugin_page`); elsewhere it opens in a window of its own.
    fn plugin_form(&mut self, ui: &mut Ui, p: &PluginView) {
        ui.label(RichText::new("Settings").color(TEXT_DIM));
        ui.vertical(|ui| {
            if p.has_inspector && INLINE_PLUGIN_PAGES {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::PUZZLE_PIECE).color(TEXT_DIM));
                    ui.label(
                        RichText::new(format!(
                            "Web page from the {} plugin, not part of Deckly",
                            p.name
                        ))
                        .color(TEXT_DIM),
                    );
                });
            } else if p.has_inspector {
                let open = self
                    .host
                    .as_ref()
                    .and_then(Host::inspector)
                    .is_some_and(|i| i.context == p.instance.context);
                let text = if open {
                    format!("{}  Close Settings", icon::X)
                } else {
                    format!("{}  Open Settings", icon::SLIDERS_HORIZONTAL)
                };
                if ui.button(text).clicked()
                    && let Some(host) = &self.host
                {
                    if open {
                        host.close_inspector();
                    } else {
                        host.open_inspector(p.instance.clone());
                    }
                }
            } else {
                ui.label(RichText::new("This action has no settings.").color(TEXT_DIM));
            }
            let (status, color) = status_text(&p.status);
            if p.official_only && !matches!(p.status, Status::Failed(_)) {
                ui.label(RichText::new(OFFICIAL_ONLY).color(WARNING))
                    .on_hover_text(OFFICIAL_ONLY_WHY);
            } else if p.status != Status::Running {
                ui.label(RichText::new(status).color(color));
            }
        });
        ui.end_row();
    }

    /// Frames the area where a plugin's settings page is drawn and opens the
    /// page for this key. The page runs in `deckly-web`; its window is placed
    /// over this area after the frame (`place_plugin_page`).
    fn plugin_page(&mut self, ui: &mut Ui, area: Rect, p: &PluginView) {
        let area = area.intersect(ui.clip_rect());
        if !area.is_positive() {
            return;
        }
        let Some(host) = self.host.clone() else {
            return;
        };
        let requested = (p.instance.context.clone(), p.instance.action.clone());
        let open = host
            .inspector()
            .is_some_and(|i| i.context == p.instance.context && i.action == p.instance.action);
        if !open && self.plugin_page_requested.as_ref() != Some(&requested) {
            host.open_inspector(p.instance.clone());
            self.plugin_page_requested = Some(requested);
        }
        let painter = ui.painter();
        painter.rect_filled(area.expand(1.0), CornerRadius::same(4), KEY_BORDER);
        painter.rect_filled(area, CornerRadius::same(3), CANVAS);
        if open {
            self.plugin_page_area = Some(area);
        } else {
            painter.text(
                area.center(),
                Align2::CENTER_CENTER,
                "Loading the plugin's settings page…",
                font(12.0),
                TEXT_DIM,
            );
            let retry = Rect::from_center_size(area.center() + vec2(0.0, 26.0), vec2(120.0, 24.0));
            if area.contains_rect(retry)
                && ui.put(retry, egui::Button::new("Retry settings")).clicked()
            {
                host.open_inspector(p.instance.clone());
            }
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
    }

    /// Puts the plugin settings page window over its area of the panel, or
    /// hides it while something of Deckly's own would be under it.
    fn place_plugin_page(&mut self, ctx: &egui::Context) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let covered = egui::Popup::is_any_open(ctx)
            || self.rename.is_some()
            || self.pending_paste.is_some()
            || ctx.memory(|m| m.top_modal_layer().is_some());
        let (inner, minimized) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().minimized));
        let place = self
            .plugin_page_area
            .take()
            .filter(|_| !covered && minimized != Some(true))
            .zip(inner)
            .and_then(|(area, inner)| plugin_page_placement(area, inner, ctx.pixels_per_point()));
        if self.plugin_page_placed != Some(place) {
            host.place_inspector(place);
            self.plugin_page_placed = Some(place);
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
            actions::CREATE_FOLDER => {
                label(ui, "Folder");
                let id = key.folder().filter(|id| !id.is_empty());
                if ui
                    .add_enabled(
                        id.is_some(),
                        egui::Button::new(format!("{}  Open Folder", icon::FOLDER_OPEN)),
                    )
                    .on_hover_text("Double-clicking the key opens it too")
                    .clicked()
                    && let Some(id) = id
                {
                    self.folder_nav = Some(FolderNav::Open(id.to_owned()));
                }
                ui.end_row();
            }
            actions::PARENT_FOLDER => {
                label(ui, "Folder");
                if ui
                    .button(format!("{}  Open Parent Folder", icon::ARROW_BEND_LEFT_UP))
                    .clicked()
                {
                    self.folder_nav = Some(FolderNav::Up);
                }
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

    fn finish_edit(
        &mut self,
        ctx: &egui::Context,
        profile: &mut Profile,
        view: &mut View,
        original: &Profile,
        original_location: Location,
        group: Option<u64>,
    ) {
        if profile != original {
            profile.normalize_folders();
            if let Err(e) = self.history.record(
                original,
                profile,
                original_location,
                view.location(self.selected_key),
                group,
            ) {
                runtime::lock(&self.state).notify(e);
            }
        }
        if let Some(direction) = self.history_shortcut.take() {
            self.apply_history(ctx, profile, view, direction);
        }
    }

    fn apply_history(
        &mut self,
        ctx: &egui::Context,
        profile: &mut Profile,
        view: &mut View,
        direction: Direction,
    ) {
        match self.history.step(profile, direction) {
            Ok(Some((restored, location))) => {
                *profile = restored;
                view.page = location.page.min(profile.pages.len().saturating_sub(1));
                view.path = location.path;
                view.live.page = view.page + 1;
                self.selected_key = location.key.filter(|k| *k < profile.model().key_count());
                self.recording_hotkey = false;
                self.pending_paste = None;
                self.folder_nav = None;
                // Reopen the inspector from restored settings, even for the same UUID.
                if let Some(host) = &self.host {
                    host.close_inspector();
                }
                self.plugin_page_requested = None;
                self.plugin_page_area = None;
                self.mark_dirty(&profile.id);
                ctx.request_repaint();
            }
            Ok(None) => {}
            Err(e) => runtime::lock(&self.state).notify(e),
        }
    }

    fn key_command(
        &mut self,
        ctx: &egui::Context,
        profile: &mut Profile,
        page_id: &str,
        folder_depth: usize,
        command: KeyCommand,
    ) {
        if self.pending_paste.is_some() {
            return;
        }
        match command {
            KeyCommand::Copy(index) => {
                if let Some(copy) = KeyClipboard::copy(profile, page_id, index) {
                    match copy.text() {
                        Ok(text) => {
                            ctx.copy_text(text);
                            self.clipboard = Some(copy);
                        }
                        Err(e) => runtime::lock(&self.state).notify(e),
                    }
                }
            }
            KeyCommand::Paste(index, copy) => {
                let original = clipboard::page(profile, page_id).and_then(|p| p.keys.get(&index));
                if original.is_some_and(|k| k.action == actions::PARENT_FOLDER) {
                    return;
                }
                self.selected_key = Some(index);
                self.recording_hotkey = false;
                self.clipboard = Some(copy.clone());
                if let Some(original) = original {
                    self.pending_paste = Some(PendingPaste {
                        profile: profile.id.clone(),
                        page: page_id.to_owned(),
                        index,
                        original: original.clone(),
                        clipboard: copy,
                    });
                    ctx.request_repaint();
                } else {
                    match copy.paste(profile, page_id, index, folder_depth) {
                        Ok(()) => self.mark_dirty(&profile.id),
                        Err(e) => runtime::lock(&self.state).notify(e),
                    }
                }
            }
        }
    }

    fn paste_confirmation(&mut self, ctx: &egui::Context, profile: &mut Profile, view: &View) {
        let Some(pending) = self.pending_paste.take() else {
            return;
        };
        let page_id = view
            .path
            .last()
            .map(String::as_str)
            .or_else(|| profile.pages.get(view.page).map(|p| p.id.as_str()))
            .unwrap_or_default();
        if !pending.matches(profile, page_id) {
            runtime::lock(&self.state).notify("Paste cancelled: the destination changed");
            return;
        }
        let mut replace = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("replace-action")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("Replace action?");
            ui.add_space(8.0);
            ui.label(format!(
                "Key {} already has an action. Are you sure you want to replace it?",
                usize::from(pending.index) + 1,
            ));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                cancel = ui.button("Cancel").clicked();
                replace = ui.button("Replace").clicked();
            });
        });
        if replace {
            match pending
                .clipboard
                .paste(profile, &pending.page, pending.index, view.path.len())
            {
                Ok(()) => self.mark_dirty(&profile.id),
                Err(e) => runtime::lock(&self.state).notify(e),
            }
        } else if !cancel && !modal.should_close() {
            self.pending_paste = Some(pending);
        }
    }

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
        let width = (ctx.content_rect().width() - 64.0).clamp(120.0, 440.0);
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
                        // Without a fixed width the anchored area shrinks to a narrow column.
                        ui.set_width(width);
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new(icon::WARNING).color(WARNING));
                            ui.add(egui::Label::new(RichText::new(&text).color(WARNING)).wrap());
                        });
                    });
            });
        ctx.request_repaint_after(left);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.plugin_page_area = None;
        // Set the owner before a selected key can request its settings page.
        if !self.owner_set
            && let Some(host) = &self.host
            && let Some(handle) = window_handle(frame)
        {
            host.set_owner_window(handle);
            self.owner_set = true;
        }
        let (title_style, key_press) = {
            let s = runtime::lock(&self.state);
            (s.settings.title_style, s.settings.key_press)
        };
        self.renderer.title_defaults = title_style;
        self.renderer.key_press = key_press;
        let (device, profile_id) = self.resolve();
        let (mut profile, mut view) = {
            let s = runtime::lock(&self.state);
            match s.profile(&profile_id) {
                Some(p) => (
                    p.clone(),
                    View {
                        page: s.page(p),
                        path: s.folder_path(p),
                        live: s.live(p),
                    },
                ),
                None => return,
            }
        };
        let original = profile.clone();
        let original_location = view.location(self.selected_key);
        let (original_page, original_path) = (view.page, view.path.clone());

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
            .show(ui, |ui| self.inspector(ui, &mut profile, &view));
        let typing = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Text(_)
                        | egui::Event::Paste(_)
                        | egui::Event::Key { pressed: true, .. }
                )
            })
        });
        let inspector_edit = (typing && profile != original)
            .then(|| ctx.memory(|m| m.focused()))
            .flatten()
            .map(|focus| {
                (
                    egui::Id::new((&profile.id, view.page, &view.path, self.selected_key, focus))
                        .value(),
                    profile.clone(),
                )
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), CornerRadius::ZERO, CANVAS);
            self.canvas(ui, device.as_ref(), &mut profile, &mut view);
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

        if let Some(nav) = self.folder_nav.take() {
            match nav {
                FolderNav::Open(id) if view.path.len() < MAX_FOLDER_DEPTH => view.path.push(id),
                FolderNav::Open(_) => {}
                FolderNav::Up => {
                    view.path.pop();
                }
                FolderNav::Keep(n) => view.path.truncate(n),
            }
            self.selected_key = None;
            self.recording_hotkey = false;
        }

        self.paste_confirmation(&ctx, &mut profile, &view);

        // Write the edited copy back. Profile switches made this frame
        // already changed which profile is current; only write if it still
        // exists.
        let group = inspector_edit
            .filter(|(_, edited)| edited == &profile)
            .map(|(id, _)| id);
        self.finish_edit(
            &ctx,
            &mut profile,
            &mut view,
            &original,
            original_location,
            group,
        );
        if profile != original || view.page != original_page || view.path != original_path {
            let mut s = runtime::lock(&self.state);
            if let Some(p) = s.profile_mut(&profile.id) {
                *p = profile.clone();
            }
            s.set_page(&profile.id, view.page);
            s.set_folder_path(&profile.id, view.path.clone());
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
        // Plugins dropped on the window are installed.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
                    p.is_dir()
                        || ext.eq_ignore_ascii_case("streamDeckPlugin")
                        || ext.eq_ignore_ascii_case("zip")
                })
                .collect()
        });
        if !dropped.is_empty() {
            self.install(dropped);
        }
        // The settings window follows the selected key.
        if let Some(host) = &self.host {
            let page_id = view
                .path
                .last()
                .cloned()
                .or_else(|| profile.pages.get(view.page).map(|p| p.id.clone()))
                .unwrap_or_default();
            let selected = self.selected_key.and_then(|k| {
                let key = view.keys(&mut profile)?.get(&k)?;
                Some((plugins::context(&page_id, k), key.action.clone()))
            });
            let stale_request = self
                .plugin_page_requested
                .as_ref()
                .is_some_and(|requested| Some(requested) != selected.as_ref());
            let stale_open = host.inspector().is_some_and(|open| {
                Some((open.context, open.action)) != selected
                    && (!INLINE_PLUGIN_PAGES || self.plugin_page_requested != selected)
            });
            if stale_request || stale_open {
                host.close_inspector();
                self.plugin_page_requested = None;
                self.plugin_page_area = None;
            }
        }
        if INLINE_PLUGIN_PAGES {
            self.place_plugin_page(&ctx);
        }
        self.save_pending(false);
        if !self.dirty.is_empty() {
            ctx.request_repaint_after(SAVE_DELAY);
        }
    }

    fn on_exit(&mut self) {
        self.save_pending(true);
        if let Some(host) = &self.host {
            host.shutdown();
        }
        let _ = self.runtime.send(Msg::Shutdown);
        // Give the writer threads a moment to send the reset.
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// One action in the list, dragged onto a key to place it.
fn action_row(
    ui: &mut Ui,
    uuid: &str,
    name: &str,
    tooltip: &str,
    draw_icon: impl FnOnce(&egui::Painter, Rect, Color32),
) {
    let id = egui::Id::new(("action", uuid));
    let resp = ui
        .dnd_drag_source(id, DragPayload::Action(uuid.to_owned()), |ui| {
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), ACTION_ROW), Sense::hover());
            let p = ui.painter();
            if resp.hovered() {
                p.rect_filled(rect, CornerRadius::ZERO, LIST_HOVER);
            }
            draw_icon(
                p,
                Rect::from_center_size(
                    pos2(rect.left() + 55.0, rect.center().y),
                    Vec2::splat(24.0),
                ),
                ITEM_ICON,
            );
            let color = if resp.hovered() { TEXT } else { TEXT_DIM };
            p.text(
                pos2(rect.left() + 76.0, rect.center().y),
                Align2::LEFT_CENTER,
                name,
                font(13.0),
                color,
            );
        })
        .response;
    if !tooltip.is_empty() {
        resp.on_hover_text(tooltip);
    }
}

/// A plugin image in the action list, tinted like the built-in icons; a
/// puzzle piece when the plugin has none.
fn draw_list_icon(p: &egui::Painter, rect: Rect, tex: Option<egui::TextureId>, tint: Color32) {
    match tex {
        Some(tex) => {
            p.image(
                tex,
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                tint,
            );
        }
        None => {
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                icon::PUZZLE_PIECE,
                font(rect.height() * 0.8),
                tint,
            );
        }
    }
}

/// Plugin settings pages sit inside the inspector panel where the system
/// lets one process's window belong to another's (Windows).
// shortcut: macOS and Linux open them in a window of their own; embed them
// there too once the helper can follow the editor window on those systems.
const INLINE_PLUGIN_PAGES: bool = cfg!(windows);

/// Convert both edges to physical pixels so fractional scaling leaves no gap.
fn plugin_page_placement(area: Rect, inner: Rect, ppp: f32) -> Option<[i32; 4]> {
    let area = area.intersect(Rect::from_min_size(egui::Pos2::ZERO, inner.size()));
    if !area.is_positive() || !ppp.is_finite() || ppp <= 0.0 {
        return None;
    }
    let min = area.min * ppp;
    let max = area.max * ppp;
    let (x, y) = (min.x.round() as i32, min.y.round() as i32);
    let (w, h) = (max.x.round() as i32 - x, max.y.round() as i32 - y);
    (w > 0 && h > 0).then_some([x, y, w, h])
}

/// The editor window's native handle (a Windows HWND).
fn window_handle(frame: &eframe::Frame) -> Option<i64> {
    use eframe::egui_wgpu::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as i64),
        _ => None,
    }
}

/// What the inspector shows for a plugin key.
struct PluginView {
    name: String,
    action: String,
    tooltip: String,
    icon: Option<plugins::Image>,
    status: Status,
    official_only: bool,
    has_inspector: bool,
    instance: deckly_plugin_host::Instance,
}

const OFFICIAL_ONLY: &str = "Works only in the official app";
const OFFICIAL_ONLY_WHY: &str = "This plugin checks that the official, signed Stream Deck \
    app started it, and turns itself off otherwise. Deckly does not work around that \
    check; only the plugin's author can change it.";

fn status_text(status: &Status) -> (String, Color32) {
    match status {
        Status::Running => ("Running".into(), TEXT_DIM),
        Status::Starting => ("Starting…".into(), TEXT_DIM),
        Status::Restarting(why) => (format!("Restarting: {why}"), WARNING),
        Status::Installing(what) => (format!("Installing {what}…"), TEXT_DIM),
        Status::Failed(why) => (format!("Not running: {why}"), WARNING),
    }
}

fn open_path(state: &Shared, path: &str) {
    if let Err(e) = crate::os::open(path) {
        runtime::lock(state).notify(e);
    }
}

/// Starts the plugin host; plugins live in the config folder and run with
/// the `deckly-web` helper installed next to Deckly.
fn start_host() -> (
    Option<Host>,
    Option<std::sync::mpsc::Receiver<deckly_plugin_host::HostEvent>>,
) {
    let Some(dir) = store::config_dir() else {
        return (None, None);
    };
    let helper = std::env::current_exe().ok().and_then(|exe| {
        let name = if cfg!(windows) {
            "deckly-web.exe"
        } else {
            "deckly-web"
        };
        Some(exe.parent()?.join(name))
    });
    let config = deckly_plugin_host::Config {
        plugins_dir: dir.join("plugins"),
        logs_dir: dir.join("logs").join("plugins"),
        web_helper: helper,
        node: std::env::var_os("DECKLY_NODE").map(PathBuf::from),
        guard: std::env::current_exe().ok(),
        runtimes_dir: dir.join("runtimes"),
        app: deckly_plugin_host::AppInfo {
            language: "en".into(),
            platform_version: String::new(),
            font: if cfg!(windows) {
                "Segoe UI".into()
            } else {
                String::new()
            },
        },
    };
    let (tx, rx) = std::sync::mpsc::channel();
    match Host::start(config, tx) {
        Ok(host) => (Some(host), Some(rx)),
        Err(e) => {
            eprintln!("deckly: cannot start plugins: {e}");
            (None, None)
        }
    }
}

/// The "T" button beside the title field. It opens the title's show,
/// position, size and colour, like the official app's. Editing makes the key
/// keep its own style; the reset button makes it follow the global style
/// again.
fn title_style_button(ui: &mut Ui, style: &mut Option<TitleStyle>, defaults: TitleStyle) {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 24.0), Sense::click());
    let resp = resp.on_hover_text("Title style");
    let popup = egui::Popup::from_toggle_button_response(&resp)
        // Clicks in the colour picker's own popup must not close this one.
        .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(4.0)
        .frame(egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(12)));
    let popup_id = popup.get_id();
    let lit = popup.is_open() || resp.hovered();
    let p = ui.painter();
    if lit {
        p.rect_filled(rect, CornerRadius::same(3), FIELD);
    }
    let color = if lit { TEXT } else { TEXT_DIM };
    let y = rect.center().y;
    p.text(
        pos2(rect.left() + 13.0, y),
        Align2::CENTER_CENTER,
        icon::TEXT_T,
        font(17.0),
        color,
    );
    p.text(
        pos2(rect.right() - 9.0, y),
        Align2::CENTER_CENTER,
        icon::CARET_DOWN,
        font(11.0),
        color,
    );

    let mut s = style.unwrap_or(defaults);
    let mut reset = false;
    let shown = popup.show(|ui| {
        let (area, _) = ui.allocate_exact_size(vec2(252.0, 61.0), Sense::hover());
        prefs::title_style_controls(ui, area.min, &mut s);
        let r = Rect::from_min_size(area.min + vec2(228.0, 0.0), Vec2::splat(24.0));
        ui.painter().rect_stroke(
            r,
            CornerRadius::same(3),
            Stroke::new(1.0, PREF_SEGMENT),
            egui::StrokeKind::Inside,
        );
        reset = prefs::icon_button(ui, r, icon::ARROW_COUNTER_CLOCKWISE, style.is_some())
            .on_hover_text("Use the global title style from Preferences")
            .clicked();
    });
    // Close on a click in the window itself, not in this or another popup.
    if shown.is_some()
        && !resp.clicked()
        && let Some(pos) = ui.input(|i| {
            i.pointer
                .any_click()
                .then(|| i.pointer.interact_pos())
                .flatten()
        })
        && ui
            .ctx()
            .layer_id_at(pos)
            .is_none_or(|l| matches!(l.order, egui::Order::Background | egui::Order::Middle))
    {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }

    if reset {
        *style = None;
    } else if s != style.unwrap_or(defaults) {
        *style = Some(s);
    }
}

fn hint(ui: &mut Ui, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(RichText::new(text).font(bold(12.0)).color(TEXT_DIM));
    });
}

fn clipboard_events(ctx: &egui::Context, enabled: bool) -> Vec<egui::Event> {
    if !enabled {
        return Vec::new();
    }
    ctx.input_mut(|i| {
        let mut events = Vec::new();
        i.events.retain(|event| {
            if matches!(event, egui::Event::Copy | egui::Event::Paste(_)) {
                events.push(event.clone());
                false
            } else {
                true
            }
        });
        events
    })
}

fn history_shortcut(ctx: &egui::Context, enabled: bool) -> Option<Direction> {
    if !enabled {
        return None;
    }
    let mac = matches!(ctx.os(), egui::os::OperatingSystem::Mac);
    ctx.input_mut(|i| {
        let found = i.events.iter().enumerate().find_map(|(index, event)| {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                return None;
            };
            if !modifiers.command || modifiers.alt {
                return None;
            }
            let direction = match (*key, modifiers.shift) {
                (egui::Key::Z, false) => Direction::Undo,
                (egui::Key::Y, false) => Direction::Redo,
                (egui::Key::Z, true) if mac => Direction::Redo,
                _ => return None,
            };
            Some((index, direction))
        });
        found.map(|(index, direction)| {
            i.events.remove(index);
            direction
        })
    })
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

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    fn editor() -> App {
        let (runtime, _) = std::sync::mpsc::channel();
        App {
            state: Arc::new(std::sync::Mutex::new(runtime::State::new(
                Vec::new(),
                Default::default(),
            ))),
            runtime,
            renderer: Renderer::new(),
            textures: HashMap::new(),
            selected_device: None,
            offline_profile: None,
            selected_key: Some(0),
            search: String::new(),
            tab: ListTab::Keys,
            expanded: HashSet::new(),
            host: None,
            images: HashMap::new(),
            plugin_page_area: None,
            plugin_page_requested: None,
            plugin_page_placed: None,
            owner_set: false,
            recording_hotkey: false,
            clipboard: None,
            pending_paste: None,
            history: History::default(),
            history_shortcut: None,
            dirty: HashSet::new(),
            last_edit: Instant::now(),
            prefs: Preferences::new(),
            rename: None,
            folder_nav: None,
            split: DEFAULT_SPLIT,
            list_width: LIST_WIDTH,
        }
    }

    fn frame(
        ctx: &egui::Context,
        app: &mut App,
        profile: &mut Profile,
        events: Vec<egui::Event>,
        text_focus: bool,
    ) -> Vec<egui::epaint::ClippedShape> {
        let mut view = View {
            page: 0,
            path: Vec::new(),
            live: Live::default(),
        };
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(852.0, 742.0))),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let original = profile.clone();
            let original_location = view.location(app.selected_key);
            if text_focus {
                ui.text_edit_singleline(&mut app.search).request_focus();
            }
            app.canvas(ui, None, profile, &mut view);
            app.paste_confirmation(ctx, profile, &view);
            app.finish_edit(ctx, profile, &mut view, &original, original_location, None);
        });
        output.textures_delta.clear();
        output.shapes
    }

    fn shortcut(key: egui::Key, mac: bool, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                ctrl: !mac,
                mac_cmd: mac,
                shift,
                ..Default::default()
            },
        }
    }

    #[test]
    fn undo_redo_keyboard_restores_paste_and_delete_without_touching_the_source() {
        for mac in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_os(if mac {
                egui::os::OperatingSystem::Mac
            } else {
                egui::os::OperatingSystem::Windows
            });
            apply(&ctx);
            let mut app = editor();
            let mut profile = Profile::new("Test", DeviceKind::Mk2);
            let key = KeyConfig::new(actions::WEBSITE, json!({"url": "https://example.com"}));
            profile.pages[0].keys.insert(0, key.clone());
            let copy = KeyClipboard::copy(&profile, &profile.pages[0].id, 0).unwrap();
            app.selected_key = Some(1);
            frame(
                &ctx,
                &mut app,
                &mut profile,
                vec![egui::Event::Paste(copy.text().unwrap())],
                false,
            );
            frame(
                &ctx,
                &mut app,
                &mut profile,
                vec![shortcut(egui::Key::Z, mac, false)],
                false,
            );
            assert!(!profile.pages[0].keys.contains_key(&1));
            let redo = if mac {
                shortcut(egui::Key::Z, true, true)
            } else {
                shortcut(egui::Key::Y, false, false)
            };
            frame(&ctx, &mut app, &mut profile, vec![redo], false);
            assert_eq!(profile.pages[0].keys[&1], key);
            frame(
                &ctx,
                &mut app,
                &mut profile,
                vec![egui::Event::Key {
                    key: egui::Key::Delete,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                false,
            );
            assert!(!profile.pages[0].keys.contains_key(&1));
            frame(
                &ctx,
                &mut app,
                &mut profile,
                vec![shortcut(egui::Key::Z, mac, false)],
                false,
            );
            assert_eq!(profile.pages[0].keys[&1], key);
            assert_eq!(profile.pages[0].keys[&0], key);
        }
    }

    #[test]
    fn undo_is_blocked_in_text_fields_and_hotkey_recording_but_button_focus_is_allowed() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        let original = profile.clone();
        profile.pages[0]
            .keys
            .insert(0, KeyConfig::new(actions::WEBSITE, json!({})));
        app.history
            .record(
                &original,
                &profile,
                Location::default(),
                Location::default(),
                None,
            )
            .unwrap();
        let edited = profile.clone();
        frame(
            &ctx,
            &mut app,
            &mut profile,
            vec![shortcut(egui::Key::Z, false, false)],
            true,
        );
        assert_eq!(profile, edited);
        app.recording_hotkey = true;
        frame(
            &ctx,
            &mut app,
            &mut profile,
            vec![shortcut(egui::Key::Z, false, false)],
            false,
        );
        assert_eq!(profile, edited);
        app.recording_hotkey = false;
        let mut view = View {
            page: 0,
            path: Vec::new(),
            live: Live::default(),
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![shortcut(egui::Key::Z, false, false)],
                ..Default::default()
            },
            |ui| {
                ui.push_id("page-control", |ui| {
                    ui.button("Page control").request_focus()
                });
                app.canvas(ui, None, &mut profile, &mut view);
                let location = view.location(app.selected_key);
                app.finish_edit(&ctx, &mut profile, &mut view, &edited, location, None);
            },
        );
        output.textures_delta.clear();
        assert_eq!(profile, original);
    }

    #[test]
    fn keyboard_copy_pastes_empty_keys_and_cancel_preserves_occupied_keys() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        let key = KeyConfig::new(actions::WEBSITE, json!({"url": "https://example.com"}));
        profile.pages[0].keys.insert(0, key.clone());
        frame(&ctx, &mut app, &mut profile, vec![egui::Event::Copy], false);
        let text = app.clipboard.as_ref().unwrap().text().unwrap();
        app.selected_key = Some(1);
        frame(
            &ctx,
            &mut app,
            &mut profile,
            vec![egui::Event::Paste(text.clone())],
            false,
        );
        assert_eq!(profile.pages[0].keys[&1], key);
        assert!(app.pending_paste.is_none());
        app.dirty.clear();
        let original = profile.clone();
        frame(
            &ctx,
            &mut app,
            &mut profile,
            vec![egui::Event::Paste(text)],
            false,
        );
        assert!(app.pending_paste.is_some());
        assert_eq!(profile, original);
        assert!(app.dirty.is_empty());
        frame(
            &ctx,
            &mut app,
            &mut profile,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        assert!(app.pending_paste.is_none());
        assert_eq!(profile, original);
        assert!(app.dirty.is_empty());
    }

    #[test]
    fn text_focus_and_hotkey_recording_do_not_copy_or_paste_actions() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.pages[0]
            .keys
            .insert(0, KeyConfig::new(actions::WEBSITE, json!({})));
        frame(&ctx, &mut app, &mut profile, vec![egui::Event::Copy], true);
        assert!(app.clipboard.is_none());
        app.recording_hotkey = true;
        frame(&ctx, &mut app, &mut profile, vec![egui::Event::Copy], false);
        assert!(app.clipboard.is_none());
        app.recording_hotkey = false;
        let original = profile.clone();
        let copy = KeyClipboard::copy(&profile, &profile.pages[0].id, 0).unwrap();
        frame(
            &ctx,
            &mut app,
            &mut profile,
            vec![egui::Event::Paste(copy.text().unwrap())],
            true,
        );
        assert_eq!(profile, original);
        assert!(app.pending_paste.is_none());
    }

    #[test]
    fn confirmation_is_bound_to_the_original_profile_page_and_action() {
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        let key = KeyConfig::new(actions::WEBSITE, json!({}));
        profile.pages[0].keys.insert(0, key.clone());
        let page = profile.pages[0].id.clone();
        let pending = PendingPaste {
            profile: profile.id.clone(),
            page: page.clone(),
            index: 0,
            original: key,
            clipboard: KeyClipboard::copy(&profile, &page, 0).unwrap(),
        };
        assert!(pending.matches(&profile, &page));
        assert!(!pending.matches(&profile, "another-page"));
        let mut other = profile.clone();
        other.id = "another-profile".into();
        assert!(!pending.matches(&other, &page));
        profile.pages[0].keys.get_mut(&0).unwrap().title = "Changed by plugin".into();
        assert!(!pending.matches(&profile, &page));
    }

    #[test]
    fn clicking_replace_applies_only_the_confirmed_action() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        let key = KeyConfig::new(actions::WEBSITE, json!({"url": "https://example.com"}));
        profile.pages[0].keys.insert(0, key.clone());
        profile.pages[0]
            .keys
            .insert(1, KeyConfig::new(actions::HOTKEY, json!({})));
        let copy = KeyClipboard::copy(&profile, &profile.pages[0].id, 0).unwrap();
        app.selected_key = Some(1);
        frame(
            &ctx,
            &mut app,
            &mut profile,
            vec![egui::Event::Paste(copy.text().unwrap())],
            false,
        );
        let shapes = frame(&ctx, &mut app, &mut profile, Vec::new(), false);
        let pos = shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.job.text == "Replace"
                {
                    Some(text.pos + text.galley.size() / 2.0)
                } else {
                    None
                }
            })
            .unwrap();
        for pressed in [true, false] {
            frame(
                &ctx,
                &mut app,
                &mut profile,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                false,
            );
        }
        assert!(app.pending_paste.is_none());
        assert_eq!(profile.pages[0].keys[&1], key);
        assert_eq!(profile.pages[0].keys[&0], key);
        assert!(app.dirty.contains(&profile.id));
    }
}

#[cfg(test)]
mod plugin_page_tests {
    use super::*;

    #[test]
    fn placement_uses_client_coordinates_and_scales_panel_edges() {
        let inner = Rect::from_min_size(pos2(38.0, 37.0), vec2(960.0, 800.0));
        let area = Rect::from_min_max(pos2(146.0, 622.0), pos2(534.0, 750.0));
        assert_eq!(
            plugin_page_placement(area, inner, 1.0),
            Some([146, 622, 388, 128])
        );

        let inner = Rect::from_min_size(pos2(-1000.0, 80.0), vec2(960.0, 640.0));
        let area = Rect::from_min_max(pos2(10.5, 20.5), pos2(111.5, 121.5));
        assert_eq!(
            plugin_page_placement(area, inner, 1.25),
            Some([13, 26, 126, 126])
        );
        let oversized = Rect::from_min_max(pos2(-20.0, -20.0), pos2(1200.0, 1000.0));
        assert_eq!(
            plugin_page_placement(oversized, inner, 1.0),
            Some([0, 0, 960, 640])
        );
    }

    #[test]
    fn collapsed_or_offscreen_panels_are_hidden() {
        let inner = Rect::from_min_size(pos2(0.0, 0.0), vec2(960.0, 640.0));
        for area in [
            Rect::NOTHING,
            Rect::from_min_max(pos2(100.0, 200.0), pos2(100.0, 200.0)),
            Rect::from_min_max(pos2(10.0, 700.0), pos2(100.0, 800.0)),
        ] {
            assert_eq!(plugin_page_placement(area, inner, 1.0), None);
        }
    }
}

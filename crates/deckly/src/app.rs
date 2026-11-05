//! The editor window.
//!
//! Layout follows the official app (docs/clean-room/2026-10-25-main-window.md):
//! the canvas with the device header, key grid and page strip on the left,
//! the property inspector under it, and the action list on the right. The editor works on a copy
//! of the current profile each frame and writes it back when it changed.

use crate::actions::{self, ACTIONS, Category};
use crate::background::Background;
use crate::clipboard::{self, KeyClipboard};
use crate::history::{Direction, History, Location, shortcut as history_shortcut};
use crate::os::tray::Command;
use crate::os::{Hotkey, MediaKey};
use crate::plugins::{self, KeyImage, Overlay};
use crate::prefs::{self, Preferences};
use crate::render::{Backdrop, Live, Renderer};
use crate::runtime::{self, Msg, NOTICE_TIME, Shared};
use crate::store;
use crate::theme::*;
use crate::update::{self, Method};
use crate::wallpaper::WallpaperEditor;
use deckly_core::profile::{self, MAX_FOLDER_DEPTH, MAX_GROUP_ACTIONS};
use deckly_core::{ActionDelays, AutoExit, DeviceKind, KeyConfig, Page, Profile, TitleStyle};
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
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
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

    /// The page or folder on the canvas.
    fn shown<'a>(&self, profile: &'a Profile) -> Option<&'a Page> {
        match self.path.last() {
            Some(id) => profile.folder(id),
            None => profile.pages.get(self.page),
        }
    }

    fn keys<'a>(&self, profile: &'a mut Profile) -> Option<&'a mut BTreeMap<u8, KeyConfig>> {
        match self.path.last() {
            Some(id) => profile.folder_mut(id).map(|f| &mut f.keys),
            None => profile.pages.get_mut(self.page).map(|p| &mut p.keys),
        }
    }
}

/// The Multi Action or Key Logic key open in the canvas, in place of the key
/// grid. The key itself is `App::selected_key`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct GroupEdit {
    /// The side of a Multi Action Switch, the press of a Key Logic key.
    list: usize,
    /// The action selected in the list; `None` edits the key itself.
    member: Option<usize>,
}

#[derive(Clone, Debug)]
enum DragPayload {
    /// A built-in or plugin action, by UUID.
    Action(String),
    Key(u8),
    /// An action in the open Multi Action, by position.
    Member(usize),
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

struct FolderExitReset {
    profile: String,
    folder: String,
    dont_ask_again: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetDecision {
    Pending,
    Cancel,
    Reset,
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
    /// One texture per animated key and size, redrawn as its frames change,
    /// with the hash of the frame it holds.
    animated: HashMap<(u64, u32), (u64, TextureHandle)>,
    selected_device: Option<String>,
    offline_profile: Option<String>,
    selected_key: Option<u8>,
    /// Open while `selected_key` holds actions and is being edited.
    group: Option<GroupEdit>,
    /// The action of the open group key; the list shows what fits in it.
    group_holder: Option<String>,
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
    app_picker: Option<crate::app_picker::Picker>,
    image_editor: Option<crate::image_editor::Editor>,
    wallpaper_editor: Option<WallpaperEditor>,
    clipboard: Option<KeyClipboard>,
    pending_paste: Option<PendingPaste>,
    pending_folder_exit_reset: Option<FolderExitReset>,
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
    background: Rc<Background>,
    /// The tray asked for Preferences; they open with the next pass.
    prefs_after_show: bool,
    /// Closing the window keeps Deckly running. Off without a tray, and once
    /// the user chose to quit.
    close_to_tray: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, background: Rc<Background>) -> Self {
        apply(&cc.egui_ctx);
        let prefs = Preferences::new();
        prefs.offer_import(&cc.egui_ctx, &background.state);
        Self {
            state: background.state.clone(),
            runtime: background.runtime.clone(),
            renderer: Renderer::new(),
            textures: HashMap::new(),
            animated: HashMap::new(),
            selected_device: None,
            offline_profile: None,
            selected_key: None,
            group: None,
            group_holder: None,
            search: String::new(),
            tab: ListTab::Keys,
            expanded: HashSet::from([
                Category::MultiAction.label().to_owned(),
                Category::System.label().to_owned(),
            ]),
            host: background.host.clone(),
            images: HashMap::new(),
            plugin_page_area: None,
            plugin_page_requested: None,
            plugin_page_placed: None,
            owner_set: false,
            recording_hotkey: false,
            app_picker: None,
            image_editor: None,
            wallpaper_editor: None,
            clipboard: None,
            pending_paste: None,
            pending_folder_exit_reset: None,
            history: History::default(),
            history_shortcut: None,
            dirty: HashSet::new(),
            last_edit: Instant::now(),
            prefs,
            rename: None,
            folder_nav: None,
            split: DEFAULT_SPLIT,
            list_width: LIST_WIDTH,
            close_to_tray: background.attach(&cc.egui_ctx),
            background,
            prefs_after_show: false,
        }
    }

    /// Handles closing the window and what the tray and later starts ask.
    fn tick(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.prefs_after_show) {
            self.prefs.open(ctx, self.selected_device.as_deref());
        }
        if self.close_to_tray && ctx.input(|i| i.viewport().close_requested()) {
            if self.background.reopens_window() {
                self.background.set_closing_to_tray(true);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide(ctx);
            }
        }
        while let Some(command) = self.background.next_command() {
            match command {
                Command::Show => self.show(ctx),
                Command::Preferences => {
                    self.show(ctx);
                    // A hidden window's pass cannot create windows: egui
                    // panics. By the next pass this window is shown.
                    self.prefs_after_show = true;
                    ctx.request_repaint();
                }
                Command::Profile { serial, id } => {
                    self.selected_key = None;
                    self.background.switch_profile(&serial, id);
                }
                Command::Refresh => {}
                Command::Quit => {
                    self.close_to_tray = false;
                    self.background.set_closing_to_tray(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        self.background.refresh_tray();
    }

    /// Puts the window in the tray. Devices and plugins run on their own
    /// threads and carry on; what floats over the window goes with it.
    fn hide(&mut self, ctx: &egui::Context) {
        self.save_pending(true);
        self.prefs.close();
        self.prefs_after_show = false;
        if let Some(host) = &self.host {
            host.close_inspector();
        }
        self.plugin_page_requested = None;
        self.plugin_page_area = None;
        self.plugin_page_placed = None;
        self.background.hidden.store(true, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.background.hidden.store(false, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
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
        backdrop: Option<Backdrop<'_>>,
    ) -> egui::TextureId {
        let px = px.clamp(32, 288);
        let now = std::time::Instant::now();
        let (live, next) = self.renderer.animate(key, live, now);
        let hash = self.renderer.key_hash_on(key, live, overlay, backdrop);
        if let Some(next) = next {
            ctx.request_repaint_after(next.saturating_duration_since(now));
            let slot = self
                .renderer
                .key_hash_on(key, Live { frame: 0, ..live }, overlay, backdrop);
            if let Some((shown, tex)) = self.animated.get_mut(&(slot, px)) {
                if *shown != hash {
                    let img = self.renderer.render_on(key, px, live, overlay, backdrop);
                    let size = [px as usize, px as usize];
                    let color = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    tex.set(color, TextureOptions::LINEAR);
                    *shown = hash;
                }
                return tex.id();
            }
            if self.animated.len() > 64 {
                self.animated.clear();
            }
            let img = self.renderer.render_on(key, px, live, overlay, backdrop);
            let color =
                ColorImage::from_rgba_unmultiplied([px as usize, px as usize], img.as_raw());
            let tex = ctx.load_texture(
                format!("key-{slot:x}-{px}-animated"),
                color,
                TextureOptions::LINEAR,
            );
            let id = tex.id();
            self.animated.insert((slot, px), (hash, tex));
            return id;
        }
        if let Some(t) = self.textures.get(&(hash, px)) {
            return t.id();
        }
        if self.textures.len() > 512 {
            self.textures.clear();
        }
        let img = self.renderer.render_on(key, px, live, overlay, backdrop);
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
                s.assign(&d.serial, id);
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
                if log.is_file() {
                    open_path(&self.state, &log.to_string_lossy());
                } else {
                    let text = format!("{} has not written a log yet", p.name);
                    runtime::lock(&self.state).notify(text);
                }
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
        let holder = self.group_holder.clone();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for category in Category::ALL {
                    let items: Vec<&'static actions::ActionDef> = ACTIONS
                        .iter()
                        .filter(|a| a.category == category)
                        .filter(|a| actions::listed(a.uuid, holder.as_deref()))
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
                        .filter(|a| match holder.as_deref() {
                            Some(actions::KEY_LOGIC) => a.in_key_logic,
                            Some(_) => a.in_multi_actions,
                            None => true,
                        })
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
        let mut open_group = false;
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
        let wallpaper = view.shown(profile).and_then(|p| p.wallpaper.clone());
        self.wallpaper_button(ui, &geo, profile, view);
        let Some(keys) = view.keys(profile) else {
            return;
        };
        for index in 0..model.key_count() {
            let backdrop = wallpaper.as_ref().map(|wallpaper| Backdrop {
                wallpaper,
                model,
                index,
            });
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
                    let tex = self.texture(
                        ui.ctx(),
                        Some(key),
                        tex_px,
                        live,
                        overlays.get(&index),
                        backdrop,
                    );
                    painter.add(
                        egui::epaint::RectShape::filled(inner, inner_radius, Color32::WHITE)
                            .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
                    );
                }
                // An empty key shows its piece of the wallpaper.
                None if backdrop.is_some() => {
                    let tex = self.texture(ui.ctx(), None, tex_px, live, None, backdrop);
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
                    DragPayload::Action(uuid) => {
                        keys.insert(index, new_key(uuid));
                        // The official app opens these when they land on a key.
                        if profile::group_count(uuid) > 0 {
                            open_group = true;
                        }
                    }
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
                    DragPayload::Key(_) | DragPayload::Member(_) => {}
                }
                self.selected_key = Some(index);
                self.recording_hotkey = false;
                changed = true;
                if std::mem::take(&mut open_group) {
                    self.group = Some(GroupEdit {
                        list: 0,
                        member: None,
                    });
                }
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
                } else if !key.groups.is_empty() {
                    self.group = Some(GroupEdit {
                        list: 0,
                        member: None,
                    });
                }
            }
            response.context_menu(|ui| {
                // A blank key can become a Multi Action or Key Logic key.
                if config.is_none() {
                    for (uuid, text) in [
                        (actions::MULTI_ACTION, "Create Multi Action"),
                        (actions::KEY_LOGIC, "Create Key Logic Action"),
                    ] {
                        let glyph = actions::find(uuid).map_or(icon::STACK, |d| d.icon);
                        if ui.button(format!("{glyph}  {text}")).clicked() {
                            keys.insert(index, new_key(uuid));
                            self.selected_key = Some(index);
                            self.group = Some(GroupEdit {
                                list: 0,
                                member: None,
                            });
                            changed = true;
                            ui.close();
                        }
                    }
                    ui.separator();
                }
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

        drag_preview(ui.ctx());
    }

    /// The wallpaper button at the right end of the page strip's row. It
    /// opens the wallpaper of the page or folder on the canvas.
    fn wallpaper_button(
        &mut self,
        ui: &mut Ui,
        geo: &CanvasGeometry,
        profile: &Profile,
        view: &View,
    ) {
        let Some(page) = view.shown(profile) else {
            return;
        };
        let y = geo.grid.bottom() + CanvasGeometry::PAGER_GAP * geo.scale.min(1.0);
        let r = Rect::from_center_size(pos2(geo.grid.right() - 12.0, y), vec2(24.0, 20.0));
        let resp = ui
            .interact(r, ui.id().with("wallpaper"), Sense::click())
            .on_hover_text("Wallpaper across all keys");
        let open = self
            .wallpaper_editor
            .as_ref()
            .is_some_and(|w| w.page == page.id);
        let color = if open || page.wallpaper.is_some() || resp.hovered() {
            TEXT
        } else {
            TEXT_DIM
        };
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            icon::IMAGE_SQUARE,
            font(15.0),
            color,
        );
        if resp.clicked() {
            self.wallpaper_editor = (!open).then(|| WallpaperEditor::new(&profile.id, &page.id));
        }
    }

    /// The open Wallpaper window. Returns the undo group of its changes.
    fn wallpaper_window(&mut self, ctx: &egui::Context, profile: &mut Profile) -> Option<u64> {
        let editor = self.wallpaper_editor.as_mut()?;
        let model = profile.model();
        let page_id = editor.page.clone();
        let page = (editor.profile == profile.id)
            .then(|| {
                let at = profile.pages.iter().position(|p| p.id == page_id);
                match at {
                    Some(i) => profile.pages.get_mut(i),
                    None => profile.folder_mut(&page_id),
                }
            })
            .flatten();
        // The page went away or another profile is on the canvas.
        let Some(page) = page else {
            self.wallpaper_editor = None;
            return None;
        };
        let group = editor.show(ctx, page, model, &mut self.renderer);
        if group.is_none() {
            self.wallpaper_editor = None;
        }
        group
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
            let mut resp = ui.interact(r, ui.id().with(("page", i)), Sense::click());
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
            let shown_page = profile.pages.get(i);
            let picture = shown_page
                .and_then(|p| p.icon.as_deref())
                .and_then(|path| prefs::page_picture(ui.ctx(), path, 14.0));
            if let Some(picture) = picture {
                prefs::paint_page_picture(&painter, &picture, r.center(), 14.0, color);
            } else {
                painter.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    (i + 1).to_string(),
                    font(12.0),
                    color,
                );
            }
            if let Some(name) = shown_page
                .map(|p| p.name.as_str())
                .filter(|n| !n.is_empty())
            {
                resp = resp.on_hover_text(name);
            }
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
            if let Some(id) = profile.pages.get(i).map(|page| page.id.clone()) {
                profile.remove_page(&id);
            }
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
        let mut parent = profile.pages.get(view.page);
        let mut crumbs = vec![format!(
            "{}  {}",
            icon::ARROW_BEND_LEFT_UP,
            parent.map(|p| p.title(view.page)).unwrap_or_default()
        )];
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

    // ----- Multi Action and Key Logic editor -------------------------------

    /// Keeps the open group valid and tells the action list what fits in it:
    /// undo, the device or another window may have changed the key meanwhile.
    fn check_group(&mut self, profile: &mut Profile, view: &View) {
        let holder = self
            .selected_key
            .and_then(|k| view.keys(profile)?.get(&k))
            .filter(|k| !k.groups.is_empty());
        match (holder, &mut self.group) {
            (Some(key), Some(group)) => {
                group.list = group.list.min(key.groups.len() - 1);
                let size = key.groups.get(group.list).map_or(0, Vec::len);
                if key.action != actions::KEY_LOGIC {
                    group.member = group.member.filter(|m| *m < size);
                }
                self.group_holder = Some(key.action.clone());
            }
            _ => {
                self.group = None;
                self.group_holder = None;
            }
        }
    }

    /// Removes the selected action from its group, or the whole key when no
    /// action is selected.
    fn remove_edited(&mut self, profile: &mut Profile, view: &View) {
        let (Some(index), Some(group)) = (self.selected_key, self.group) else {
            return;
        };
        let Some(keys) = view.keys(profile) else {
            return;
        };
        match group.member {
            Some(member) => {
                if let Some(list) = keys
                    .get_mut(&index)
                    .and_then(|k| k.groups.get_mut(group.list))
                    && member < list.len()
                {
                    list.remove(member);
                }
            }
            None => {
                keys.remove(&index);
                self.group = None;
            }
        }
        if let Some(g) = &mut self.group {
            g.member = None;
        }
        self.mark_dirty(&profile.id);
    }

    /// What a Multi Action or Key Logic key holds, in place of the key grid.
    fn group_canvas(&mut self, ui: &mut Ui, profile: &mut Profile, view: &View) {
        let (Some(index), Some(mut group)) = (self.selected_key, self.group) else {
            return;
        };
        let page_id = view
            .path
            .last()
            .cloned()
            .or_else(|| profile.pages.get(view.page).map(|p| p.id.clone()))
            .unwrap_or_default();
        let live = view.live;
        let area = ui.max_rect();
        let mut remove = false;
        let device = profile.device;
        let Some(key) = view.keys(profile).and_then(|k| k.get_mut(&index)) else {
            return;
        };
        let before = key.clone();
        let name = actions::find(&key.action).map_or("Multi Action", |d| d.name);

        // Back arrow and title.
        let painter = ui.painter().clone();
        let title_y = area.top() + 48.0;
        let galley = painter.layout_no_wrap(name.to_owned(), bold(17.0), TEXT);
        let back = Rect::from_min_size(
            pos2(area.left() + 52.0, title_y - 14.0),
            vec2(galley.size().x + 32.0, 28.0),
        );
        let resp = ui
            .interact(back, ui.id().with("group-back"), Sense::click())
            .on_hover_text("Back to the keys");
        let color = if resp.hovered() { Color32::WHITE } else { TEXT };
        painter.text(
            pos2(back.left() + 12.0, title_y),
            Align2::CENTER_CENTER,
            icon::ARROW_ELBOW_LEFT_UP,
            font(16.0),
            color,
        );
        painter.galley_with_override_text_color(
            pos2(back.left() + 26.0, title_y - galley.size().y / 2.0),
            galley,
            color,
        );
        let close = resp.clicked();

        // The two sides of a Multi Action Switch.
        if key.groups.len() == 2 {
            for side in 0..2 {
                let r = Rect::from_min_size(
                    pos2(back.right() + 8.0 + side as f32 * 30.0, title_y - 11.0),
                    vec2(30.0, 22.0),
                );
                let resp = ui.interact(r, ui.id().with(("side", side)), Sense::click());
                let on = group.list == side;
                let corners = if side == 0 {
                    CornerRadius {
                        nw: 3,
                        sw: 3,
                        ..CornerRadius::ZERO
                    }
                } else {
                    CornerRadius {
                        ne: 3,
                        se: 3,
                        ..CornerRadius::ZERO
                    }
                };
                painter.rect_filled(
                    r,
                    corners,
                    if on { PREF_ROW_SELECTED } else { PREF_SEGMENT },
                );
                painter.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    (side + 1).to_string(),
                    font(13.0),
                    if on { Color32::WHITE } else { TEXT },
                );
                if resp.clicked() && !on {
                    group = GroupEdit {
                        list: side,
                        member: None,
                    };
                }
            }
        }

        let body = Rect::from_min_max(
            pos2(area.left() + 56.0, area.top() + 95.0),
            pos2(area.right() - 36.0, area.bottom() - 8.0),
        );
        if key.action == actions::KEY_LOGIC {
            self.key_logic_slots(ui, area, key, &mut group, &page_id, index, live);
        } else if body.is_positive() {
            remove = self.member_rows(ui, body, key, &mut group);
        }

        let shortcuts = self.pending_paste.is_none()
            && !egui::Popup::is_any_open(ui.ctx())
            && ui.ctx().memory(|m| m.focused().is_none());
        if shortcuts && group.member.is_some() && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            remove = true;
        }
        for event in clipboard_events(ui.ctx(), shortcuts) {
            match event {
                egui::Event::Copy => {
                    if let Some(member) = group
                        .member
                        .and_then(|m| key.groups.get(group.list)?.get(m))
                    {
                        let copy = KeyClipboard::of_key(device, member);
                        match copy.text() {
                            Ok(text) => {
                                ui.ctx().copy_text(text);
                                self.clipboard = Some(copy);
                            }
                            Err(e) => runtime::lock(&self.state).notify(e),
                        }
                    }
                }
                egui::Event::Paste(text) => match KeyClipboard::from_text(&text) {
                    Ok(Some(copy)) => paste_member(key, &mut group, &copy),
                    Ok(None) => {}
                    Err(e) => runtime::lock(&self.state).notify(e),
                },
                _ => {}
            }
        }

        if let Some(direction) = history_shortcut(
            ui.ctx(),
            self.pending_paste.is_none() && !ui.ctx().text_edit_focused(),
        ) {
            self.history_shortcut = Some(direction);
        }
        drag_preview(ui.ctx());
        let changed = *key != before;
        self.group = (!close).then_some(group);
        if changed {
            self.mark_dirty(&profile.id);
        }
        if remove && group.member.is_some() {
            self.remove_edited(profile, view);
        }
    }

    /// The rows of a Multi Action, in running order. Actions are dropped
    /// between rows and rows are dragged by themselves. Returns whether the
    /// selected row should be removed.
    fn member_rows(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        key: &mut KeyConfig,
        group: &mut GroupEdit,
    ) -> bool {
        const ROW: f32 = 40.0;
        let mut remove = false;
        let Some(list) = key.groups.get(group.list) else {
            return false;
        };
        let names: Vec<(String, String, Option<egui::TextureId>, &'static str)> = list
            .iter()
            .map(|m| self.member_label(ui.ctx(), m))
            .collect();
        let count = names.len();
        let painter = ui.painter().clone();
        let mut target = None;
        let dragging = egui::DragAndDrop::has_any_payload(ui.ctx());
        let pointer = ui.ctx().pointer_interact_pos();
        let mut scroll_ui = ui.new_child(egui::UiBuilder::new().max_rect(body));
        let scrolled = egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(&mut scroll_ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for (i, (category, name, tex, glyph)) in names.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), ROW),
                        Sense::click_and_drag(),
                    );
                    resp.dnd_set_drag_payload(DragPayload::Member(i));
                    let on = group.member == Some(i);
                    let fill = if on {
                        PREF_ROW_SELECTED
                    } else if resp.hovered() {
                        LIST_HOVER
                    } else {
                        MEMBER_ROW
                    };
                    let p = ui.painter();
                    p.rect_filled(rect, CornerRadius::ZERO, fill);
                    let tint = if on { Color32::WHITE } else { ITEM_ICON };
                    let icon_rect = Rect::from_center_size(
                        pos2(rect.left() + 21.0, rect.center().y),
                        Vec2::splat(22.0),
                    );
                    match tex {
                        Some(_) => draw_list_icon(p, icon_rect, *tex, tint),
                        None => {
                            p.text(
                                icon_rect.center(),
                                Align2::CENTER_CENTER,
                                *glyph,
                                font(18.0),
                                tint,
                            );
                        }
                    }
                    p.text(
                        pos2(rect.left() + 45.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        format!("{category}: {name}"),
                        font(13.0),
                        if on { Color32::WHITE } else { TEXT },
                    );
                    p.text(
                        pos2(rect.right() - 29.0, rect.center().y),
                        Align2::CENTER_CENTER,
                        icon::LIST,
                        font(15.0),
                        if on { Color32::WHITE } else { TEXT_DIM },
                    );
                    if resp.clicked() {
                        group.member = Some(i);
                        self.recording_hotkey = false;
                    }
                    resp.context_menu(|ui| {
                        if ui.button(format!("{}  Remove", icon::TRASH)).clicked() {
                            group.member = Some(i);
                            remove = true;
                            ui.close();
                        }
                    });
                    if dragging
                        && let Some(pos) = pointer
                        && rect.contains(pos)
                    {
                        let below = pos.y > rect.center().y;
                        target = Some(i + usize::from(below));
                    }
                }
            });
        // Below the last row means at the end.
        if dragging && target.is_none() && pointer.is_some_and(|pos| body.contains(pos)) {
            target = Some(count);
        }
        if let Some(at) = target {
            let y = (body.top() + at as f32 * ROW - scrolled.state.offset.y)
                .clamp(body.top(), body.bottom());
            painter.hline(body.x_range(), y, Stroke::new(2.0, KEY_DROP));
        }
        if ui.input(|i| i.pointer.any_released())
            && let Some(at) = target
            && let Some(payload) = egui::DragAndDrop::take_payload::<DragPayload>(ui.ctx())
        {
            match &*payload {
                DragPayload::Action(uuid) if key.can_hold(uuid) => {
                    if let Some(list) = key.groups.get_mut(group.list)
                        && list.len() < MAX_GROUP_ACTIONS
                    {
                        let at = at.min(list.len());
                        list.insert(at, new_key(uuid));
                        group.member = Some(at);
                    }
                }
                &DragPayload::Member(from) => {
                    if let Some(list) = key.groups.get_mut(group.list)
                        && from < list.len()
                    {
                        let member = list.remove(from);
                        let to = if at > from { at - 1 } else { at }.min(list.len());
                        list.insert(to, member);
                        group.member = Some(to);
                    }
                }
                _ => {}
            }
        }
        remove
    }

    /// The three presses of a Key Logic key, each a slot for one action.
    #[allow(clippy::too_many_arguments)]
    fn key_logic_slots(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        key: &mut KeyConfig,
        group: &mut GroupEdit,
        page_id: &str,
        index: u8,
        live: Live,
    ) {
        const SLOT: f32 = 78.0;
        const PITCH: f32 = 98.0;
        const LABELS: [&[&str]; 3] = [&["Single", "Press"], &["Double", "Press"], &["Hold"]];
        let center = pos2(area.center().x, area.top() + area.height() * 0.42);
        let px = (SLOT * ui.ctx().pixels_per_point()) as u32;
        let painter = ui.painter().clone();
        for (slot, lines) in LABELS.into_iter().enumerate() {
            let rect = Rect::from_center_size(
                center + vec2((slot as f32 - 1.0) * PITCH, 0.0),
                Vec2::splat(SLOT),
            );
            let resp = ui.interact(rect, ui.id().with(("slot", slot)), Sense::click());
            let on = group.list == slot && group.member == Some(0);
            let drop = resp.dnd_hover_payload::<DragPayload>().is_some();
            let border = if drop {
                KEY_DROP
            } else if on {
                KEY_SELECTED
            } else if resp.hovered() {
                TEXT_DIM
            } else {
                KEY_BORDER
            };
            painter.rect_filled(rect, CornerRadius::same(12), border);
            let inner = rect.shrink(3.0);
            match key.groups.get(slot).and_then(|l| l.first()) {
                Some(member) => {
                    let context = plugins::member_context(page_id, index, slot, 0);
                    let overlay = runtime::lock(&self.state).overlay(&context, member);
                    let tex =
                        self.texture(ui.ctx(), Some(member), px, live, overlay.as_ref(), None);
                    painter.add(
                        egui::epaint::RectShape::filled(
                            inner,
                            CornerRadius::same(9),
                            Color32::WHITE,
                        )
                        .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
                    );
                }
                None => {
                    painter.rect_filled(inner, CornerRadius::same(9), KEY_EMPTY);
                }
            }
            for (i, line) in lines.iter().enumerate() {
                painter.text(
                    pos2(rect.center().x, rect.bottom() + 12.0 + i as f32 * 16.0),
                    Align2::CENTER_TOP,
                    line,
                    font(12.0),
                    TEXT_DIM,
                );
            }
            if resp.clicked() {
                *group = GroupEdit {
                    list: slot,
                    member: Some(0),
                };
                self.recording_hotkey = false;
            }
            if let Some(payload) = resp.dnd_release_payload::<DragPayload>()
                && let DragPayload::Action(uuid) = &*payload
                && key.can_hold(uuid)
                && let Some(list) = key.groups.get_mut(slot)
            {
                *list = vec![new_key(uuid)];
                *group = GroupEdit {
                    list: slot,
                    member: Some(0),
                };
            }
            resp.context_menu(|ui| {
                let full = key.groups.get(slot).is_some_and(|l| !l.is_empty());
                if ui
                    .add_enabled(full, egui::Button::new(format!("{}  Clear", icon::TRASH)))
                    .clicked()
                {
                    if let Some(list) = key.groups.get_mut(slot) {
                        list.clear();
                    }
                    ui.close();
                }
            });
        }
    }

    /// How a row names an action, with its plugin icon or built-in glyph.
    fn member_label(
        &mut self,
        ctx: &egui::Context,
        member: &KeyConfig,
    ) -> (String, String, Option<egui::TextureId>, &'static str) {
        if let Some(def) = actions::find(&member.action) {
            return (
                def.category.label().to_owned(),
                def.name.to_owned(),
                None,
                actions::key_icon(member),
            );
        }
        let plugin = {
            let s = runtime::lock(&self.state);
            s.catalog
                .action(&member.action)
                .map(|(p, a)| (p.category.clone(), a.name.clone(), a.icon.clone()))
        };
        match plugin {
            Some((category, name, image)) => (
                category,
                name,
                image.map(|img| self.image_texture(ctx, &img)),
                icon::PUZZLE_PIECE,
            ),
            None => (
                "Plugin not installed".to_owned(),
                member.action.clone(),
                None,
                icon::PUZZLE_PIECE,
            ),
        }
    }

    // ----- property inspector --------------------------------------------

    fn inspector(&mut self, ui: &mut Ui, profile: &mut Profile, view: &View) {
        let panel = ui.max_rect();
        let profile_id = profile.id.clone();
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
        let pages: Vec<String> = profile
            .pages
            .iter()
            .enumerate()
            .map(|(i, page)| page.label(i))
            .collect();
        let Some(index) = self.selected_key else {
            self.app_picker = None;
            self.image_editor = None;
            hint(ui, "Select a key to edit its action.");
            return;
        };
        let group = self.group;
        let context = edited_context(&page_id, index, group);
        let top_level = group.is_none_or(|group| group.member.is_none());
        let image_context = format!("{profile_id}/{context}");
        if self
            .app_picker
            .as_ref()
            .is_some_and(|p| p.context != context)
        {
            self.app_picker = None;
        }
        let in_sequence = group.is_some_and(|g| g.member.is_some())
            && self.group_holder.as_deref() != Some(actions::KEY_LOGIC);
        let folder_exit = view.path.last().and_then(|id| {
            profile.folder(id)?.keys.values().find_map(|key| {
                (key.action == actions::PARENT_FOLDER).then(|| key.auto_exit.unwrap_or_default())
            })
        });
        let model = profile.model();
        let wallpaper = view
            .shown(profile)
            .and_then(|p| p.wallpaper.clone())
            .filter(|_| top_level);
        let Some(key) = edited_key(profile, view, index, group) else {
            self.app_picker = None;
            self.image_editor = None;
            let text = if group.is_some() {
                "Drag an action from the list onto this slot."
            } else {
                "Drag an action from the list onto this key."
            };
            hint(ui, text);
            return;
        };
        if self
            .image_editor
            .as_ref()
            .is_some_and(|editor| !editor.matches(&image_context, key))
        {
            self.image_editor = None;
        }
        let before = key.clone();
        if key.action != actions::OPEN {
            self.app_picker = None;
        }
        let defaults = self.renderer.title_defaults.clone();
        let def = actions::find(&key.action);
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
                states: a.states.len(),
                instance: s.instance(&context, key),
            });
            (s.overlay(&context, key), plugin)
        };
        let mut remove = false;
        let mut reset_folder_exit = false;
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
            if group.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);
                    remove = icon_button(ui, icon::TRASH, "Remove").clicked();
                    if in_sequence && key.action != actions::DELAY {
                        ui.add_space(4.0);
                        delays_button(ui, &mut key.delays);
                    }
                });
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
                let backdrop = wallpaper.as_ref().map(|wallpaper| Backdrop {
                    wallpaper,
                    model,
                    index,
                });
                let tex = self.texture(
                    ui.ctx(),
                    Some(key),
                    (96.0 * ppp) as u32,
                    view.live,
                    overlay.as_ref(),
                    backdrop,
                );
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
                ui.painter().add(
                    egui::epaint::RectShape::filled(rect, CornerRadius::same(12), Color32::WHITE)
                        .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
                );
                ui.add_space(6.0);
                ui.menu_button(format!("{}  Image…", icon::IMAGE), |ui| {
                    use crate::image_editor::{self, Editor, ImageChoice, Mode};
                    if let Some(choice) = image_editor::image_menu(ui, key.image.is_some()) {
                        if choice == ImageChoice::Reset {
                            key.image = None;
                        } else {
                            let mode = if choice == ImageChoice::Library {
                                Mode::Library
                            } else {
                                Mode::Create
                            };
                            let mut editor = Editor::new(image_context.clone(), key, mode);
                            if choice != ImageChoice::Upload || editor.upload(ui.ctx()) {
                                self.image_editor = Some(editor);
                            }
                        }
                    }
                });
                if ui
                    .add_enabled(
                        key.image.is_some(),
                        egui::Button::new(format!("{}  Reset", icon::ARROW_COUNTER_CLOCKWISE)),
                    )
                    .on_hover_text("Restore the action's default image")
                    .clicked()
                {
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
                                let linked = overlay.as_ref().and_then(|o| o.title.as_deref());
                                ui.add(
                                    egui::TextEdit::multiline(&mut key.title)
                                        .hint_text(
                                            RichText::new(linked.unwrap_or("")).color(TEXT_FAINT),
                                        )
                                        .desired_rows(1)
                                        .desired_width(
                                            if top_level
                                                && key.action != actions::PARENT_FOLDER
                                                && folder_exit.is_some()
                                            {
                                                228.0
                                            } else {
                                                260.0
                                            },
                                        ),
                                );
                                title_style_button(ui, &mut key.title_style, &defaults);
                                if top_level
                                    && key.action != actions::PARENT_FOLDER
                                    && let Some(folder_exit) = folder_exit
                                {
                                    folder_exit_button(ui, &mut key.auto_exit, folder_exit);
                                }
                            });
                            ui.end_row();
                            if key.action == actions::PARENT_FOLDER {
                                reset_folder_exit = parent_auto_exit_rows(ui, &mut key.auto_exit);
                            }
                            match &plugin {
                                Some(p) => {
                                    self.plugin_form(ui, p);
                                    if in_sequence {
                                        desired_state_row(ui, key, p.states);
                                    }
                                }
                                None => self.action_form(ui, key, &profiles, &pages, &context),
                            }
                        });
                })
                .response
                .rect;
        });
        if let Some(editor) = &mut self.image_editor {
            use crate::image_editor::Selection;
            match editor.show(ui.ctx(), &profile_id) {
                Selection::Image(path) => {
                    key.image = Some(path);
                    self.image_editor = None;
                }
                Selection::Cancelled => self.image_editor = None,
                Selection::Pending => {}
            }
        }
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
        let mut changed = *key != before;
        if reset_folder_exit && let Some(folder) = view.path.last() {
            changed |= self.request_folder_exit_reset(ui.ctx(), profile, folder);
        }
        if remove {
            self.remove_edited(profile, view);
        } else if changed {
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
        pages: &[String],
        context: &str,
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
                    // The desktop keeps this key to itself, so recording
                    // cannot see it: egui reports it only on macOS.
                    let mut meta = actions::bool_setting(s, "meta");
                    if ui.checkbox(&mut meta, actions::meta_key_name()).changed() {
                        actions::set_setting(s, "meta", json!(meta));
                    }
                });
                ui.end_row();
            }
            actions::OPEN => {
                label(ui, "App / File");
                let mut selected_app = None;
                ui.vertical(|ui| {
                    let name = actions::str_setting(s, "application_name");
                    if !name.is_empty() {
                        ui.label(RichText::new(name).strong());
                    }
                    let mut path = actions::str_setting(s, "path").to_owned();
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut path)
                                .hint_text("Application, file or command")
                                .desired_width(320.0),
                        )
                        .changed()
                    {
                        actions::set_setting(s, "path", json!(path));
                        actions::set_setting(s, "application_name", json!(""));
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button(format!("{}  Applications…", icon::APP_WINDOW))
                            .clicked()
                        {
                            self.app_picker =
                                Some(crate::app_picker::Picker::new(ui.ctx(), context.to_owned()));
                        }
                        if ui.button("File…").clicked()
                            && let Some(p) = rfd::FileDialog::new()
                                .set_title("Choose a file or program")
                                .pick_file()
                        {
                            actions::set_setting(s, "path", json!(p.to_string_lossy()));
                            actions::set_setting(s, "application_name", json!(""));
                        }
                        if ui.button("Folder…").clicked()
                            && let Some(p) = rfd::FileDialog::new()
                                .set_title("Choose a folder")
                                .pick_folder()
                        {
                            actions::set_setting(s, "path", json!(p.to_string_lossy()));
                            actions::set_setting(s, "application_name", json!(""));
                        }
                    });
                    if let Some(picker) = &mut self.app_picker {
                        match picker.show(ui.ctx()) {
                            crate::app_picker::Selection::Pending => {}
                            crate::app_picker::Selection::Cancelled => self.app_picker = None,
                            crate::app_picker::Selection::Application(app) => {
                                selected_app = Some(app);
                                self.app_picker = None;
                            }
                        }
                    }
                });
                if let Some(app) = selected_app {
                    crate::app_picker::apply(key, &app);
                }
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
                let current = actions::int_setting(s, "page", 1);
                let name = usize::try_from(current - 1)
                    .ok()
                    .and_then(|i| pages.get(i))
                    .map_or("Choose a page", String::as_str);
                egui::ComboBox::from_id_salt("go-to-page")
                    .selected_text(name)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (number, page) in (1i64..).zip(pages) {
                            if ui.selectable_label(number == current, page).clicked() {
                                actions::set_setting(s, "page", json!(number));
                            }
                        }
                    });
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
            actions::DELAY => {
                label(ui, "Delay");
                let mut ms = actions::int_setting(s, "delay", 1000).clamp(1, actions::MAX_DELAY_MS);
                if ui
                    .add(
                        egui::DragValue::new(&mut ms)
                            .range(1..=actions::MAX_DELAY_MS)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    actions::set_setting(s, "delay", json!(ms));
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

    fn request_folder_exit_reset(
        &mut self,
        ctx: &egui::Context,
        profile: &mut Profile,
        folder: &str,
    ) -> bool {
        if runtime::lock(&self.state)
            .settings
            .confirm_folder_auto_exit_reset
        {
            self.pending_folder_exit_reset = Some(FolderExitReset {
                profile: profile.id.clone(),
                folder: folder.to_owned(),
                dont_ask_again: false,
            });
            ctx.request_repaint();
            false
        } else {
            self.reset_folder_exit(profile, folder)
        }
    }

    fn reset_folder_exit(&mut self, profile: &mut Profile, folder: &str) -> bool {
        let changed = reset_folder_auto_exit(profile, folder);
        runtime::lock(&self.state).notify(if changed {
            "All actions now use the folder's Auto Exit settings."
        } else {
            "All actions already use the folder's Auto Exit settings."
        });
        changed
    }

    fn folder_exit_confirmation(
        &mut self,
        ctx: &egui::Context,
        profile: &mut Profile,
        view: &View,
    ) {
        let Some(mut pending) = self.pending_folder_exit_reset.take() else {
            return;
        };
        if pending.profile != profile.id
            || view.path.last() != Some(&pending.folder)
            || profile.folder(&pending.folder).is_none()
        {
            runtime::lock(&self.state).notify("Auto Exit reset cancelled: the folder changed");
            return;
        }
        let overrides = folder_exit_overrides(profile, &pending.folder);
        match folder_exit_reset_modal(ctx, &mut pending.dont_ask_again, overrides) {
            ResetDecision::Reset => {
                if pending.dont_ask_again {
                    let mut state = runtime::lock(&self.state);
                    state.settings.confirm_folder_auto_exit_reset = false;
                    state.save_settings();
                }
                if self.reset_folder_exit(profile, &pending.folder) {
                    self.mark_dirty(&profile.id);
                }
            }
            ResetDecision::Pending => self.pending_folder_exit_reset = Some(pending),
            ResetDecision::Cancel => {}
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
            host: self.host.as_ref(),
            updater: &self.background.updater,
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

    /// Offers a newer release at the bottom of the window, and finishes an
    /// update the user started: Deckly starts the new version and quits.
    fn update_banner(&mut self, ctx: &egui::Context) {
        let updater = self.background.updater.clone();
        if let Some((release, finish)) = updater.take_ready() {
            match finish.start() {
                Ok(()) => self.background.send(Command::Quit),
                Err(e) => updater.fail(release, e),
            }
            return;
        }
        let status = updater.status();
        let skipped = runtime::lock(&self.state).settings.skipped_update.clone();
        let release = match &status {
            update::Status::Available(r) if r.version != skipped && !updater.dismissed() => r,
            update::Status::Downloading(r) => r,
            update::Status::InstallFailed(r, _) => r,
            _ => return,
        };
        let method = updater.method();
        let width = (ctx.content_rect().width() - 64.0).clamp(240.0, 640.0);
        let mut skip = false;
        egui::Area::new(egui::Id::new("update-banner"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -24.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(POPUP)
                    .stroke(Stroke::new(1.0, ACCENT))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| match &status {
                            update::Status::Downloading(_) => {
                                ui.add(egui::Spinner::new().color(ACCENT));
                                ui.label(format!("Downloading Deckly {}…", release.version));
                            }
                            update::Status::InstallFailed(_, error) => {
                                ui.label(RichText::new(icon::WARNING).color(WARNING));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "Could not update to {}: {error}",
                                            release.version
                                        ))
                                        .color(WARNING),
                                    )
                                    .wrap(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button(icon::X).on_hover_text("Close").clicked() {
                                            updater.dismiss();
                                        }
                                        if ui.button("Try Again").clicked() {
                                            updater.install(release.clone());
                                        }
                                    },
                                );
                            }
                            _ => {
                                ui.label(RichText::new(icon::ARROW_CIRCLE_UP).color(ACCENT));
                                let text = format!(
                                    "Deckly {} is available. You have {}.",
                                    release.version,
                                    env!("CARGO_PKG_VERSION")
                                );
                                ui.add(egui::Label::new(text).wrap());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .button(icon::X)
                                            .on_hover_text("Remind me later")
                                            .clicked()
                                        {
                                            updater.dismiss();
                                        }
                                        if ui.button("Skip This Version").clicked() {
                                            skip = true;
                                        }
                                        if ui.button("What's New").clicked()
                                            && let Err(e) = crate::os::open(&release.page())
                                        {
                                            runtime::lock(&self.state).notify(e);
                                        }
                                        match method {
                                            Method::Manual(hint) => {
                                                ui.label(RichText::new(icon::INFO).color(TEXT_DIM))
                                                    .on_hover_text(hint);
                                            }
                                            _ => {
                                                let update = egui::Button::new(
                                                    RichText::new("Update Now")
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(ACCENT);
                                                if ui
                                                    .add(update)
                                                    .on_hover_text(
                                                        "Download, install and restart Deckly. \
                                                     Your profiles stay as they are.",
                                                    )
                                                    .clicked()
                                                {
                                                    updater.install(release.clone());
                                                }
                                            }
                                        }
                                    },
                                );
                            }
                        });
                    });
            });
        if skip {
            let mut s = runtime::lock(&self.state);
            s.settings.skipped_update = release.version.clone();
            s.save_settings();
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
    /// Unlike `ui`, this also runs while the window is minimized.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        // egui treats only minimized and covered windows as not visible, so
        // this runs for a hidden one too.
        if self.background.hidden.load(Ordering::Relaxed) {
            return;
        }
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
            (s.settings.title_style.clone(), s.settings.key_press)
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
        self.check_group(&mut profile, &view);
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
                    egui::Id::new((
                        &profile.id,
                        view.page,
                        &view.path,
                        self.selected_key,
                        self.group,
                        focus,
                    ))
                    .value(),
                    profile.clone(),
                )
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), CornerRadius::ZERO, CANVAS);
            if self.group.is_some() {
                self.group_canvas(ui, &mut profile, &view);
            } else {
                self.canvas(ui, device.as_ref(), &mut profile, &mut view);
            }
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
            self.group = None;
            self.recording_hotkey = false;
        }

        self.paste_confirmation(&ctx, &mut profile, &view);
        self.folder_exit_confirmation(&ctx, &mut profile, &view);
        let wallpaper_edit = self.wallpaper_window(&ctx, &mut profile);

        // Write the edited copy back. Profile switches made this frame
        // already changed which profile is current; only write if it still
        // exists.
        let group = inspector_edit
            .filter(|(_, edited)| edited == &profile)
            .map(|(id, _)| id)
            .or(wallpaper_edit);
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
        self.update_banner(&ctx);
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
                let key = edited_key(&mut profile, &view, k, self.group)?;
                Some((edited_context(&page_id, k, self.group), key.action.clone()))
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
        self.background.detach();
        if !self.background.closing_to_tray() {
            self.background.shutdown();
        } else if let Some(host) = &self.host {
            host.close_inspector();
        }
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
    states: usize,
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

/// The "T" button beside the title field. It opens the title's show,
/// position, size and colour, like the official app's. Editing makes the key
/// keep its own style; the reset button makes it follow the global style
/// again.
fn title_style_button(ui: &mut Ui, style: &mut Option<TitleStyle>, defaults: &TitleStyle) {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 24.0), Sense::click());
    let resp = resp.on_hover_text("Title style");
    // egui remembers one open popup at a time, and the font menu and the
    // colour picker inside are popups too, so this one keeps its own state.
    let open_id = resp.id.with("open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    if resp.clicked() {
        open = !open;
    }
    let lit = open || resp.hovered();
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

    let mut s = style.clone().unwrap_or_else(|| defaults.clone());
    let mut reset = false;
    let popup = egui::Popup::from_response(&resp)
        .open_bool(&mut open)
        // Clicks in the font menu and the colour picker must not close it.
        .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(4.0)
        .frame(egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(12)));
    let shown = popup.show(|ui| {
        let size = prefs::TITLE_STYLE_SIZE + vec2(38.0, 0.0);
        let (area, _) = ui.allocate_exact_size(size, Sense::hover());
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
        open = false;
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));

    if reset {
        *style = None;
    } else if s != *style.as_ref().unwrap_or(defaults) {
        *style = Some(s);
    }
}

/// The folder's Exit key controls the default for every action in it.
fn parent_auto_exit_rows(ui: &mut Ui, setting: &mut Option<AutoExit>) -> bool {
    let mut value = setting.unwrap_or_default();
    let before = value;
    let mut reset = false;
    ui.label(RichText::new("Auto Exit").color(TEXT_DIM));
    ui.horizontal(|ui| {
        auto_exit_toggle(ui, &mut value.enabled)
            .on_hover_text("Return to the parent folder after executing an action in this folder");
        reset = icon_button(
            ui,
            icon::ARROW_COUNTER_CLOCKWISE,
            "Reset all actions to folder Auto Exit settings",
        )
        .clicked();
    });
    ui.end_row();
    auto_exit_delay_row(ui, &mut value);
    if value != before {
        *setting = Some(value);
    }
    reset
}

fn folder_exit_overrides(profile: &Profile, folder: &str) -> usize {
    profile.folder(folder).map_or(0, |folder| {
        folder
            .keys
            .values()
            .filter(|key| key.action != actions::PARENT_FOLDER && key.auto_exit.is_some())
            .count()
    })
}

fn folder_exit_reset_modal(
    ctx: &egui::Context,
    dont_ask_again: &mut bool,
    overrides: usize,
) -> ResetDecision {
    let mut decision = ResetDecision::Pending;
    let modal = egui::Modal::new(egui::Id::new("reset-folder-auto-exit")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading("Reset Auto Exit settings?");
        ui.add_space(8.0);
        if overrides == 0 {
            ui.label("All actions in this folder already follow its Auto Exit settings.");
        } else {
            ui.label("This replaces the individual Auto Exit settings of actions in this folder with the folder's settings.");
        }
        ui.add_space(8.0);
        ui.checkbox(dont_ask_again, "Don't ask me again");
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                decision = ResetDecision::Cancel;
            }
            if ui.button("Reset").clicked() {
                decision = ResetDecision::Reset;
            }
        });
    });
    if decision == ResetDecision::Pending && modal.should_close() {
        ResetDecision::Cancel
    } else {
        decision
    }
}

fn reset_folder_auto_exit(profile: &mut Profile, folder: &str) -> bool {
    let Some(folder) = profile.folder_mut(folder) else {
        return false;
    };
    let mut changed = false;
    for key in folder
        .keys
        .values_mut()
        .filter(|key| key.action != actions::PARENT_FOLDER)
    {
        changed |= key.auto_exit.take().is_some();
    }
    changed
}

/// An individual action can follow its folder, opt out, or use its own delay.
fn folder_exit_button(ui: &mut Ui, setting: &mut Option<AutoExit>, inherited: AutoExit) {
    let response = icon_button(ui, icon::ARROW_BEND_LEFT_UP, "Folder Exit");
    egui::Popup::from_toggle_button_response(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(4.0)
        .frame(egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(14)))
        .show(|ui| {
            ui.set_width(240.0);
            ui.label(RichText::new("Folder Exit").font(bold(13.0)));
            ui.separator();
            let mut follow = setting.is_none();
            if ui.checkbox(&mut follow, "Follow folder settings").changed() {
                *setting = (!follow).then_some(inherited);
            }
            ui.add_space(4.0);
            let mut value = setting.unwrap_or(inherited);
            let before = value;
            ui.add_enabled_ui(!follow, |ui| {
                egui::Grid::new("action-auto-exit")
                    .num_columns(2)
                    .spacing(vec2(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Auto Exit").color(TEXT_DIM));
                        auto_exit_toggle(ui, &mut value.enabled);
                        ui.end_row();
                        auto_exit_delay_row(ui, &mut value);
                    });
            });
            if !follow && value != before {
                *setting = Some(value);
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("Returns one folder up after this action is executed.")
                    .small()
                    .color(TEXT_DIM),
            );
        });
}

fn auto_exit_delay_row(ui: &mut Ui, value: &mut AutoExit) {
    ui.label(RichText::new("Exit after").color(TEXT_DIM));
    ui.add_enabled(
        value.enabled,
        egui::DragValue::new(&mut value.delay)
            .range(0..=AutoExit::MAX_DELAY)
            .suffix(" s"),
    )
    .on_hover_text("0 seconds returns immediately after executing the action");
    ui.end_row();
}

/// A drawn switch, matching the switches in Preferences.
fn auto_exit_toggle(ui: &mut Ui, value: &mut bool) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(30.0, 20.0), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            *value,
            "Auto Exit",
        )
    });
    let t = ui.ctx().animate_bool(response.id, *value);
    let track = Rect::from_center_size(rect.center(), vec2(30.0, 16.0));
    ui.painter().rect_filled(
        track,
        CornerRadius::same(8),
        PREF_SEGMENT.lerp_to_gamma(PREF_CHECK, t),
    );
    let x = egui::lerp(track.left() + 8.0..=track.right() - 8.0, t);
    ui.painter()
        .circle_filled(pos2(x, track.center().y), 6.0, TEXT);
    response
}

/// Preview of what is being dragged.
fn drag_preview(ctx: &egui::Context) {
    let Some(payload) = egui::DragAndDrop::payload::<DragPayload>(ctx) else {
        return;
    };
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let glyph = match &*payload {
        DragPayload::Action(uuid) => actions::find(uuid).map_or(icon::PUZZLE_PIECE, |a| a.icon),
        DragPayload::Key(_) | DragPayload::Member(_) => icon::SQUARE,
    };
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drag-preview"));
    let p = ctx.layer_painter(layer);
    let r = Rect::from_center_size(pos + vec2(20.0, 20.0), Vec2::splat(40.0));
    p.rect_filled(r, CornerRadius::same(8), KEY_BORDER);
    p.rect_filled(r.shrink(2.0), CornerRadius::same(6), Color32::BLACK);
    p.text(r.center(), Align2::CENTER_CENTER, glyph, font(20.0), TEXT);
}

/// Puts a copied action in the open group: after the selected row, or in
/// the selected Key Logic slot in place of what it holds.
fn paste_member(key: &mut KeyConfig, group: &mut GroupEdit, copy: &KeyClipboard) {
    let Some(member) = copy.key().filter(|m| key.can_hold(&m.action)).cloned() else {
        return;
    };
    let logic = key.action == actions::KEY_LOGIC;
    let Some(list) = key.groups.get_mut(group.list) else {
        return;
    };
    if logic {
        if group.member.is_some() {
            *list = vec![member];
        }
    } else if list.len() < MAX_GROUP_ACTIONS {
        let at = group.member.map_or(list.len(), |m| m + 1).min(list.len());
        list.insert(at, member);
        group.member = Some(at);
    }
}

/// A new key or group member with an action's default settings.
fn new_key(uuid: &str) -> KeyConfig {
    match actions::find(uuid) {
        Some(def) => actions::new_key(def),
        None => KeyConfig::new(uuid, json!({})),
    }
}

/// What the inspector edits: the action selected in the open group, else the
/// key itself. `None` for an empty Key Logic slot.
fn edited_key<'a>(
    profile: &'a mut Profile,
    view: &View,
    index: u8,
    group: Option<GroupEdit>,
) -> Option<&'a mut KeyConfig> {
    let key = view.keys(profile)?.get_mut(&index)?;
    match group {
        Some(GroupEdit {
            list,
            member: Some(member),
        }) => key.groups.get_mut(list)?.get_mut(member),
        _ => Some(key),
    }
}

fn edited_context(page: &str, index: u8, group: Option<GroupEdit>) -> String {
    match group {
        Some(GroupEdit {
            list,
            member: Some(member),
        }) => plugins::member_context(page, index, list, member),
        _ => plugins::context(page, index),
    }
}

fn icon_button(ui: &mut Ui, glyph: &str, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    let color = if resp.hovered() { TEXT } else { TEXT_DIM };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        font(17.0),
        color,
    );
    resp.on_hover_text(tip)
}

/// The clock button of an action in a Multi Action and its popup.
fn delays_button(ui: &mut Ui, delays: &mut Option<ActionDelays>) {
    let resp = icon_button(ui, icon::CLOCK_COUNTDOWN, "Customize delays");
    let popup = egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(4.0)
        .frame(egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(14)));
    let mut d = delays.unwrap_or_default();
    let mut reset = false;
    popup.show(|ui| {
        ui.set_width(270.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Custom action delays").font(bold(13.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                reset = icon_button(
                    ui,
                    icon::ARROW_COUNTER_CLOCKWISE,
                    "Revert delays to defaults",
                )
                .clicked();
            });
        });
        ui.separator();
        ui.add_space(4.0);
        let row = |ui: &mut Ui, left: &str, right: &str, tip: &str, value: &mut u32| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{left} {} {right}", icon::DOTS_THREE)).color(TEXT_DIM),
                )
                .on_hover_text(tip);
                ui.spacing_mut().slider_width = 120.0;
                ui.add(
                    egui::Slider::new(value, ActionDelays::MIN..=ActionDelays::MAX)
                        .show_value(false),
                );
                ui.add(
                    egui::DragValue::new(value)
                        .range(ActionDelays::MIN..=ActionDelays::MAX)
                        .suffix(" ms"),
                );
            });
        };
        row(
            ui,
            icon::ARROW_LINE_DOWN,
            icon::ARROW_LINE_UP,
            "From the virtual key press to its release",
            &mut d.press,
        );
        row(
            ui,
            icon::NUMBER_SQUARE_ONE,
            icon::NUMBER_SQUARE_TWO,
            "From this action to the next one",
            &mut d.next,
        );
    });
    if reset {
        *delays = None;
    } else if d != delays.unwrap_or_default() {
        *delays = Some(d);
    }
}

/// Picks the state a many-state plugin action asks for in its Multi Action.
fn desired_state_row(ui: &mut Ui, key: &mut KeyConfig, states: usize) {
    if states < 2 {
        return;
    }
    ui.label(RichText::new("State").color(TEXT_DIM));
    ui.horizontal(|ui| {
        for state in 0..states.min(usize::from(u8::MAX)) as u8 {
            let on = key.desired_state.unwrap_or(0) == state;
            if ui.selectable_label(on, (state + 1).to_string()).clicked() {
                key.desired_state = Some(state);
            }
        }
    });
    ui.end_row();
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
mod tray_tests {
    use super::clipboard_tests::editor;
    use super::*;
    use eframe::egui::{ViewportCommand, ViewportEvent, ViewportId};

    /// Runs what eframe runs for a hidden window: `tick`, without any ui.
    fn pass(app: &mut App, ctx: &egui::Context, close: bool) -> Vec<ViewportCommand> {
        let mut input = egui::RawInput::default();
        if close && let Some(root) = input.viewports.get_mut(&ViewportId::ROOT) {
            root.events.push(ViewportEvent::Close);
        }
        ctx.run_logic(&input, |ctx| app.tick(ctx))
            .viewport_commands
            .remove(&ViewportId::ROOT)
            .unwrap_or_default()
    }

    fn tray_editor(reopens_window: bool) -> App {
        App {
            background: Background::idle(reopens_window),
            close_to_tray: true,
            ..editor()
        }
    }

    #[test]
    fn closing_hides_the_window_and_what_floats_over_it() {
        let ctx = egui::Context::default();
        let mut app = tray_editor(false);
        app.prefs.open(&ctx, None);
        let commands = pass(&mut app, &ctx, true);
        assert!(commands.contains(&ViewportCommand::CancelClose));
        assert!(commands.contains(&ViewportCommand::Visible(false)));
        assert!(app.background.hidden.load(Ordering::Relaxed));
        assert!(!app.prefs.is_open());
    }

    #[test]
    fn closing_quits_without_a_tray() {
        let ctx = egui::Context::default();
        let mut app = editor();
        assert!(pass(&mut app, &ctx, true).is_empty());
        assert!(!app.background.closing_to_tray());
    }

    #[test]
    fn closing_really_closes_where_the_window_cannot_hide() {
        let ctx = egui::Context::default();
        let mut app = tray_editor(true);
        assert!(pass(&mut app, &ctx, true).is_empty());
        assert!(app.background.closing_to_tray());
        assert!(!app.background.hidden.load(Ordering::Relaxed));
    }

    #[test]
    fn quit_right_after_closing_still_quits() {
        let ctx = egui::Context::default();
        let mut app = tray_editor(true);
        app.background.send(Command::Quit);
        assert!(pass(&mut app, &ctx, true).contains(&ViewportCommand::Close));
        assert!(!app.background.closing_to_tray());
    }

    #[test]
    fn show_brings_a_hidden_or_minimized_window_back() {
        let ctx = egui::Context::default();
        let mut app = tray_editor(false);
        pass(&mut app, &ctx, true);
        app.background.send(Command::Show);
        let commands = pass(&mut app, &ctx, false);
        assert!(commands.contains(&ViewportCommand::Visible(true)));
        assert!(commands.contains(&ViewportCommand::Minimized(false)));
        assert!(commands.contains(&ViewportCommand::Focus));
        assert!(!app.background.hidden.load(Ordering::Relaxed));
        assert!(!app.prefs.is_open());
    }

    #[test]
    fn preferences_from_the_tray_show_the_hidden_window_too() {
        let ctx = egui::Context::default();
        let mut app = tray_editor(false);
        pass(&mut app, &ctx, true);
        app.background.send(Command::Preferences);
        let commands = pass(&mut app, &ctx, false);
        assert!(commands.contains(&ViewportCommand::Visible(true)));
        assert!(!app.background.hidden.load(Ordering::Relaxed));
        pass(&mut app, &ctx, false);
        assert!(app.prefs.is_open());
    }

    /// Clicking Preferences in the tray with the window hidden crashed:
    /// `ui` follows `tick` in the same pass, and in a hidden window's pass
    /// eframe cannot create a window, so it never calls the viewport's
    /// callback and egui panics.
    #[test]
    fn preferences_wait_for_a_pass_that_can_create_their_window() {
        egui::Context::set_immediate_viewport_renderer(|_, _| {});
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut app = tray_editor(false);
        pass(&mut app, &ctx, true);
        app.background.send(Command::Preferences);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.tick(ui.ctx());
            app.preferences(ui.ctx(), None, DeviceKind::Mk2);
        });
        output.textures_delta.clear();
        assert!(!app.prefs.is_open());
        pass(&mut app, &ctx, false);
        assert!(app.prefs.is_open());
    }

    #[test]
    fn closing_again_cancels_preferences_that_are_on_their_way() {
        let ctx = egui::Context::default();
        let mut app = tray_editor(false);
        app.background.send(Command::Preferences);
        pass(&mut app, &ctx, false);
        pass(&mut app, &ctx, true);
        pass(&mut app, &ctx, false);
        assert!(!app.prefs.is_open());
    }

    #[test]
    fn quit_is_not_held_back_by_the_tray() {
        let ctx = egui::Context::default();
        let mut app = tray_editor(false);
        pass(&mut app, &ctx, true);
        app.background.send(Command::Quit);
        assert!(pass(&mut app, &ctx, false).contains(&ViewportCommand::Close));
        // eframe answers that command with a close request.
        assert!(pass(&mut app, &ctx, true).is_empty());
    }
}

#[cfg(test)]
mod auto_exit_tests {
    use super::*;

    type Shapes = Vec<egui::epaint::ClippedShape>;

    fn frame(
        ctx: &egui::Context,
        setting: &mut Option<AutoExit>,
        parent: bool,
        events: Vec<egui::Event>,
    ) -> Shapes {
        frame_result(ctx, setting, parent, events).0
    }

    fn frame_result(
        ctx: &egui::Context,
        setting: &mut Option<AutoExit>,
        parent: bool,
        events: Vec<egui::Event>,
    ) -> (Shapes, bool) {
        let mut reset = false;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(554.0, 310.0))),
                events,
                ..Default::default()
            },
            |ui| {
                if parent {
                    egui::Grid::new("parent-form")
                        .show(ui, |ui| reset = parent_auto_exit_rows(ui, setting));
                } else {
                    folder_exit_button(
                        ui,
                        setting,
                        AutoExit {
                            enabled: true,
                            delay: 3,
                        },
                    );
                }
            },
        );
        output.textures_delta.clear();
        (output.shapes, reset)
    }

    fn text_position(shapes: &Shapes, label: &str) -> egui::Pos2 {
        shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                _ => None,
            })
            .unwrap()
    }

    fn toggle_position(shapes: &Shapes) -> egui::Pos2 {
        shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) if circle.radius == 6.0 => Some(circle.center),
                _ => None,
            })
            .unwrap()
    }

    fn click(
        ctx: &egui::Context,
        setting: &mut Option<AutoExit>,
        parent: bool,
        pos: egui::Pos2,
    ) -> Shapes {
        click_result(ctx, setting, parent, pos).0
    }

    fn click_result(
        ctx: &egui::Context,
        setting: &mut Option<AutoExit>,
        parent: bool,
        pos: egui::Pos2,
    ) -> (Shapes, bool) {
        let mut shapes = Vec::new();
        let mut reset = false;
        for pressed in [true, false] {
            let result = frame_result(
                ctx,
                setting,
                parent,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            shapes = result.0;
            reset |= result.1;
        }
        (shapes, reset)
    }

    #[test]
    fn parent_auto_exit_reset_keeps_folder_settings() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut setting = Some(AutoExit {
            enabled: false,
            delay: 7,
        });
        frame(&ctx, &mut setting, true, Vec::new());
        let shapes = frame(&ctx, &mut setting, true, Vec::new());
        let pos = toggle_position(&shapes);
        let shapes = click(&ctx, &mut setting, true, pos);
        assert_eq!(
            setting,
            Some(AutoExit {
                enabled: true,
                delay: 7
            })
        );
        let pos = text_position(&shapes, icon::ARROW_COUNTER_CLOCKWISE);
        assert!(click_result(&ctx, &mut setting, true, pos).1);
        assert_eq!(
            setting,
            Some(AutoExit {
                enabled: true,
                delay: 7
            })
        );
    }

    #[test]
    fn individual_action_can_override_and_follow_its_folder_again() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut setting = None;
        frame(&ctx, &mut setting, false, Vec::new());
        let shapes = frame(&ctx, &mut setting, false, Vec::new());
        let pos = text_position(&shapes, icon::ARROW_BEND_LEFT_UP);
        click(&ctx, &mut setting, false, pos);
        let shapes = frame(&ctx, &mut setting, false, Vec::new());
        assert_eq!(setting, None);
        let pos = text_position(&shapes, "Follow folder settings");
        let shapes = click(&ctx, &mut setting, false, pos);
        assert_eq!(
            setting,
            Some(AutoExit {
                enabled: true,
                delay: 3
            })
        );
        let pos = toggle_position(&shapes);
        let shapes = click(&ctx, &mut setting, false, pos);
        assert_eq!(
            setting,
            Some(AutoExit {
                enabled: false,
                delay: 3
            })
        );
        let pos = text_position(&shapes, "Follow folder settings");
        click(&ctx, &mut setting, false, pos);
        assert_eq!(setting, None);
    }

    #[test]
    fn a_group_key_has_its_own_folder_exit_control_but_members_do_not() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = super::clipboard_tests::editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.pages[0]
            .keys
            .insert(0, new_key(actions::CREATE_FOLDER));
        profile.normalize_folders();
        let folder = profile.pages[0].keys[&0].folder().unwrap().to_owned();
        let mut key = new_key(actions::MULTI_ACTION);
        key.groups[0].push(new_key(actions::WEBSITE));
        profile.folder_mut(&folder).unwrap().keys.insert(1, key);
        app.selected_key = Some(1);
        let view = View {
            page: 0,
            path: vec![folder],
            live: Live::default(),
        };
        for member in [None, Some(0)] {
            app.group = Some(GroupEdit { list: 0, member });
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(852.0, 742.0))),
                    ..Default::default()
                },
                |ui| app.inspector(ui, &mut profile, &view),
            );
            output.textures_delta.clear();
            let shown = output.shapes.iter().any(|shape| {
                matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text==icon::ARROW_BEND_LEFT_UP)
            });
            assert_eq!(shown, member.is_none());
        }
    }

    #[test]
    fn resetting_folder_overrides_is_local_cancelable_and_undoable() {
        fn frame(
            ctx: &egui::Context,
            app: &mut App,
            profile: &mut Profile,
            view: &mut View,
            events: Vec<egui::Event>,
        ) -> Shapes {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(852.0, 742.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let original = profile.clone();
                    let location = view.location(app.selected_key);
                    app.inspector(ui, profile, view);
                    app.folder_exit_confirmation(ctx, profile, view);
                    app.finish_edit(ctx, profile, view, &original, location, None);
                },
            );
            output.textures_delta.clear();
            output.shapes
        }
        fn click(
            ctx: &egui::Context,
            app: &mut App,
            profile: &mut Profile,
            view: &mut View,
            pos: egui::Pos2,
        ) {
            for pressed in [true, false] {
                frame(
                    ctx,
                    app,
                    profile,
                    view,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = super::clipboard_tests::editor();
        // Settings saved before the question existed ask it too.
        let saved: deckly_core::Settings = serde_json::from_str("{}").unwrap();
        assert!(saved.confirm_folder_auto_exit_reset);
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.pages[0]
            .keys
            .insert(0, new_key(actions::CREATE_FOLDER));
        profile.normalize_folders();
        let outer = profile.pages[0].keys[&0].folder().unwrap().to_owned();
        let mut action = new_key(actions::WEBSITE);
        action.auto_exit = Some(AutoExit {
            enabled: false,
            delay: 11,
        });
        let mut inner_key = new_key(actions::CREATE_FOLDER);
        inner_key.auto_exit = action.auto_exit;
        let folder = profile.folder_mut(&outer).unwrap();
        folder.keys.get_mut(&0).unwrap().auto_exit = Some(AutoExit {
            enabled: true,
            delay: 7,
        });
        folder.keys.insert(1, action.clone());
        folder.keys.insert(2, inner_key);
        profile.normalize_folders();
        let inner = profile.folder(&outer).unwrap().keys[&2]
            .folder()
            .unwrap()
            .to_owned();
        profile.folder_mut(&inner).unwrap().keys.insert(1, action);
        let original = profile.clone();
        let mut view = View {
            page: 0,
            path: vec![outer.clone()],
            live: Live::default(),
        };
        frame(&ctx, &mut app, &mut profile, &mut view, Vec::new());
        let shapes = frame(&ctx, &mut app, &mut profile, &mut view, Vec::new());
        let pos = text_position(&shapes, icon::ARROW_COUNTER_CLOCKWISE);
        click(&ctx, &mut app, &mut profile, &mut view, pos);
        let shapes = frame(&ctx, &mut app, &mut profile, &mut view, Vec::new());
        let pos = text_position(&shapes, "Cancel");
        click(&ctx, &mut app, &mut profile, &mut view, pos);
        assert_eq!(profile, original);
        let shapes = frame(&ctx, &mut app, &mut profile, &mut view, Vec::new());
        let pos = text_position(&shapes, icon::ARROW_COUNTER_CLOCKWISE);
        click(&ctx, &mut app, &mut profile, &mut view, pos);
        let shapes = frame(&ctx, &mut app, &mut profile, &mut view, Vec::new());
        let pos = text_position(&shapes, "Reset");
        click(&ctx, &mut app, &mut profile, &mut view, pos);
        let folder = profile.folder(&outer).unwrap();
        assert_eq!(
            folder.keys[&0].auto_exit,
            original.folder(&outer).unwrap().keys[&0].auto_exit
        );
        assert_eq!(folder.keys[&1].auto_exit, None);
        assert_eq!(folder.keys[&2].auto_exit, None);
        assert_eq!(profile.folder(&inner), original.folder(&inner));
        assert!(app.dirty.contains(&profile.id));
        assert_eq!(
            app.history
                .step(&profile, Direction::Undo)
                .unwrap()
                .unwrap()
                .0,
            original
        );
        // With the question turned off the button resets at once.
        runtime::lock(&app.state)
            .settings
            .confirm_folder_auto_exit_reset = false;
        let mut profile = original;
        let shapes = frame(&ctx, &mut app, &mut profile, &mut view, Vec::new());
        let pos = text_position(&shapes, icon::ARROW_COUNTER_CLOCKWISE);
        click(&ctx, &mut app, &mut profile, &mut view, pos);
        assert_eq!(profile.folder(&outer).unwrap().keys[&1].auto_exit, None);
    }
}

#[cfg(test)]
mod key_image_tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        app: &mut App,
        profile: &mut Profile,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let mut view = View {
            page: 0,
            path: Vec::new(),
            live: Live::default(),
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(852.0, 742.0))),
                events,
                ..Default::default()
            },
            |ui| {
                let original = profile.clone();
                let location = view.location(app.selected_key);
                app.inspector(ui, profile, &view);
                app.finish_edit(ctx, profile, &mut view, &original, location, None);
            },
        );
        output.textures_delta.clear();
        output.shapes
    }

    #[test]
    fn reset_button_preserves_action_and_is_undoable() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = super::clipboard_tests::editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        let mut key = KeyConfig::new(actions::WEBSITE, json!({"url":"https://example.com"}));
        key.image = Some("custom-key.png".into());
        key.title = "Keep title".into();
        profile.pages[0].keys.insert(0, key.clone());
        let before = profile.clone();
        frame(&ctx, &mut app, &mut profile, Vec::new());
        let shapes = frame(&ctx, &mut app, &mut profile, Vec::new());
        let pos = shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Text(text) = &s.shape
                    && text.galley.job.text.ends_with("  Reset")
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
            );
        }
        key.image = None;
        assert_eq!(profile.pages[0].keys[&0], key);
        assert!(app.dirty.contains(&profile.id));
        let (restored, _) = app
            .history
            .step(&profile, Direction::Undo)
            .unwrap()
            .unwrap();
        assert_eq!(restored, before);
        assert_eq!(
            app.history
                .step(&restored, Direction::Redo)
                .unwrap()
                .unwrap()
                .0,
            profile
        );
    }

    #[test]
    fn image_editor_closes_when_selection_changes_and_keeps_profile() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = super::clipboard_tests::editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        let key = KeyConfig::new(actions::WEBSITE, json!({}));
        profile.pages[0].keys.insert(0, key.clone());
        let context = format!("{}/{}.0", profile.id, profile.pages[0].id);
        app.image_editor = Some(crate::image_editor::Editor::new(
            context,
            &key,
            crate::image_editor::Mode::Create,
        ));
        let original = profile.clone();
        frame(&ctx, &mut app, &mut profile, Vec::new());
        assert!(app.image_editor.is_some());
        app.selected_key = None;
        frame(&ctx, &mut app, &mut profile, Vec::new());
        assert!(app.image_editor.is_none());
        assert_eq!(profile, original);
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    pub(super) fn editor() -> App {
        let background = Background::idle(false);
        App {
            state: background.state.clone(),
            runtime: background.runtime.clone(),
            renderer: Renderer::new(),
            textures: HashMap::new(),
            animated: HashMap::new(),
            selected_device: None,
            offline_profile: None,
            selected_key: Some(0),
            group: None,
            group_holder: None,
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
            app_picker: None,
            image_editor: None,
            wallpaper_editor: None,
            clipboard: None,
            pending_paste: None,
            pending_folder_exit_reset: None,
            history: History::default(),
            history_shortcut: None,
            dirty: HashSet::new(),
            last_edit: Instant::now(),
            prefs: Preferences::new(),
            rename: None,
            folder_nav: None,
            split: DEFAULT_SPLIT,
            list_width: LIST_WIDTH,
            background,
            prefs_after_show: false,
            close_to_tray: false,
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

    #[test]
    fn opening_and_cancelling_application_picker_keeps_the_action() {
        fn inspector_frame(
            ctx: &egui::Context,
            app: &mut App,
            profile: &mut Profile,
            events: Vec<egui::Event>,
        ) -> Vec<egui::epaint::ClippedShape> {
            let view = View {
                page: 0,
                path: Vec::new(),
                live: Live::default(),
            };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(852.0, 742.0))),
                    events,
                    ..Default::default()
                },
                |ui| app.inspector(ui, profile, &view),
            );
            output.textures_delta.clear();
            output.shapes
        }
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut app = editor();
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.pages[0].keys.insert(
            0,
            KeyConfig::new(actions::OPEN, json!({"path":"existing command"})),
        );
        let before = profile.clone();
        let shapes = inspector_frame(&ctx, &mut app, &mut profile, Vec::new());
        let pos = shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.job.text.contains("Applications…")
                {
                    Some(text.pos + text.galley.size() / 2.0)
                } else {
                    None
                }
            })
            .unwrap();
        for pressed in [true, false] {
            inspector_frame(
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
            );
        }
        assert!(app.app_picker.is_some());
        inspector_frame(&ctx, &mut app, &mut profile, Vec::new());
        inspector_frame(
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
        );
        assert!(app.app_picker.is_none());
        assert_eq!(profile, before);
        assert!(app.dirty.is_empty());
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

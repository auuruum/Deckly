//! The Preferences window.
//!
//! A separate window with General, Devices, Profiles and Style tabs. Layout,
//! sizes and colours follow the official app's Preferences window
//! (docs/clean-room/2026-10-26-preferences.md); controls are placed at the
//! measured positions, relative to the window's client area.

use crate::actions;
use crate::app_picker::{self, Picker};
use crate::backup;
use crate::cropper::{Cropper, Purpose};
use crate::image_editor::{self, Editor, ImageChoice, Mode, Selection};
use crate::logo;
use crate::os::autostart;
use crate::plugins;
use crate::render::{self, Live, Renderer};
use crate::runtime::{self, Msg, Shared};
use crate::store;
use crate::theme::*;
use crate::transfer::{self, Job, PluginKind, SharedJob, Summary};
use deckly_core::profile::{
    MAX_PAGE_NAME_CHARS, MAX_PAGES, MAX_PROFILE_APPLICATIONS, MAX_PROFILE_NAME_CHARS,
};
use deckly_core::settings::{self, MAX_DEVICE_NAME_CHARS, SLEEP_CHOICES};
use deckly_core::{
    AppRule, DeviceKind, DeviceModel, KeyConfig, KeyPress, LockLook, Orientation, Page, Profile,
    TitleAlign, TitleStyle,
};
use deckly_plugin_host::Host;
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense,
    Stroke, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2,
};
use egui_phosphor::regular as icon;
use image::RgbaImage;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

const SIZE: Vec2 = vec2(740.0, 489.0);
const ADVANCED_SIZE: Vec2 = vec2(400.0, 200.0);
const STRIP_HEIGHT: f32 = 35.0;
const APP_ICON_SIZE: f32 = 18.0;
const LABEL_X: f32 = 20.0;
const LABEL_WIDTH: f32 = 182.0;
const VALUE_X: f32 = 224.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    General,
    Devices,
    Profiles,
    Style,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::General, Tab::Devices, Tab::Profiles, Tab::Style];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Devices => "Devices",
            Tab::Profiles => "Profiles",
            Tab::Style => "Style",
        }
    }
}

/// What the main window passes in each frame.
pub struct Env<'a> {
    pub state: &'a Shared,
    pub runtime: &'a Sender<Msg>,
    pub renderer: &'a mut Renderer,
    /// Serial of the device selected in the main window, if connected.
    pub device: Option<&'a str>,
    /// Model of the profile shown in the main window.
    pub kind: DeviceKind,
    pub host: Option<&'a Host>,
}

pub struct Preferences {
    open: bool,
    tab: Tab,
    /// Device shown on the Devices tab.
    device: Option<String>,
    /// Profile selected on the Profiles tab.
    profile: Option<String>,
    /// Page of that profile selected on the tab, instead of the profile.
    page: Option<String>,
    /// Profiles showing their pages in the list.
    expanded: HashSet<String>,
    /// Profile being renamed in the list.
    renaming: Option<Rename>,
    /// Profile waiting for delete confirmation.
    confirm_delete: Option<String>,
    /// Profile and page waiting for page deletion confirmation.
    confirm_delete_page: Option<(String, String)>,
    /// Style tab edits; they take effect with Apply.
    style: Option<TitleStyle>,
    /// Read from the system when the General tab is first shown.
    autostart: Option<Result<bool, String>>,
    /// The official app's data folder, looked for at the same time.
    official: Option<Option<PathBuf>>,
    /// A running import or backup, or its result.
    job: SharedJob,
    /// What the import dialog will bring in.
    import_profiles: bool,
    import_plugins: bool,
    preview: Option<(u64, TextureHandle)>,
    /// The Advanced Settings window, open for this device.
    advanced: Option<(String, &'static DeviceModel)>,
    cropper: Cropper,
    /// Select a duplicated profile when its background save finishes.
    pending_profile: Option<String>,
    page_icon_editor: Option<PageIconEditor>,
    /// Choosing an application for the profile in its `context`.
    app_picker: Option<Picker>,
    /// Why the application just chosen could not be added.
    app_note: Option<String>,
    /// Pictures of the applications a profile lists, while the window is open.
    app_icons: Option<app_picker::icons::Cache>,
}

struct PageIconEditor {
    profile: String,
    page: String,
    editor: Editor,
}

impl PageIconEditor {
    fn matches(&self, state: &runtime::State) -> bool {
        state
            .profile(&self.profile)
            .and_then(|profile| profile.pages.iter().find(|page| page.id == self.page))
            .is_some_and(|page| self.editor.matches_page(&self.profile, page))
    }

    /// Revalidate the destination after background image work; save before applying.
    fn apply(
        &self,
        state: &mut runtime::State,
        path: PathBuf,
        save: impl FnOnce(&Profile) -> Result<(), String>,
    ) -> Result<bool, String> {
        if !self.matches(state) {
            return Ok(false);
        }
        let Some(mut profile) = state.profile(&self.profile).cloned() else {
            return Ok(false);
        };
        let Some(page) = profile.pages.iter_mut().find(|page| page.id == self.page) else {
            return Ok(false);
        };
        page.icon = Some(path);
        save(&profile)?;
        if let Some(target) = state.profile_mut(&self.profile) {
            *target = profile;
        }
        Ok(true)
    }
}

/// A profile as the Profiles tab lists it.
struct ProfileRow {
    id: String,
    name: String,
    pages: Vec<PageRow>,
}

struct PageRow {
    id: String,
    name: String,
    title: String,
    icon: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum ProfileCommand {
    Rename,
    Duplicate,
    Export,
    AddPage,
    Delete,
    Import,
}

enum PageCommand {
    Delete,
    Icon(ImageChoice),
}

/// The row menu and the options button expose the same commands.
fn profile_commands(ui: &mut Ui, profile: &ProfileRow, can_delete: bool) -> Option<ProfileCommand> {
    let mut selected = None;
    for (command, glyph, label, enabled) in [
        (ProfileCommand::Rename, icon::PENCIL_SIMPLE, "Rename", true),
        (ProfileCommand::Duplicate, icon::COPY, "Duplicate", true),
        (ProfileCommand::Export, icon::EXPORT, "Export…", true),
        (
            ProfileCommand::AddPage,
            icon::FILE_PLUS,
            "Add Page",
            profile.pages.len() < MAX_PAGES,
        ),
        (ProfileCommand::Delete, icon::TRASH, "Delete", can_delete),
        (
            ProfileCommand::Import,
            icon::DOWNLOAD_SIMPLE,
            "Import…",
            true,
        ),
    ] {
        if ui
            .add_enabled(enabled, egui::Button::new(format!("{glyph}  {label}")))
            .clicked()
        {
            selected = Some(command);
            ui.close();
        }
    }
    selected
}

fn page_menu(ui: &mut Ui, can_delete: bool, has_icon: bool) -> Option<PageCommand> {
    let mut choice = None;
    ui.menu_button(format!("{}  Change Icon", icon::IMAGE), |ui| {
        if let Some(pick) = image_editor::image_menu(ui, has_icon) {
            choice = Some(PageCommand::Icon(pick));
        }
    });
    ui.separator();
    let delete = ui
        .add_enabled(
            can_delete,
            egui::Button::new(format!("{}  Delete Page", icon::TRASH)),
        )
        .clicked();
    if delete {
        ui.close();
    }
    if delete {
        Some(PageCommand::Delete)
    } else {
        choice
    }
}

impl ProfileRow {
    fn new(profile: &Profile) -> Self {
        let page = |(index, page): (usize, &Page)| PageRow {
            id: page.id.clone(),
            name: page.name.clone(),
            title: page.title(index),
            icon: page.icon.clone(),
        };
        Self {
            id: profile.id.clone(),
            name: profile.name.clone(),
            pages: profile.pages.iter().enumerate().map(page).collect(),
        }
    }
}

/// A profile name being edited in place.
struct Rename {
    id: String,
    text: String,
    /// Focus the field and select its text on the next frame.
    fresh: bool,
}

impl Rename {
    fn new(id: String, text: String) -> Self {
        Self {
            id,
            text,
            fresh: true,
        }
    }
}

impl Preferences {
    pub fn new() -> Self {
        Self {
            open: false,
            tab: Tab::General,
            device: None,
            profile: None,
            page: None,
            expanded: HashSet::new(),
            renaming: None,
            confirm_delete: None,
            confirm_delete_page: None,
            style: None,
            autostart: None,
            official: None,
            job: SharedJob::default(),
            import_profiles: true,
            import_plugins: true,
            preview: None,
            advanced: None,
            cropper: Cropper::default(),
            pending_profile: None,
            page_icon_editor: None,
            app_picker: None,
            app_note: None,
            app_icons: None,
        }
    }

    /// Opens the window, or brings it to the front when already open.
    pub fn open(&mut self, ctx: &egui::Context, device: Option<&str>) {
        if self.open {
            ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Focus);
        }
        self.open = true;
        self.device = device.map(str::to_owned);
        self.autostart = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.advanced = None;
        self.page_icon_editor = None;
        self.app_picker = None;
        self.app_icons = None;
    }

    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window on the Devices tab.
    pub fn open_devices(&mut self, ctx: &egui::Context, device: Option<&str>) {
        self.open(ctx, device);
        self.tab = Tab::Devices;
    }

    pub fn show(&mut self, ctx: &egui::Context, env: &mut Env<'_>) {
        if !self.open {
            return;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Preferences - Deckly")
            .with_icon(logo::window_icon())
            .with_inner_size(SIZE)
            .with_min_inner_size(SIZE)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_minimize_button(false);
        ctx.show_viewport_immediate(viewport_id(), builder, |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                self.open = false;
            }
            egui::CentralPanel::no_frame().show(ui, |ui| self.contents(ui, env));
            self.job_dialog(ui.ctx(), env);
            self.application_picker(ui.ctx(), env);
        });
        if !self.open {
            self.advanced = None;
            self.page_icon_editor = None;
            self.app_picker = None;
            self.app_icons = None;
        }
        self.show_advanced(ctx, env);
        if let Some((purpose, image)) = self.cropper.show(ctx) {
            use_picture(env, purpose, image);
        }
        self.show_page_icon_editor(ctx, env);
    }

    fn show_page_icon_editor(&mut self, ctx: &egui::Context, env: &mut Env<'_>) {
        let valid = self.open
            && self.tab == Tab::Profiles
            && self.page_icon_editor.as_ref().is_some_and(|edit| {
                self.profile.as_ref() == Some(&edit.profile)
                    && self.page.as_ref() == Some(&edit.page)
                    && edit.matches(&runtime::lock(env.state))
            });
        if !valid {
            self.page_icon_editor = None;
            return;
        }
        let Some(edit) = self.page_icon_editor.as_mut() else {
            return;
        };
        let builder = egui::ViewportBuilder::default()
            .with_title("Page icon - Deckly")
            .with_icon(logo::window_icon())
            .with_inner_size([852.0, 742.0])
            .with_min_inner_size([852.0, 742.0]);
        let mut selection = Selection::Pending;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("page-icon-editor"),
            builder,
            |ui, _| {
                if ui.ctx().input(|input| input.viewport().close_requested()) {
                    selection = Selection::Cancelled;
                } else {
                    egui::CentralPanel::no_frame().show(ui, |ui| {
                        selection = edit.editor.show(ui.ctx(), &edit.profile);
                    });
                }
            },
        );
        match selection {
            Selection::Pending => {}
            Selection::Cancelled => self.page_icon_editor = None,
            Selection::Image(path) => {
                let mut state = runtime::lock(env.state);
                match edit.apply(&mut state, path, store::save_profile) {
                    Ok(true) => {
                        let _ = env.runtime.send(Msg::Refresh);
                    }
                    Ok(false) => {}
                    Err(error) => state.notify(format!("Cannot save page icon: {error}")),
                }
                self.page_icon_editor = None;
            }
        }
    }

    // ----- Advanced Settings ---------------------------------------------

    fn show_advanced(&mut self, ctx: &egui::Context, env: &mut Env<'_>) {
        let Some((serial, model)) = self.advanced.clone() else {
            return;
        };
        let builder = egui::ViewportBuilder::default()
            .with_title("Advanced Settings - Deckly")
            .with_icon(logo::window_icon())
            .with_inner_size(ADVANCED_SIZE)
            .with_min_inner_size(ADVANCED_SIZE)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_minimize_button(false);
        let mut close = false;
        ctx.show_viewport_immediate(advanced_id(), builder, |ui, _class| {
            close |= ui.ctx().input(|i| i.viewport().close_requested());
            egui::CentralPanel::no_frame().show(ui, |ui| {
                let o = ui.max_rect().min;
                ui.painter()
                    .rect_filled(ui.max_rect(), CornerRadius::ZERO, PREF_BODY);
                ui.painter().text(
                    o + vec2(20.0, 38.0),
                    Align2::LEFT_CENTER,
                    "Standby screen",
                    font(13.0),
                    TEXT_DIM,
                );
                frame(ui, at(o, 20.0, 52.0, 360.0, 92.0));
                if model.screen.is_some() {
                    if button(ui, at(o, 33.0, 64.0, 162.0, 22.0), "Set Image…").clicked() {
                        let purpose = Purpose::Standby(serial.clone());
                        self.cropper.open(ui.ctx(), purpose, model);
                    }
                    if button(ui, at(o, 205.0, 64.0, 162.0, 22.0), "Reset").clicked() {
                        let image = render::default_standby(model);
                        let serial = serial.clone();
                        let _ = env.runtime.send(Msg::Standby { serial, image });
                    }
                    value_dim(
                        ui,
                        o,
                        33.0,
                        94.0,
                        335.0,
                        "Set a custom image that appears when the device is powered \
                         on but not connected to Deckly.",
                    );
                } else {
                    let text = format!("{} does not support a standby image.", model.name);
                    value_dim(ui, o, 33.0, 64.0, 335.0, &text);
                }
                let bottom = ADVANCED_SIZE.y - 37.0;
                close |= button(ui, at(o, 262.0, bottom, 56.0, 22.0), "OK").clicked();
                close |= button(ui, at(o, 324.0, bottom, 56.0, 22.0), "Cancel").clicked();
            });
        });
        if close {
            self.advanced = None;
        }
    }

    fn contents(&mut self, ui: &mut Ui, env: &mut Env<'_>) {
        let origin = ui.max_rect().min;
        let painter = ui.painter().clone();
        painter.rect_filled(ui.max_rect(), CornerRadius::ZERO, PREF_BODY);
        painter.rect_filled(
            Rect::from_min_size(origin, vec2(ui.max_rect().width(), STRIP_HEIGHT)),
            CornerRadius::ZERO,
            PREF_STRIP,
        );
        for (i, tab) in Tab::ALL.into_iter().enumerate() {
            let r = at(
                origin,
                7.0 + 91.0 * i as f32,
                10.0,
                91.0,
                STRIP_HEIGHT - 10.0,
            );
            let resp = ui.interact(r, ui.id().with(("pref-tab", i)), Sense::click());
            let selected = self.tab == tab;
            if selected {
                painter.rect_filled(
                    r,
                    CornerRadius {
                        nw: 3,
                        ne: 3,
                        sw: 0,
                        se: 0,
                    },
                    PREF_BODY,
                );
                let stroke = Stroke::new(1.0, PREF_TAB_BORDER);
                painter.line_segment([r.left_bottom(), r.left_top()], stroke);
                painter.line_segment([r.left_top(), r.right_top()], stroke);
                painter.line_segment([r.right_top(), r.right_bottom()], stroke);
            }
            let color = if selected || resp.hovered() {
                TEXT
            } else {
                TEXT_DIM
            };
            let text_y = r.top() + 12.5;
            painter.text(
                pos2(r.center().x, text_y),
                Align2::CENTER_CENTER,
                tab.label(),
                font(13.0),
                color,
            );
            if resp.clicked() {
                self.tab = tab;
            }
        }
        match self.tab {
            Tab::General => self.general(ui, origin, env),
            Tab::Devices => self.devices(ui, origin, env),
            Tab::Profiles => self.profiles(ui, origin, env),
            Tab::Style => self.style(ui, origin, env),
        }
    }

    // ----- General -------------------------------------------------------

    fn general(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        label(ui, o, 61.0, 30.0, "Version:");
        value(ui, o, VALUE_X, 61.0, 30.0, env!("CARGO_PKG_VERSION"));

        label(ui, o, 91.0, 22.0, "Startup:");
        let enabled = self.autostart.get_or_insert_with(autostart::is_enabled);
        match enabled {
            Ok(on) => {
                let mut on = *on;
                if checkbox(
                    ui,
                    at(o, VALUE_X, 92.0, 200.0, 20.0),
                    &mut on,
                    "Open automatically at login",
                )
                .changed()
                {
                    self.autostart = Some(match autostart::set_enabled(on) {
                        Ok(()) => Ok(on),
                        Err(e) => {
                            runtime::lock(env.state).notify(format!("Open at login: {e}"));
                            autostart::is_enabled()
                        }
                    });
                }
            }
            Err(e) => {
                let text = format!("Open at login is not available: {e}");
                value(ui, o, VALUE_X, 91.0, 22.0, &text);
            }
        }

        label(ui, o, 113.0, 30.0, "Settings Folder:");
        if let Some(dir) = store::config_dir() {
            let r = at(o, VALUE_X, 117.0, 100.0, 22.0);
            if button(ui, r, "Open Folder…")
                .on_hover_text(dir.display().to_string())
                .clicked()
            {
                let _ = std::fs::create_dir_all(&dir);
                if let Err(e) = crate::os::open(&dir.to_string_lossy()) {
                    runtime::lock(env.state).notify(e);
                }
            }
        }

        label(ui, o, 147.0, 30.0, "Backup:");
        if button(ui, at(o, VALUE_X, 151.0, 130.0, 22.0), "Create Backup…")
            .on_hover_text("Save profiles, settings and plugins to one file")
            .clicked()
        {
            self.create_backup(ui.ctx(), env);
        }
        let restore = at(o, VALUE_X + 140.0, 151.0, 150.0, 22.0);
        if button(ui, restore, "Restore from Backup…")
            .on_hover_text("Bring back a Deckly backup, also from another computer")
            .clicked()
        {
            self.restore_backup(ui.ctx(), env);
        }

        label(ui, o, 181.0, 30.0, "Stream Deck App:");
        let official = self
            .official
            .get_or_insert_with(transfer::official_dir)
            .clone();
        let mut x = VALUE_X;
        if let Some(dir) = official {
            if button(ui, at(o, x, 185.0, 130.0, 22.0), "Import Everything…")
                .on_hover_text("Import the profiles and plugins of the app on this computer")
                .clicked()
            {
                self.scan(ui.ctx(), env, dir.join("ProfilesV3"), Some(dir));
            }
            x += 140.0;
        }
        if button(ui, at(o, x, 185.0, 150.0, 22.0), "Import Backup File…")
            .on_hover_text("Import a backup or profile exported by the official app")
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Stream Deck backup",
                    &["streamDeckProfilesBackup", "streamDeckProfile"],
                )
                .pick_file()
        {
            self.scan(ui.ctx(), env, path, None);
        }

        self.lock_keys(ui, o, env);
        self.switch_by_window(ui, o, env);

        label(ui, o, 392.0, 22.0, "About:");
        let text = "Deckly is an independent app. It is not made, approved or \
                    supported by Elgato or Corsair.";
        value_dim(ui, o, VALUE_X, 395.0, 440.0, text);

        footer(ui, o, "Deckly is built with Rust and egui.");
    }

    /// Whether keys stop on the lock screen, and what devices show then.
    fn lock_keys(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        label(ui, o, 215.0, 22.0, "Lock Screen:");
        let mut s = runtime::lock(env.state);
        let mut on = s.settings.lock_keys;
        let text = "Lock keys while the computer is locked";
        checkbox(ui, at(o, VALUE_X, 216.0, 280.0, 20.0), &mut on, text);
        label(ui, o, 241.0, 30.0, "While Locked:");
        let mut look = s.settings.lock_look;
        ui.add_enabled_ui(on, |ui| {
            combo(
                ui,
                at(o, VALUE_X + 1.0, 243.0, 163.0, 25.0),
                "pref-lock-look",
                look.label(),
                |ui| {
                    for choice in LockLook::ALL {
                        ui.selectable_value(&mut look, choice, choice.label());
                    }
                },
            );
        });
        if on != s.settings.lock_keys || look != s.settings.lock_look {
            s.settings.lock_keys = on;
            s.settings.lock_look = look;
            s.save_settings();
            let _ = env.runtime.send(Msg::Settings);
        }
        let note = if s.gate.undetected() {
            "This system does not report its lock screen, so keys stay active there."
        } else {
            "Keys do nothing until you unlock. Sleep turns the keys off or shows the \
             screensaver; Standby Screen shows the picture stored on the device."
        };
        value_dim(ui, o, VALUE_X, 279.0, 440.0, note);
    }

    /// Whether profiles follow the applications they list.
    fn switch_by_window(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        label(ui, o, 322.0, 22.0, "Active Window:");
        let mut s = runtime::lock(env.state);
        let mut on = s.settings.switch_by_window;
        let text = "Switch profiles by active window";
        if checkbox(ui, at(o, VALUE_X, 323.0, 280.0, 20.0), &mut on, text).changed() {
            s.settings.switch_by_window = on;
            s.save_settings();
            let _ = env.runtime.send(Msg::Settings);
        }
        match s.follower.error() {
            Some(error) => wrapped(ui, o, VALUE_X, 347.0, 440.0, error, WARNING),
            None => value_dim(
                ui,
                o,
                VALUE_X,
                347.0,
                440.0,
                "Choose the applications of a profile on the Profiles tab. It shows \
                 while one of them is active, then the device goes back.",
            ),
        }
    }

    fn create_backup(&mut self, ctx: &egui::Context, env: &Env<'_>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Deckly backup", &[backup::EXTENSION])
            .set_file_name(format!("Deckly.{}", backup::EXTENSION))
            .save_file()
        else {
            return;
        };
        let (state, host) = (env.state.clone(), env.host.cloned());
        transfer::start(&self.job, ctx, "Creating the backup", move |progress| {
            let done = backup::create(&path, &state, host.as_ref(), progress);
            finished("Backup failed", done)
        });
    }

    fn restore_backup(&mut self, ctx: &egui::Context, env: &Env<'_>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Deckly backup", &[backup::EXTENSION])
            .pick_file()
        else {
            return;
        };
        let (state, host) = (env.state.clone(), env.host.cloned());
        let runtime = env.runtime.clone();
        transfer::start(&self.job, ctx, "Restoring the backup", move |progress| {
            let done = backup::restore(&path, &state, host.as_ref(), &runtime, progress);
            finished("Restore failed", done)
        });
    }

    /// Reads a source; what it found waits in the import dialog.
    fn scan(
        &mut self,
        ctx: &egui::Context,
        env: &Env<'_>,
        path: PathBuf,
        official: Option<PathBuf>,
    ) {
        let state = env.state.clone();
        self.import_profiles = true;
        self.import_plugins = true;
        transfer::start(
            &self.job,
            ctx,
            "Reading profiles",
            move |_| match transfer::scan(&path, official.as_deref().map(Path::new), &state) {
                Ok(scan) => Job::Review(Box::new(scan)),
                Err(e) => finished("Import failed", Err(e)),
            },
        );
    }

    /// The dialog over the window while a job runs or waits for an answer.
    fn job_dialog(&mut self, ctx: &egui::Context, env: &Env<'_>) {
        let mut close = false;
        let mut import = false;
        let job = self.job.clone();
        let mut job = match job.lock() {
            Ok(job) => job,
            Err(poisoned) => poisoned.into_inner(),
        };
        match &*job {
            Job::Idle => return,
            Job::Busy(text) => {
                egui::Modal::new(egui::Id::new("job-busy")).show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(text);
                    });
                });
            }
            Job::Review(scan) => {
                egui::Modal::new(egui::Id::new("job-review")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading("Import profiles");
                    ui.add_space(8.0);
                    ui.checkbox(
                        &mut self.import_profiles,
                        transfer::count(scan.profiles.len(), "profile"),
                    );
                    if scan.replaces > 0 {
                        ui.small(format!(
                            "{} of them replace profiles imported earlier, with any changes \
                             made since.",
                            scan.replaces
                        ));
                    }
                    let ready = scan.count(PluginKind::Ready);
                    if ready > 0 {
                        let text = transfer::count(ready, "plugin");
                        ui.checkbox(&mut self.import_plugins, text);
                    }
                    let installed = scan.count(PluginKind::Installed);
                    if installed > 0 {
                        ui.small(format!("{installed} more plugins are already installed."));
                    }
                    let encrypted = scan.count(PluginKind::Encrypted);
                    if encrypted > 0 {
                        ui.small(format!(
                            "{encrypted} plugins are encrypted by the official marketplace and \
                             cannot be copied. Their keys are kept."
                        ));
                    }
                    if !scan.skipped.is_empty() {
                        let n = transfer::count(scan.skipped.len(), "profile");
                        ui.small(format!("{n} cannot be imported."));
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        close = ui.button("Cancel").clicked();
                        let any = (self.import_profiles && !scan.profiles.is_empty())
                            || (self.import_plugins && ready > 0);
                        import = ui.add_enabled(any, egui::Button::new("Import")).clicked();
                    });
                });
            }
            Job::Done(summary) => {
                egui::Modal::new(egui::Id::new("job-done")).show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.heading(&summary.title);
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for line in &summary.lines {
                                ui.label(line);
                                ui.add_space(4.0);
                            }
                        });
                    ui.add_space(12.0);
                    close = ui.button("OK").clicked();
                });
            }
        }
        if close {
            *job = Job::Idle;
        } else if import {
            let taken = std::mem::take(&mut *job);
            drop(job);
            if let Job::Review(scan) = taken {
                let (state, host) = (env.state.clone(), env.host.cloned());
                let runtime = env.runtime.clone();
                let (profiles, plugins) = (self.import_profiles, self.import_plugins);
                transfer::start(&self.job, ctx, "Importing", move |progress| {
                    Job::Done(transfer::commit(
                        *scan,
                        profiles,
                        plugins,
                        &state,
                        host.as_ref(),
                        &runtime,
                        progress,
                    ))
                });
            }
        }
    }

    // ----- Devices -------------------------------------------------------

    fn devices(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        let panel = at(o, 20.0, 55.0, 700.0, 414.0);
        frame(ui, panel);
        let devices = runtime::lock(env.state).devices.clone();
        let Some(device) = devices
            .iter()
            .find(|d| Some(&d.serial) == self.device.as_ref())
            .or_else(|| devices.first())
        else {
            ui.painter().text(
                panel.center(),
                Align2::CENTER_CENTER,
                "No device found",
                font(13.0),
                TEXT_DIM,
            );
            return;
        };
        self.device = Some(device.serial.clone());
        let mut y = 68.0;
        let mut s = runtime::lock(env.state);
        let before = s.settings.device(&device.serial);
        let mut prefs = before.clone();

        // A picker when more than one device is connected.
        if devices.len() > 1 {
            label(ui, o, y, 30.0, "Device:");
            let names: Vec<(String, String)> = devices
                .iter()
                .map(|d| {
                    (
                        d.serial.clone(),
                        format!("{} ({})", s.device_name(d), d.serial),
                    )
                })
                .collect();
            let current = names
                .iter()
                .find(|(serial, _)| *serial == device.serial)
                .map_or(String::new(), |(_, n)| n.clone());
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(at(o, VALUE_X + 1.0, y + 2.0, 300.0, 25.0)),
                |ui| {
                    egui::ComboBox::from_id_salt("pref-device")
                        .selected_text(current)
                        .width(300.0)
                        .show_ui(ui, |ui| {
                            for (serial, name) in &names {
                                if ui
                                    .selectable_label(*serial == device.serial, name)
                                    .clicked()
                                {
                                    self.device = Some(serial.clone());
                                }
                            }
                        });
                },
            );
            y += 36.0;
        }

        label(ui, o, y, 30.0, "Device Name:");
        let field = at(o, VALUE_X + 1.0, y + 2.0, 163.0, 25.0);
        ui.put(
            field,
            egui::TextEdit::singleline(&mut prefs.name)
                .hint_text(RichText::new(device.model.name).color(TEXT_FAINT))
                .char_limit(MAX_DEVICE_NAME_CHARS)
                .vertical_align(egui::Align::Center)
                .font(font(13.0)),
        );
        toggle(
            ui,
            at(o, VALUE_X + 185.0, y + 6.0, 70.0, 18.0),
            &mut prefs.enabled,
            "Enabled",
        );
        y += 36.0;

        label(ui, o, y, 30.0, "Visual Orientation:");
        combo(
            ui,
            at(o, VALUE_X + 1.0, y + 2.0, 163.0, 25.0),
            "pref-orientation",
            prefs.orientation.label(),
            |ui| {
                let groups: [(&str, &[Orientation]); 2] = [
                    ("Icons and text", &Orientation::ICONS),
                    ("Device", &Orientation::DEVICE),
                ];
                for (i, (title, choices)) in groups.into_iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    ui.label(RichText::new(title).font(font(13.0)).color(TEXT_FAINT));
                    for &choice in choices {
                        ui.selectable_value(&mut prefs.orientation, choice, choice.label());
                    }
                }
            },
        );
        y += 36.0;

        label(ui, o, y, 30.0, "Sleep After:");
        combo(
            ui,
            at(o, VALUE_X + 1.0, y + 2.0, 163.0, 25.0),
            "pref-sleep",
            &settings::sleep_label(prefs.sleep_minutes),
            |ui| {
                for minutes in SLEEP_CHOICES {
                    ui.selectable_value(
                        &mut prefs.sleep_minutes,
                        minutes,
                        settings::sleep_label(minutes),
                    );
                }
            },
        );
        if button(
            ui,
            at(o, VALUE_X + 185.0, y + 4.0, 108.0, 22.0),
            "Set Screensaver…",
        )
        .on_hover_text("Shown on the keys when the device sleeps")
        .clicked()
        {
            let purpose = Purpose::Screensaver(device.serial.clone());
            self.cropper.open(ui.ctx(), purpose, device.model);
        }
        if prefs.screensaver.is_some()
            && button(ui, at(o, VALUE_X + 303.0, y + 4.0, 90.0, 22.0), "Reset").clicked()
            && let Some(path) = prefs.screensaver.take()
        {
            store::delete_screensaver(&path);
        }
        y += 36.0;

        label(ui, o, y, 30.0, "Brightness:");
        let painter = ui.painter().clone();
        painter.text(
            at(o, VALUE_X + 1.0, y + 5.0, 19.0, 19.0).center(),
            Align2::CENTER_CENTER,
            icon::SUN_DIM,
            font(17.0),
            TEXT_DIM,
        );
        painter.text(
            at(o, 690.0, y + 5.0, 19.0, 19.0).center(),
            Align2::CENTER_CENTER,
            icon::SUN,
            font(19.0),
            TEXT_DIM,
        );
        let slider = at(o, VALUE_X + 30.0, y + 5.0, 426.0, 20.0);
        let resp = ui
            .scope_builder(egui::UiBuilder::new().max_rect(slider), |ui| {
                ui.spacing_mut().slider_width = slider.width();
                ui.add(egui::Slider::new(&mut prefs.brightness, 0..=100).show_value(false))
            })
            .inner;
        y += 36.0;

        label(ui, o, y, 30.0, "Firmware Version:");
        value(
            ui,
            o,
            VALUE_X + 1.0,
            y,
            30.0,
            device.firmware.as_deref().unwrap_or("Unknown"),
        );
        y += 36.0;
        label(ui, o, y, 30.0, "Serial Number:");
        value(ui, o, VALUE_X + 1.0, y, 30.0, &device.serial);

        if button(ui, at(o, 630.0, 437.0, 80.0, 22.0), "Advanced…").clicked() {
            if self.advanced.is_some() {
                ui.ctx()
                    .send_viewport_cmd_to(advanced_id(), egui::ViewportCommand::Focus);
            }
            self.advanced = Some((device.serial.clone(), device.model));
        }

        if prefs != before {
            prefs.name = prefs.name.trim_start().to_owned();
            *s.settings.device_mut(&device.serial) = prefs;
            // A dragged slider is saved once it is let go.
            if !resp.dragged() {
                s.save_settings();
            }
            drop(s);
            let _ = env.runtime.send(Msg::Settings);
        } else if resp.drag_stopped() {
            s.save_settings();
        }
    }

    // ----- Profiles ------------------------------------------------------

    fn profiles(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        if self
            .pending_profile
            .as_ref()
            .is_some_and(|id| runtime::lock(env.state).profile(id).is_some())
        {
            self.profile = self.pending_profile.take();
            self.page = None;
        }
        let list = at(o, 21.0, 56.0, 220.0, 387.0);
        frame(ui, at(o, 20.0, 55.0, 222.0, 414.0));
        let details = at(o, 262.0, 55.0, 458.0, 414.0);
        frame(ui, details);

        let (profiles, default) = {
            let s = runtime::lock(env.state);
            let profiles: Vec<ProfileRow> = s
                .profiles
                .iter()
                .filter(|p| p.device == env.kind)
                .map(ProfileRow::new)
                .collect();
            // The connected device's default profile, marked in the list.
            let default = env
                .device
                .and_then(|serial| {
                    s.devices
                        .iter()
                        .find(|d| d.serial == serial && d.model.kind == env.kind)
                })
                .and_then(|d| s.settings.device(&d.serial).default_profile);
            (profiles, default)
        };
        if !profiles
            .iter()
            .any(|p| Some(&p.id) == self.profile.as_ref())
        {
            self.profile = profiles.first().map(|p| p.id.clone());
        }
        let page_exists = profiles
            .iter()
            .filter(|p| Some(&p.id) == self.profile.as_ref())
            .flat_map(|p| &p.pages)
            .any(|p| Some(&p.id) == self.page.as_ref());
        if !page_exists {
            self.page = None;
        }

        // The list; double-click a name, press F2 or right-click and pick
        // Rename to rename it. Enter or a click elsewhere keeps the new name,
        // Escape drops it. The caret shows a profile's pages, and the handle
        // on the right moves the profile.
        let mut commit = false;
        let mut cancel = false;
        let mut start: Option<(String, String)> = None;
        let mut reorder = None;
        let mut command = None;
        let mut image_command = None;
        let can_delete = profiles.len() > 1;
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let mut headers = Vec::new();
                    let mut moving = None;
                    let mut dropped = false;
                    for (index, p) in profiles.iter().enumerate() {
                        let id = &p.id;
                        let name = &p.name;
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                        headers.push(rect);
                        let selected = self.profile.as_ref() == Some(id) && self.page.is_none();
                        if selected {
                            ui.painter()
                                .rect_filled(rect, CornerRadius::ZERO, PREF_ROW_SELECTED);
                        } else if resp.hovered() {
                            ui.painter()
                                .rect_filled(rect, CornerRadius::ZERO, LIST_HOVER);
                        }
                        let open = self.expanded.contains(id);
                        let caret = Rect::from_center_size(
                            pos2(rect.left() + 22.0, rect.center().y),
                            Vec2::splat(24.0),
                        );
                        let caret_resp =
                            ui.interact(caret, ui.id().with(("caret", id)), Sense::click());
                        centered_glyph(
                            ui,
                            caret.center(),
                            if open {
                                icon::CARET_DOWN
                            } else {
                                icon::CARET_RIGHT
                            },
                            12.0,
                            if caret_resp.hovered() || selected {
                                TEXT
                            } else {
                                TEXT_DIM
                            },
                        );
                        if caret_resp.clicked() && !self.expanded.remove(id) {
                            self.expanded.insert(id.clone());
                        }
                        let handle = Rect::from_center_size(
                            pos2(rect.right() - 22.0, rect.center().y),
                            Vec2::splat(24.0),
                        );
                        let grip = ui
                            .interact(handle, ui.id().with(("handle", id)), Sense::drag())
                            .on_hover_and_drag_cursor(CursorIcon::Grab);
                        centered_glyph(
                            ui,
                            handle.center(),
                            icon::LIST,
                            15.0,
                            if grip.hovered() || grip.dragged() || selected {
                                TEXT
                            } else {
                                TEXT_DIM
                            },
                        );
                        if grip.dragged() || grip.drag_stopped() {
                            moving = Some(index);
                            dropped = grip.drag_stopped();
                        }
                        match &mut self.renaming {
                            Some(r) if r.id == *id => {
                                // The field's text starts where the name was drawn.
                                let field = Rect::from_x_y_ranges(
                                    rect.left() + 30.0..=handle.left() - 2.0,
                                    rect.shrink(4.0).y_range(),
                                );
                                let mut out = ui
                                    .scope_builder(egui::UiBuilder::new().max_rect(field), |ui| {
                                        egui::TextEdit::singleline(&mut r.text)
                                            .char_limit(MAX_PROFILE_NAME_CHARS)
                                            .font(font(13.0))
                                            .vertical_align(egui::Align::Center)
                                            .margin(vec2(10.0, 0.0))
                                            .min_size(field.size())
                                            .desired_width(field.width())
                                            .show(ui)
                                    })
                                    .inner;
                                if r.fresh {
                                    r.fresh = false;
                                    out.response.request_focus();
                                    let all = egui::text::CCursorRange::select_all(&out.galley);
                                    out.state.cursor.set_char_range(Some(all));
                                    out.state.store(ui.ctx(), out.response.id);
                                } else if out.response.lost_focus() {
                                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                        cancel = true;
                                    } else {
                                        commit = true;
                                    }
                                }
                            }
                            _ => {
                                let mut name_right = handle.left() - 4.0;
                                if default.as_ref() == Some(id) {
                                    let tag = ui.painter().text(
                                        pos2(name_right, rect.center().y),
                                        Align2::RIGHT_CENTER,
                                        "Default",
                                        font(12.0),
                                        if selected { TEXT } else { TEXT_DIM },
                                    );
                                    name_right = tag.left() - 8.0;
                                }
                                let clip =
                                    Rect::from_x_y_ranges(rect.left()..=name_right, rect.y_range());
                                ui.painter().with_clip_rect(clip).text(
                                    pos2(rect.left() + 40.0, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    name,
                                    font(13.0),
                                    TEXT,
                                );
                            }
                        }
                        if resp.clicked() || resp.secondary_clicked() {
                            self.profile = Some(id.clone());
                            self.page = None;
                            self.confirm_delete = None;
                            self.confirm_delete_page = None;
                        }
                        if resp.double_clicked() {
                            start = Some((id.clone(), name.clone()));
                        }
                        resp.context_menu(|ui| {
                            if let Some(pick) = profile_commands(ui, p, can_delete) {
                                command = Some((id.clone(), pick));
                            }
                        });
                        if open {
                            for (number, page) in p.pages.iter().enumerate() {
                                if let Some(choice) =
                                    self.page_row(ui, id, page, number + 1, p.pages.len() > 1)
                                {
                                    image_command = Some((id.clone(), page.id.clone(), choice));
                                }
                            }
                        }
                    }
                    let end = ui.cursor().top();
                    if let (Some(from), Some(pointer)) =
                        (moving, ui.input(|i| i.pointer.latest_pos()))
                    {
                        let to = headers
                            .iter()
                            .position(|r| pointer.y < r.center().y)
                            .unwrap_or(headers.len());
                        let y = headers.get(to).map_or(end, |r| r.top());
                        ui.painter()
                            .hline(ui.max_rect().x_range(), y, Stroke::new(2.0, TEXT));
                        // Near the edges the list scrolls along.
                        let view = ui.clip_rect();
                        let edge = 24.0;
                        if pointer.y < view.top() + edge {
                            ui.scroll_with_delta(vec2(0.0, 4.0));
                        } else if pointer.y > view.bottom() - edge {
                            ui.scroll_with_delta(vec2(0.0, -4.0));
                        }
                        if dropped {
                            reorder = Some((from, to));
                        }
                    }
                });
        });
        if let Some((from, to)) = reorder {
            let ids = profiles.iter().map(|p| p.id.clone()).collect();
            runtime::lock(env.state).reorder_profiles(&moved(ids, from, to));
            let _ = env.runtime.send(Msg::Refresh);
        }
        if commit {
            self.finish_rename(env);
        } else if cancel {
            self.renaming = None;
        }
        if self.renaming.is_none() && ui.input(|i| i.key_pressed(egui::Key::F2)) {
            start = profiles
                .iter()
                .find(|p| Some(&p.id) == self.profile.as_ref())
                .map(|p| (p.id.clone(), p.name.clone()));
        }
        if let Some((id, name)) = start {
            self.finish_rename(env);
            self.renaming = Some(Rename::new(id, name));
        }

        // Add and remove buttons under the list.
        let painter = ui.painter().clone();
        painter.hline(
            list.x_range(),
            list.bottom(),
            Stroke::new(1.0, PREF_TAB_BORDER),
        );
        let plus = at(o, 21.0, 444.0, 24.0, 24.0);
        if icon_button(ui, plus, icon::PLUS, true)
            .on_hover_text("New profile")
            .clicked()
        {
            self.finish_rename(env);
            let id = runtime::lock(env.state).add_profile("New Profile", env.kind);
            self.profile = Some(id.clone());
            self.page = None;
            self.renaming = Some(Rename::new(id, "New Profile".to_owned()));
            self.confirm_delete = None;
        }
        let minus = at(o, 45.0, 444.0, 24.0, 24.0);
        let selected = profiles
            .iter()
            .find(|p| Some(&p.id) == self.profile.as_ref());
        let selected_page = selected.and_then(|p| {
            p.pages
                .iter()
                .find(|page| Some(&page.id) == self.page.as_ref())
        });
        let can_delete_selected = if selected_page.is_some() {
            selected.is_some_and(|p| p.pages.len() > 1)
        } else {
            can_delete
        };
        if icon_button(ui, minus, icon::MINUS, can_delete_selected)
            .on_hover_text(if selected_page.is_some() {
                "Delete page"
            } else {
                "Delete profile"
            })
            .clicked()
        {
            if let (Some(profile), Some(page)) = (selected, selected_page) {
                self.confirm_delete_page = Some((profile.id.clone(), page.id.clone()));
            } else {
                self.confirm_delete = self.profile.clone();
            }
        }
        let options = icon_button(ui, at(o, 69.0, 444.0, 24.0, 24.0), icon::GEAR, true)
            .on_hover_text("Profile options");
        if let Some(profile) = profiles
            .iter()
            .find(|p| Some(&p.id) == self.profile.as_ref())
        {
            egui::Popup::menu(&options).show(|ui| {
                if let Some(page) = selected_page {
                    match page_menu(ui, profile.pages.len() > 1, page.icon.is_some()) {
                        Some(PageCommand::Delete) => {
                            self.confirm_delete_page = Some((profile.id.clone(), page.id.clone()))
                        }
                        Some(PageCommand::Icon(choice)) => {
                            image_command = Some((profile.id.clone(), page.id.clone(), choice))
                        }
                        None => {}
                    }
                } else if let Some(pick) = profile_commands(ui, profile, can_delete) {
                    command = Some((profile.id.clone(), pick));
                }
            });
        }
        if let Some((id, command)) = command {
            self.finish_rename(env);
            self.profile_command(ui.ctx(), env, &id, command);
        }
        if let Some((profile, page, choice)) = image_command {
            self.page_image_command(ui.ctx(), env, &profile, &page, choice);
        }

        // Details of the selected page or profile.
        let Some(profile) = profiles
            .iter()
            .find(|p| Some(&p.id) == self.profile.as_ref())
        else {
            return;
        };
        let page = profile
            .pages
            .iter()
            .position(|p| Some(&p.id) == self.page.as_ref());
        if let Some((index, page)) = page.and_then(|i| Some((i, profile.pages.get(i)?))) {
            if let Some(choice) = page_details(ui, o, env, &profile.id, page, index + 1) {
                self.page_image_command(ui.ctx(), env, &profile.id, &page.id, choice);
            }
            self.page_delete_confirmation(ui, o, env, &profile.id, page);
            return;
        }
        let (id, name, pages) = (
            profile.id.clone(),
            profile.name.clone(),
            profile.pages.len(),
        );
        let x = 272.0;
        painter.text(
            o + vec2(x, 80.0),
            Align2::LEFT_CENTER,
            &name,
            bold(15.0),
            TEXT,
        );
        let count = if pages == 1 {
            "1 page".to_owned()
        } else {
            format!("{pages} pages")
        };
        painter.text(
            o + vec2(x, 102.0),
            Align2::LEFT_CENTER,
            count,
            font(13.0),
            TEXT_DIM,
        );

        let device = env.device.map(str::to_owned);
        let mut s = runtime::lock(env.state);
        let connected = device
            .as_ref()
            .and_then(|serial| s.devices.iter().find(|d| d.serial == *serial).cloned());
        match connected.filter(|d| d.model.kind == env.kind) {
            Some(d) => {
                let current = s.settings.device(&d.serial).default_profile;
                let mut is_default = current.as_deref() == Some(id.as_str());
                if checkbox(
                    ui,
                    at(o, x, 135.0, 260.0, 20.0),
                    &mut is_default,
                    "Make this my default profile",
                )
                .changed()
                {
                    s.settings.device_mut(&d.serial).default_profile =
                        is_default.then(|| id.clone());
                    s.save_settings();
                }
                let note = format!("{} shows this profile when it connects.", s.device_name(&d));
                value_dim(ui, o, x, 165.0, 438.0, &note);
            }
            None => value_dim(
                ui,
                o,
                x,
                135.0,
                438.0,
                "Connect a device to choose its default profile.",
            ),
        }
        drop(s);
        self.applications(ui, o, env, &id);

        if self.confirm_delete.as_ref() == Some(&id) {
            let text = format!("Delete \"{name}\" and all its pages?");
            value(ui, o, x, 400.0, 22.0, &text);
            if button(ui, at(o, x, 428.0, 70.0, 22.0), "Delete").clicked() {
                runtime::lock(env.state).delete_profile(&id);
                self.confirm_delete = None;
                self.profile = None;
                self.renaming = None;
                let _ = env.runtime.send(Msg::Refresh);
            }
            if button(ui, at(o, x + 80.0, 428.0, 70.0, 22.0), "Cancel").clicked() {
                self.confirm_delete = None;
            }
        }
    }

    /// The applications that bring a profile up.
    fn applications(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>, id: &str) {
        let x = 272.0;
        let (rules, on, error) = {
            let s = runtime::lock(env.state);
            (
                s.profile(id)
                    .map_or_else(Vec::new, |p| p.applications.clone()),
                s.settings.switch_by_window,
                s.follower.error().map(str::to_owned),
            )
        };
        let title = "Show this profile while one of these is active:";
        value(ui, o, x, 195.0, 20.0, title);
        let list = at(o, x, 218.0, 438.0, 118.0);
        frame(ui, list);
        let mut remove = None;
        let icons = self
            .app_icons
            .get_or_insert_with(|| app_picker::icons::Cache::new(ui.ctx(), APP_ICON_SIZE));
        icons.receive(ui.ctx());
        ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink(1.0)), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("profile-applications")
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for (index, rule) in rules.iter().enumerate() {
                        let width = ui.available_width();
                        let (rect, row) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
                        if row.hovered() {
                            ui.painter()
                                .rect_filled(rect, CornerRadius::ZERO, LIST_HOVER);
                        }
                        let y = rect.center().y;
                        let center = pos2(rect.left() + 16.0, y);
                        if let Some(picture) = icons.texture(&rule.icon) {
                            paint_page_picture(
                                ui.painter(),
                                picture,
                                center,
                                APP_ICON_SIZE,
                                Color32::WHITE,
                            );
                        } else {
                            centered_glyph(ui, center, icon::APP_WINDOW, 15.0, TEXT_DIM);
                        }
                        let close =
                            Rect::from_center_size(pos2(rect.right() - 16.0, y), Vec2::splat(22.0));
                        let clip =
                            Rect::from_x_y_ranges(rect.left()..=close.left() - 4.0, rect.y_range());
                        ui.painter().with_clip_rect(clip).text(
                            pos2(rect.left() + 32.0, y),
                            Align2::LEFT_CENTER,
                            &rule.name,
                            font(13.0),
                            TEXT,
                        );
                        let button = ui.push_id(index, |ui| icon_button(ui, close, icon::X, true));
                        if button.inner.on_hover_text("Remove").clicked() {
                            remove = Some(index);
                        }
                        row.on_hover_text(&rule.id);
                    }
                });
        });
        if rules.is_empty() {
            value_dim(ui, o, x + 10.0, 226.0, 418.0, "No applications yet.");
        }
        if let Some(index) = remove {
            self.app_note = None;
            edit_applications(env, id, |list| remove_application(list, index));
        }
        let room = rules.len() < MAX_PROFILE_APPLICATIONS;
        if button(ui, at(o, x, 344.0, 130.0, 22.0), "Add Application…").clicked() && room {
            self.app_note = None;
            self.app_picker = Some(Picker::new(ui.ctx(), id.to_owned()));
        }
        let file = button(ui, at(o, x + 140.0, 344.0, 110.0, 22.0), "Choose File…")
            .on_hover_text("For a program the list does not have, such as a game");
        if file.clicked()
            && room
            && let Some(path) = rfd::FileDialog::new().pick_file()
        {
            self.app_note = None;
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let file = path.to_string_lossy();
            let rule = AppRule::new(&name, &file).with_icon(&file);
            edit_applications(env, id, |list| add_application(list, rule));
        }
        if let Some(note) = error.as_ref().or(self.app_note.as_ref()) {
            wrapped(ui, o, x, 372.0, 438.0, note, WARNING);
        } else if !on {
            value_dim(ui, o, x, 372.0, 438.0, "Turned off on the General tab.");
        }
    }

    /// Runs the picker opened on the Profiles tab.
    fn application_picker(&mut self, ctx: &egui::Context, env: &mut Env<'_>) {
        let Some(picker) = &mut self.app_picker else {
            return;
        };
        match picker.show(ctx) {
            app_picker::Selection::Pending => return,
            app_picker::Selection::Cancelled => {}
            app_picker::Selection::Application(app) if app.process.is_empty() => {
                self.app_note = Some(format!(
                    "Deckly cannot tell when {} is active. Use Choose File… with its program.",
                    app.name
                ));
            }
            app_picker::Selection::Application(app) => {
                let rule = AppRule::new(&app.name, &app.process).with_icon(&app.icon);
                edit_applications(env, &picker.context, |list| add_application(list, rule));
            }
        }
        self.app_picker = None;
    }

    /// A page of an open profile in the list; `number` counts from 1.
    fn page_row(
        &mut self,
        ui: &mut Ui,
        profile: &str,
        page: &PageRow,
        number: usize,
        can_delete: bool,
    ) -> Option<ImageChoice> {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        if self.page.as_ref() == Some(&page.id) {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, PREF_ROW_SELECTED);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, LIST_HOVER);
        }
        let y = rect.center().y;
        page_glyph(ui, pos2(rect.left() + 45.0, y), 16.0, page, number);
        ui.painter().with_clip_rect(rect).text(
            pos2(rect.left() + 66.0, y),
            Align2::LEFT_CENTER,
            &page.title,
            font(13.0),
            TEXT,
        );
        if resp.clicked() || resp.secondary_clicked() {
            self.profile = Some(profile.to_owned());
            self.page = Some(page.id.clone());
            self.confirm_delete = None;
            self.confirm_delete_page = None;
        }
        let mut image = None;
        resp.context_menu(|ui| match page_menu(ui, can_delete, page.icon.is_some()) {
            Some(PageCommand::Delete) => {
                self.confirm_delete_page = Some((profile.to_owned(), page.id.clone()))
            }
            Some(PageCommand::Icon(choice)) => image = Some(choice),
            None => {}
        });
        image
    }

    fn page_image_command(
        &mut self,
        ctx: &egui::Context,
        env: &mut Env<'_>,
        profile: &str,
        page: &str,
        choice: ImageChoice,
    ) {
        self.confirm_delete_page = None;
        self.page_icon_editor = None;
        if choice == ImageChoice::Reset {
            edit_page(env, profile, page, |page| page.icon = None);
            return;
        }
        let source = runtime::lock(env.state)
            .profile(profile)
            .and_then(|profile| profile.pages.iter().find(|p| p.id == page))
            .cloned();
        let Some(source) = source else { return };
        let mode = if choice == ImageChoice::Library {
            Mode::Library
        } else {
            Mode::Create
        };
        let mut editor = Editor::new_page(profile, &source, mode);
        if choice != ImageChoice::Upload || editor.upload(ctx) {
            self.page_icon_editor = Some(PageIconEditor {
                profile: profile.to_owned(),
                page: page.to_owned(),
                editor,
            });
        }
    }

    fn page_delete_confirmation(
        &mut self,
        ui: &mut Ui,
        o: Pos2,
        env: &mut Env<'_>,
        profile: &str,
        page: &PageRow,
    ) {
        if self
            .confirm_delete_page
            .as_ref()
            .is_none_or(|(p, id)| p != profile || *id != page.id)
        {
            return;
        }
        let text = format!("Delete \"{}\" and all its actions?", page.title);
        value(ui, o, 272.0, 400.0, 22.0, &text);
        if button(ui, at(o, 272.0, 428.0, 70.0, 22.0), "Delete").clicked() {
            let result = runtime::lock(env.state).delete_page(profile, &page.id);
            match result {
                Ok(neighbor) => {
                    self.page = neighbor;
                    let _ = env.runtime.send(Msg::Refresh);
                }
                Err(error) => {
                    runtime::lock(env.state).notify(format!("Cannot delete page: {error}"))
                }
            }
            self.confirm_delete_page = None;
        }
        if button(ui, at(o, 352.0, 428.0, 70.0, 22.0), "Cancel").clicked() {
            self.confirm_delete_page = None;
        }
    }

    /// Saves the name being typed, if any.
    fn finish_rename(&mut self, env: &mut Env<'_>) {
        if let Some(r) = self.renaming.take() {
            rename_profile(env, &r.id, &r.text);
        }
    }

    fn profile_command(
        &mut self,
        ctx: &egui::Context,
        env: &mut Env<'_>,
        id: &str,
        command: ProfileCommand,
    ) {
        let profile = runtime::lock(env.state).profile(id).cloned();
        let Some(profile) = profile else { return };
        self.confirm_delete = None;
        match command {
            ProfileCommand::Rename => {
                self.page = None;
                self.renaming = Some(Rename::new(profile.id, profile.name));
            }
            ProfileCommand::Delete => {
                self.page = None;
                self.confirm_delete = Some(profile.id);
            }
            ProfileCommand::Duplicate => {
                // A new id makes the source's name count as occupied.
                let mut copy = profile.duplicate(profile.name.clone());
                copy.name = copy.available_name(&runtime::lock(env.state).profiles);
                self.pending_profile = Some(copy.id.clone());
                let state = env.state.clone();
                let runtime = env.runtime.clone();
                transfer::start(&self.job, ctx, "Duplicating profile", move |_| {
                    let result = store::save_profile_copy(copy).map(|copy| {
                        let name = copy.name.clone();
                        let id = copy.id.clone();
                        let mut state = runtime::lock(&state);
                        state.adopt_profiles(vec![copy]);
                        state.place_imported(&[id]);
                        let _ = runtime.send(Msg::Refresh);
                        Summary {
                            title: "Profile duplicated".into(),
                            lines: vec![format!("Created \"{name}\".")],
                        }
                    });
                    finished("Duplicate failed", result)
                });
            }
            ProfileCommand::Export => {
                let file = transfer::clean_file_name(&profile.name)
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "Profile".into());
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Deckly profile", &[backup::PROFILE_EXTENSION])
                    .set_file_name(format!("{file}.{}", backup::PROFILE_EXTENSION))
                    .save_file()
                {
                    transfer::start(&self.job, ctx, "Exporting profile", move |_| {
                        finished("Export failed", backup::export_profile(&path, profile))
                    });
                }
            }
            ProfileCommand::AddPage => {
                let mut state = runtime::lock(env.state);
                let Some(profile) = state.profile_mut(id) else {
                    return;
                };
                let Some(page) = profile.add_page() else {
                    return;
                };
                match store::save_profile(profile) {
                    Ok(()) => {
                        self.page = Some(page);
                        self.expanded.insert(id.to_owned());
                    }
                    Err(error) => {
                        profile.pages.pop();
                        state.notify(format!("Cannot save profile: {error}"));
                    }
                }
                let _ = env.runtime.send(Msg::Refresh);
            }
            ProfileCommand::Import => self.import_profile(ctx, env),
        }
    }

    fn import_profile(&mut self, ctx: &egui::Context, env: &Env<'_>) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Profiles",
                &[
                    backup::PROFILE_EXTENSION,
                    "streamDeckProfile",
                    "streamDeckProfilesBackup",
                ],
            )
            .pick_file()
        {
            self.scan(ctx, env, path, None);
        }
    }

    // ----- Style ---------------------------------------------------------

    fn style(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        let saved = runtime::lock(env.state).settings.title_style;
        let mut style = self.style.unwrap_or(saved);
        label(ui, o, 61.0, 20.0, "Global Title Style:");
        label(ui, o, 95.0, 86.0, "Keys:");
        title_style_controls(ui, o + vec2(219.0, 95.0), &mut style);

        // Preview.
        let preview = at(o, 443.0, 86.0, 78.0, 78.0);
        self.preview(ui, preview, &style, env.renderer);

        value_dim(
            ui,
            o,
            219.0,
            176.0,
            480.0,
            "Keys that set their own title style keep it.",
        );

        self.style = (style != saved).then_some(style);
        let changed = self.style.is_some();
        if ui
            .add_enabled_ui(changed, |ui| {
                button(ui, at(o, 218.0, 223.0, 49.0, 22.0), "Apply")
            })
            .inner
            .clicked()
        {
            let mut s = runtime::lock(env.state);
            s.settings.title_style = style;
            s.save_settings();
            drop(s);
            self.style = None;
            let _ = env.runtime.send(Msg::Settings);
        }
        if ui
            .add_enabled_ui(changed, |ui| {
                button(ui, at(o, 277.0, 223.0, 51.0, 22.0), "Revert")
            })
            .inner
            .clicked()
        {
            self.style = None;
        }

        self.key_press(ui, o, env);
    }

    /// The press effect, saved as soon as it changes.
    fn key_press(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        let mut s = runtime::lock(env.state);
        let before = s.settings.key_press;
        let mut press = before;
        label(ui, o, 275.0, 22.0, "Key Press:");
        toggle(
            ui,
            at(o, 219.0, 278.0, 220.0, 18.0),
            &mut press.shrink,
            "Shrink keys while held",
        );
        let slider = at(o, 219.0, 310.0, 300.0, 20.0);
        let resp = ui
            .add_enabled_ui(press.shrink, |ui| {
                ui.scope_builder(egui::UiBuilder::new().max_rect(slider), |ui| {
                    ui.spacing_mut().slider_width = slider.width();
                    ui.add(
                        egui::Slider::new(&mut press.size, KeyPress::MIN_SIZE..=KeyPress::MAX_SIZE)
                            .show_value(false),
                    )
                })
                .inner
            })
            .inner;
        value(ui, o, 531.0, 309.0, 22.0, &format!("{}%", press.size));
        value_dim(
            ui,
            o,
            219.0,
            340.0,
            480.0,
            "Size of a held key's picture on the device.",
        );
        if press != before {
            s.settings.key_press = press;
            // A dragged slider is saved once it is let go.
            if !resp.dragged() {
                s.save_settings();
            }
            drop(s);
            let _ = env.runtime.send(Msg::Settings);
        } else if resp.drag_stopped() {
            s.save_settings();
        }
    }

    fn preview(&mut self, ui: &mut Ui, rect: Rect, style: &TitleStyle, renderer: &mut Renderer) {
        let mut key = KeyConfig::new(actions::WEBSITE, json!({}));
        key.title = "Preview".into();
        key.title_style = Some(*style);
        let hash = renderer.key_hash(Some(&key), Live::default(), None);
        let ppp = ui.ctx().pixels_per_point();
        let border = KEY_BORDER_WIDTH;
        let px = ((rect.width() - 2.0 * border) * ppp).round() as u32;
        let texture = match &self.preview {
            Some((h, t)) if *h == hash && t.size()[0] == px as usize => t.id(),
            _ => {
                let img = renderer.render(Some(&key), px, Live::default(), None);
                let size = [img.width() as usize, img.height() as usize];
                let color = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                let t = ui
                    .ctx()
                    .load_texture("title-preview", color, TextureOptions::LINEAR);
                let id = t.id();
                self.preview = Some((hash, t));
                id
            }
        };
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(12), KEY_BORDER);
        p.add(
            egui::epaint::RectShape::filled(
                rect.shrink(border),
                CornerRadius::same(9),
                Color32::WHITE,
            )
            .with_texture(texture, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
        );
    }
}

fn finished(failed: &str, result: Result<Summary, String>) -> Job {
    Job::Done(result.unwrap_or_else(|e| Summary {
        title: failed.to_owned(),
        lines: vec![e],
    }))
}

fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("deckly-preferences")
}

fn advanced_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("deckly-advanced-settings")
}

/// Takes the picture finished in the Customize Image window: a screensaver
/// is saved and set for its device, a standby image goes to the device.
fn use_picture(env: &mut Env<'_>, purpose: Purpose, image: RgbaImage) {
    match purpose {
        Purpose::Screensaver(serial) => match store::save_screensaver(&serial, &image) {
            Ok(path) => {
                let mut s = runtime::lock(env.state);
                let old = s
                    .settings
                    .device_mut(&serial)
                    .screensaver
                    .replace(path.clone());
                if let Some(old) = old.filter(|old| *old != path) {
                    store::delete_screensaver(&old);
                }
                s.save_settings();
                drop(s);
                let _ = env.runtime.send(Msg::Settings);
            }
            Err(e) => runtime::lock(env.state).notify(format!("Cannot save screensaver: {e}")),
        },
        Purpose::Standby(serial) => {
            let _ = env.runtime.send(Msg::Standby { serial, image });
        }
    }
}

fn rename_profile(env: &mut Env<'_>, id: &str, text: &str) {
    let name: String = text.trim().chars().take(MAX_PROFILE_NAME_CHARS).collect();
    if name.is_empty() {
        return;
    }
    let mut s = runtime::lock(env.state);
    let Some(p) = s.profile_mut(id).filter(|p| p.name != name) else {
        return;
    };
    p.name = name;
    let result = store::save_profile(p);
    if let Err(e) = result {
        s.notify(format!("Cannot save profile: {e}"));
    }
    drop(s);
    let _ = env.runtime.send(Msg::Refresh);
}

/// The ids with the one at `from` moved to just before the one at `to`.
fn moved(mut ids: Vec<String>, from: usize, to: usize) -> Vec<String> {
    if from < ids.len() {
        let id = ids.remove(from);
        ids.insert(if to > from { to - 1 } else { to }.min(ids.len()), id);
    }
    ids
}

/// The name and icon of a selected page, in place of the profile's details.
fn page_details(
    ui: &mut Ui,
    o: Pos2,
    env: &mut Env<'_>,
    profile: &str,
    page: &PageRow,
    number: usize,
) -> Option<ImageChoice> {
    let (label_x, field_x) = (272.0, 352.0);
    let text = |ui: &Ui, y: f32, text: &str| {
        ui.painter().text(
            o + vec2(label_x, y),
            Align2::LEFT_CENTER,
            text,
            font(13.0),
            TEXT_DIM,
        );
    };
    text(ui, 80.0, "Page Name:");
    let mut name = page.name.clone();
    let field = at(o, field_x, 69.0, 180.0, 22.0);
    let edited = ui.put(
        field,
        egui::TextEdit::singleline(&mut name)
            .char_limit(MAX_PAGE_NAME_CHARS)
            .hint_text(format!("Page {number}"))
            .font(font(13.0))
            .vertical_align(egui::Align::Center)
            .min_size(field.size()),
    );
    if edited.changed() {
        edit_page(env, profile, &page.id, |p| p.name = name);
    }

    text(ui, 126.0, "Change Icon:");
    let area = at(o, field_x, 105.0, 42.0, 42.0);
    let change = ui
        .interact(area, ui.id().with("page-icon"), Sense::click())
        .on_hover_text("Change icon")
        .on_hover_cursor(CursorIcon::PointingHand);
    let fill = if change.hovered() {
        PREF_SEGMENT
    } else {
        PREF_FIELD
    };
    ui.painter().rect_filled(area, CornerRadius::same(2), fill);
    page_glyph(ui, area.center(), 24.0, page, number);
    let mut choice = None;
    egui::Popup::menu(&change).show(|ui| {
        choice = image_editor::image_menu(ui, page.icon.is_some());
    });
    let reset = at(o, field_x + 48.0, 114.0, 24.0, 24.0);
    if icon_button(
        ui,
        reset,
        icon::ARROW_COUNTER_CLOCKWISE,
        page.icon.is_some(),
    )
    .on_hover_text("Reset icon")
    .clicked()
    {
        choice = Some(ImageChoice::Reset);
    }
    choice
}

/// Changes a page and saves its profile.
fn edit_page(env: &mut Env<'_>, profile: &str, page: &str, edit: impl FnOnce(&mut Page)) {
    edit_profile(env, profile, |p| {
        let target = p.pages.iter_mut().find(|q| q.id == page);
        target.map(edit).is_some()
    });
}

/// Adds an application to a list unless it is there or the list is full.
fn add_application(list: &mut Vec<AppRule>, rule: AppRule) -> bool {
    let id = rule.id.to_lowercase();
    let known = list.iter().any(|r| r.id.to_lowercase() == id);
    let add = !known && !id.is_empty() && list.len() < MAX_PROFILE_APPLICATIONS;
    if add {
        list.push(rule);
    }
    add
}

fn remove_application(list: &mut Vec<AppRule>, index: usize) -> bool {
    let exists = index < list.len();
    if exists {
        list.remove(index);
    }
    exists
}

/// Changes the applications of a profile and saves it.
fn edit_applications(
    env: &mut Env<'_>,
    profile: &str,
    edit: impl FnOnce(&mut Vec<AppRule>) -> bool,
) {
    edit_profile(env, profile, |p| edit(&mut p.applications));
    runtime::lock(env.state).follower.rules_changed();
}

/// Changes a profile, when `edit` says it did, and saves it.
fn edit_profile(env: &mut Env<'_>, profile: &str, edit: impl FnOnce(&mut Profile) -> bool) {
    let mut s = runtime::lock(env.state);
    let Some(mut p) = s.profile(profile).cloned() else {
        return;
    };
    if !edit(&mut p) {
        return;
    }
    let result = store::save_profile(&p);
    if let Err(e) = result {
        s.notify(format!("Cannot save profile: {e}"));
    } else if let Some(target) = s.profile_mut(profile) {
        *target = p;
    }
    drop(s);
    let _ = env.runtime.send(Msg::Refresh);
}

/// A page's picture, or a frame with its number.
fn page_glyph(ui: &Ui, center: Pos2, size: f32, page: &PageRow, number: usize) {
    let picture = page
        .icon
        .as_deref()
        .and_then(|path| page_picture(ui.ctx(), path, size));
    if let Some(picture) = picture {
        paint_page_picture(ui.painter(), &picture, center, size, Color32::WHITE);
    } else {
        // Whole device pixels for the frame and its line, or both come out blurry.
        let ppp = ui.ctx().pixels_per_point();
        let area = pixel_rect(center, Vec2::splat(size), ppp);
        let line = (1.5 * ppp).round().max(1.0) / ppp;
        ui.painter().rect_stroke(
            area,
            CornerRadius::same(3),
            Stroke::new(line, TEXT),
            egui::StrokeKind::Inside,
        );
        centered_glyph(ui, center, &number.to_string(), size * 0.6, TEXT);
    }
}

/// Center visible ink, not the surrounding system font line metrics.
fn centered_glyph(ui: &Ui, center: Pos2, glyph: &str, size: f32, color: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(glyph.to_owned(), font(size), color);
    if galley.mesh_bounds.is_finite() {
        // egui snaps text to whole pixels and rounds a half up, which puts a
        // small digit a pixel right of the frame's middle. Round it down here.
        let ppp = ui.ctx().pixels_per_point();
        let origin = (center - galley.mesh_bounds.center().to_vec2()).to_vec2() * ppp;
        let origin = (origin - vec2(0.5, 0.5)).ceil() / ppp;
        ui.painter().galley(origin.to_pos2(), galley, color);
    }
}

/// Page icons keep their aspect ratio in the list, details and page strip.
pub(crate) fn paint_page_picture(
    painter: &egui::Painter,
    picture: &TextureHandle,
    center: Pos2,
    size: f32,
    color: Color32,
) {
    let pixels = picture.size_vec2();
    let fitted = pixels * (size / pixels.max_elem().max(1.0));
    let area = pixel_rect(center, fitted, painter.pixels_per_point());
    let whole = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
    painter.image(picture.id(), area, whole, color);
}

/// A rect of `size` around `center` whose edges sit on device pixels.
fn pixel_rect(center: Pos2, size: Vec2, ppp: f32) -> Rect {
    let size = (size * ppp).round();
    let min = (center.to_vec2() * ppp - size / 2.0).round();
    Rect::from_min_size((min / ppp).to_pos2(), size / ppp)
}

/// The picture at `path` as a texture for an icon `size` points wide. Both
/// windows ask every frame, so it stays in egui's memory, also when it cannot
/// be read.
pub(crate) fn page_picture(ctx: &egui::Context, path: &Path, size: f32) -> Option<TextureHandle> {
    let target = (size * ctx.pixels_per_point()).round().max(1.0) as u32;
    let id = egui::Id::new(("page-picture", path, target));
    if let Some(known) = ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        return known;
    }
    let picture = plugins::decode_picture(path).ok().map(|image| {
        let pixels = shrink_to(image, target);
        ctx.load_texture("page-picture", pixels, TextureOptions::LINEAR)
    });
    ctx.data_mut(|d| d.insert_temp(id, picture.clone()));
    picture
}

/// Scales a larger picture down to `target` pixels on its long side. egui-wgpu
/// has no mipmaps, so a big icon drawn small would alias. Colour is scaled
/// premultiplied so transparent pixels leave no dark edge.
pub(crate) fn shrink_to(mut image: image::RgbaImage, target: u32) -> ColorImage {
    let long = image.width().max(image.height());
    if long > target {
        for px in image.pixels_mut() {
            let a = u32::from(px.0[3]);
            for c in &mut px.0[..3] {
                *c = ((u32::from(*c) * a + 127) / 255) as u8;
            }
        }
        let scale = |n: u32| (n * target / long).max(1);
        image = image::imageops::resize(
            &image,
            scale(image.width()),
            scale(image.height()),
            image::imageops::FilterType::Lanczos3,
        );
        let size = [image.width() as usize, image.height() as usize];
        return ColorImage::from_rgba_premultiplied(size, image.as_raw());
    }
    let size = [image.width() as usize, image.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, image.as_raw())
}

/// Show, position, size and colour controls for a title style, laid out
/// like the official app's: the check box and position buttons on top, the
/// size and colour under them. `o` is the top left of a 214 x 61 area.
pub(crate) fn title_style_controls(ui: &mut Ui, o: Pos2, style: &mut TitleStyle) {
    let painter = ui.painter().clone();
    checkbox(
        ui,
        at(o, 0.0, 2.0, 90.0, 20.0),
        &mut style.show,
        "Show Title",
    );

    // Title position: bottom, middle, top.
    let positions = [TitleAlign::Bottom, TitleAlign::Middle, TitleAlign::Top];
    painter.rect_filled(
        at(o, 124.0, 0.0, 90.0, 24.0),
        CornerRadius::same(3),
        PREF_SEGMENT,
    );
    for (i, align) in positions.into_iter().enumerate() {
        let r = at(o, 124.0 + 30.0 * i as f32, 0.0, 30.0, 24.0);
        let resp = ui
            .interact(r, ui.id().with(("title-align", i)), Sense::click())
            .on_hover_text(align.label());
        if style.align == align {
            painter.rect_filled(r.shrink(1.0), CornerRadius::same(3), PREF_CHECK);
        }
        let color = if style.align == align || resp.hovered() {
            TEXT
        } else {
            TEXT_DIM
        };
        title_position_icon(&painter, r.center(), align, color);
        if resp.clicked() {
            style.align = align;
        }
    }

    // Size in px with up/down buttons.
    let size = at(o, 0.0, 37.0, 48.0, 24.0);
    painter.rect(
        size,
        CornerRadius::same(3),
        PREF_FIELD,
        Stroke::new(1.0, PREF_SEGMENT),
        egui::StrokeKind::Inside,
    );
    painter.text(
        size.center(),
        Align2::CENTER_CENTER,
        format!("{} px", style.size),
        font(13.0),
        TEXT,
    );
    let up = at(o, 56.0, 37.0, 14.0, 12.0);
    let down = at(o, 56.0, 49.0, 14.0, 12.0);
    if small_icon_button(ui, up, icon::CARET_UP, style.size < TitleStyle::MAX_SIZE).clicked() {
        style.size += 1;
    }
    if small_icon_button(
        ui,
        down,
        icon::CARET_DOWN,
        style.size > TitleStyle::MIN_SIZE,
    )
    .clicked()
    {
        style.size -= 1;
    }

    // Colour.
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(at(o, 82.0, 39.0, 30.0, 20.0)),
        |ui| {
            ui.spacing_mut().interact_size = vec2(30.0, 20.0);
            egui::color_picker::color_edit_button_srgb(ui, &mut style.color);
        },
    );
}

/// A key outline with a line where the title sits.
fn title_position_icon(painter: &egui::Painter, center: Pos2, align: TitleAlign, color: Color32) {
    let key = Rect::from_center_size(center, Vec2::splat(14.0));
    painter.rect_stroke(
        key,
        CornerRadius::same(2),
        Stroke::new(1.5, color),
        egui::StrokeKind::Inside,
    );
    let y = match align {
        TitleAlign::Top => key.top() + 3.5,
        TitleAlign::Middle => key.center().y,
        TitleAlign::Bottom => key.bottom() - 3.5,
    };
    painter.line_segment(
        [pos2(key.left() + 3.5, y), pos2(key.right() - 3.5, y)],
        Stroke::new(2.0, color),
    );
}

fn at(o: Pos2, x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(o + vec2(x, y), vec2(w, h))
}

/// A form label, right-aligned in the label column.
fn label(ui: &Ui, o: Pos2, y: f32, h: f32, text: &str) {
    let r = at(o, LABEL_X, y, LABEL_WIDTH, h);
    ui.painter().text(
        pos2(r.right(), r.center().y),
        Align2::RIGHT_CENTER,
        text,
        font(13.0),
        TEXT_DIM,
    );
}

fn value(ui: &Ui, o: Pos2, x: f32, y: f32, h: f32, text: &str) {
    ui.painter().text(
        o + vec2(x, y + h / 2.0),
        Align2::LEFT_CENTER,
        text,
        font(13.0),
        TEXT,
    );
}

/// Dim text wrapped at `w`, starting at the top left.
fn value_dim(ui: &Ui, o: Pos2, x: f32, y: f32, w: f32, text: &str) {
    wrapped(ui, o, x, y, w, text, TEXT_DIM);
}

fn wrapped(ui: &Ui, o: Pos2, x: f32, y: f32, w: f32, text: &str, color: Color32) {
    let galley = ui.painter().layout(text.to_owned(), font(13.0), color, w);
    ui.painter().galley(o + vec2(x, y), galley, color);
}

fn footer(ui: &Ui, o: Pos2, text: &str) {
    ui.painter().text(
        o + vec2(SIZE.x / 2.0, 475.0),
        Align2::CENTER_CENTER,
        text,
        font(12.0),
        TEXT_DIM,
    );
}

/// The framed panel used by the Devices and Profiles tabs.
fn frame(ui: &Ui, rect: Rect) {
    ui.painter().rect(
        rect,
        CornerRadius::ZERO,
        PREF_PANEL,
        Stroke::new(1.0, PREF_PANEL_BORDER),
        egui::StrokeKind::Inside,
    );
}

pub(crate) fn button(ui: &mut Ui, rect: Rect, text: &str) -> egui::Response {
    ui.put(
        rect,
        egui::Button::new(RichText::new(text).font(font(13.0)))
            .wrap_mode(egui::TextWrapMode::Extend)
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, TEXT_DIM))
            .corner_radius(CornerRadius::same(2)),
    )
}

pub(crate) fn icon_button(ui: &mut Ui, rect: Rect, glyph: &str, enabled: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, ui.id().with(("icon-button", glyph)), sense);
    let color = if !enabled {
        TEXT_FAINT
    } else if resp.hovered() {
        TEXT
    } else {
        TEXT_DIM
    };
    centered_glyph(ui, rect.center(), glyph, 14.0, color);
    resp
}

fn small_icon_button(ui: &mut Ui, rect: Rect, glyph: &str, enabled: bool) -> egui::Response {
    let resp = icon_button(ui, rect, glyph, enabled);
    if resp.hovered() && enabled {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), PREF_SEGMENT);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            font(11.0),
            TEXT,
        );
    }
    resp
}

/// A check box drawn like the official one: blue when checked.
pub(crate) fn checkbox(ui: &mut Ui, rect: Rect, value: &mut bool, text: &str) -> egui::Response {
    let mut resp = ui.interact(rect, ui.id().with(("check", text)), Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let p = ui.painter();
    let check = Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), Vec2::splat(16.0));
    if *value {
        p.rect_filled(check, CornerRadius::same(3), PREF_CHECK);
        p.text(
            check.center(),
            Align2::CENTER_CENTER,
            icon::CHECK,
            font(13.0),
            TEXT,
        );
    } else {
        let stroke = if resp.hovered() {
            TEXT_DIM
        } else {
            PREF_SEGMENT
        };
        p.rect(
            check,
            CornerRadius::same(3),
            PREF_FIELD,
            Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
    }
    p.text(
        pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font(13.0),
        TEXT,
    );
    resp
}

/// An on/off switch with a label to its right.
fn toggle(ui: &mut Ui, rect: Rect, value: &mut bool, text: &str) -> egui::Response {
    let mut resp = ui.interact(rect, ui.id().with(("toggle", text)), Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *value);
    let track = Rect::from_min_size(rect.min, vec2(30.0, 16.0)).translate(vec2(0.0, 1.0));
    let fill = PREF_SEGMENT.lerp_to_gamma(PREF_CHECK, t);
    let p = ui.painter();
    p.rect_filled(track, CornerRadius::same(8), fill);
    let x = egui::lerp(track.left() + 8.0..=track.right() - 8.0, t);
    p.circle_filled(pos2(x, track.center().y), 6.0, TEXT);
    p.text(
        pos2(track.right() + 6.0, track.center().y),
        Align2::LEFT_CENTER,
        text,
        font(13.0),
        TEXT,
    );
    resp
}

fn combo(ui: &mut Ui, rect: Rect, id: &str, selected: &str, items: impl FnOnce(&mut Ui)) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.spacing_mut().interact_size.y = rect.height();
        egui::ComboBox::from_id_salt(id)
            .selected_text(RichText::new(selected).font(font(13.0)))
            .width(rect.width())
            .show_ui(ui, items);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_icon_results_only_update_the_unchanged_destination_after_a_successful_save() {
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.add_page();
        let source = profile.pages[0].clone();
        let id = profile.id.clone();
        let edit = PageIconEditor {
            profile: id.clone(),
            page: source.id.clone(),
            editor: Editor::new_page(&id, &source, Mode::Library),
        };
        let mut state = runtime::State::new(vec![profile.clone()], Default::default());
        assert!(edit.matches(&state));
        assert!(
            edit.apply(&mut state, "generated.png".into(), |_| Err(
                "disk full".into()
            ))
            .is_err()
        );
        assert_eq!(state.profile(&id), Some(&profile));
        assert!(
            edit.apply(&mut state, "generated.png".into(), |saved| {
                assert_eq!(
                    saved.pages[0].icon.as_deref(),
                    Some(Path::new("generated.png"))
                );
                Ok(())
            })
            .unwrap()
        );
        assert_eq!(state.profile(&id).unwrap().pages[1], profile.pages[1]);
        assert!(!edit.matches(&state));
        assert!(
            !edit
                .apply(&mut state, "stale.png".into(), |_| panic!(
                    "stale result must not be saved"
                ))
                .unwrap()
        );
        let current = state.profile_mut(&id).unwrap();
        current.pages.remove(0);
        assert!(!edit.matches(&state));
        state.profiles.clear();
        assert!(!edit.matches(&state));
    }

    #[test]
    fn change_icon_offers_the_shared_library_and_creator_without_changing_the_page() {
        for label in ["Icon library…", "Create image…"] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            let profile = Profile::new("Test", DeviceKind::Mk2);
            let state = std::sync::Arc::new(std::sync::Mutex::new(runtime::State::new(
                vec![profile.clone()],
                Default::default(),
            )));
            let (sender, _) = std::sync::mpsc::channel();
            let mut renderer = Renderer::new();
            let mut prefs = Preferences::new();
            prefs.profile = Some(profile.id.clone());
            prefs.page = Some(profile.pages[0].id.clone());
            let mut pass = |prefs: &mut Preferences, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::no_frame().show(ui, |ui| {
                            prefs.profiles(
                                ui,
                                Pos2::ZERO,
                                &mut Env {
                                    state: &state,
                                    runtime: &sender,
                                    renderer: &mut renderer,
                                    device: None,
                                    kind: DeviceKind::Mk2,
                                    host: None,
                                },
                            );
                        });
                    },
                );
                output.textures_delta.clear();
                output.shapes
            };
            pass(&mut prefs, vec![]);
            pass(&mut prefs, vec![]);
            let mut click = |prefs: &mut Preferences, pos| {
                for pressed in [true, false] {
                    pass(
                        prefs,
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: Default::default(),
                            },
                        ],
                    );
                }
                pass(prefs, vec![])
            };
            let shapes = click(&mut prefs, pos2(373.0, 126.0));
            for option in ["Upload image…", "Icon library…", "Create image…"] {
                assert!(shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains(option))));
            }
            let target = shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text().contains(label) => {
                        Some(text.pos + text.galley.size() / 2.0)
                    }
                    _ => None,
                })
                .unwrap();
            click(&mut prefs, target);
            assert!(
                prefs
                    .page_icon_editor
                    .as_ref()
                    .is_some_and(|edit| edit.matches(&runtime::lock(&state)))
            );
            assert_eq!(runtime::lock(&state).profile(&profile.id), Some(&profile));
            prefs.close();
            assert!(prefs.page_icon_editor.is_none());
        }
    }

    /// One pass of a Preferences tab; pointer events come with the click.
    fn tab_shapes(
        ctx: &egui::Context,
        prefs: &mut Preferences,
        state: &Shared,
        tab: Tab,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let (sender, _) = std::sync::mpsc::channel();
        let mut renderer = Renderer::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    let mut env = Env {
                        state,
                        runtime: &sender,
                        renderer: &mut renderer,
                        device: None,
                        kind: DeviceKind::Mk2,
                        host: None,
                    };
                    match tab {
                        Tab::General => prefs.switch_by_window(ui, Pos2::ZERO, &mut env),
                        _ => prefs.profiles(ui, Pos2::ZERO, &mut env),
                    }
                });
            },
        );
        output.textures_delta.clear();
        output.shapes
    }

    fn says(shapes: &[egui::epaint::ClippedShape], text: &str) -> bool {
        shapes.iter().any(
            |shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text().contains(text)),
        )
    }

    fn with_game() -> (Shared, Profile) {
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.applications.push(AppRule::new("Game", "game.exe"));
        let state = runtime::State::new(vec![profile.clone()], Default::default());
        (std::sync::Arc::new(std::sync::Mutex::new(state)), profile)
    }

    #[test]
    fn a_profile_lists_its_applications_and_offers_the_picker() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let (state, profile) = with_game();
        let mut prefs = Preferences::new();
        prefs.profile = Some(profile.id.clone());
        tab_shapes(&ctx, &mut prefs, &state, Tab::Profiles, vec![]);
        let shapes = tab_shapes(&ctx, &mut prefs, &state, Tab::Profiles, vec![]);
        assert!(says(&shapes, "Game"));
        assert!(says(&shapes, "Add Application…"));
        assert!(!says(&shapes, "No applications yet."));
        assert!(!says(&shapes, "Turned off"));
        let pos = pos2(330.0, 355.0);
        for pressed in [true, false] {
            let press = egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            let events = vec![egui::Event::PointerMoved(pos), press];
            tab_shapes(&ctx, &mut prefs, &state, Tab::Profiles, events);
        }
        let picker = prefs.app_picker.as_ref().expect("the picker opens");
        assert_eq!(picker.context, profile.id);
    }

    #[test]
    fn the_rule_list_says_when_it_cannot_work() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let (state, profile) = with_game();
        let mut prefs = Preferences::new();
        prefs.profile = Some(profile.id.clone());
        runtime::lock(&state).settings.switch_by_window = false;
        tab_shapes(&ctx, &mut prefs, &state, Tab::Profiles, vec![]);
        let shapes = tab_shapes(&ctx, &mut prefs, &state, Tab::Profiles, vec![]);
        assert!(says(&shapes, "Turned off on the General tab."));
        let error = "Wayland does not tell applications which window is active.";
        runtime::lock(&state).follower.set_error(error.to_owned());
        let shapes = tab_shapes(&ctx, &mut prefs, &state, Tab::Profiles, vec![]);
        assert!(says(&shapes, error));
        assert!(!says(&shapes, "Turned off"));
    }

    #[test]
    fn the_setting_shows_why_this_system_cannot_follow_windows() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let (state, _) = with_game();
        let mut prefs = Preferences::new();
        tab_shapes(&ctx, &mut prefs, &state, Tab::General, vec![]);
        let shapes = tab_shapes(&ctx, &mut prefs, &state, Tab::General, vec![]);
        assert!(says(&shapes, "Switch profiles by active window"));
        assert!(says(&shapes, "Choose the applications of a profile"));
        let error = "Wayland does not tell applications which window is active.";
        runtime::lock(&state).follower.set_error(error.to_owned());
        let shapes = tab_shapes(&ctx, &mut prefs, &state, Tab::General, vec![]);
        assert!(says(&shapes, error));
        assert!(!says(&shapes, "Choose the applications of a profile"));
    }

    #[test]
    fn an_application_is_listed_once_and_within_the_limit() {
        let mut list = Vec::new();
        assert!(add_application(&mut list, AppRule::new("Game", "game.exe")));
        assert!(!add_application(
            &mut list,
            AppRule::new("Other", "GAME.EXE")
        ));
        assert!(!add_application(&mut list, AppRule::new("Empty", "")));
        assert_eq!(list.len(), 1);
        for i in 1..MAX_PROFILE_APPLICATIONS {
            assert!(add_application(
                &mut list,
                AppRule::new("", &format!("{i}.exe"))
            ));
        }
        assert!(!add_application(&mut list, AppRule::new("", "more.exe")));
        assert!(!remove_application(&mut list, MAX_PROFILE_APPLICATIONS));
        assert!(remove_application(&mut list, 0));
        assert_eq!(list.len(), MAX_PROFILE_APPLICATIONS - 1);
    }

    #[test]
    fn a_big_icon_is_shrunk_to_its_pixels_without_a_dark_edge() {
        // White ink on transparent black: scaling straight alpha would gray the edge.
        let mut icon = image::RgbaImage::from_pixel(64, 32, image::Rgba([0, 0, 0, 0]));
        for x in 16..48 {
            for y in 8..24 {
                icon.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let small = shrink_to(icon.clone(), 16);
        assert_eq!(small.size, [16, 8]);
        assert!(small.pixels.iter().all(|p| p.r() <= p.a()));
        assert_eq!(small.pixels[4 * 16 + 8], Color32::WHITE);
        // A small icon is left as it is.
        assert_eq!(shrink_to(icon, 128).size, [64, 32]);
    }

    #[test]
    fn page_controls_center_visible_glyphs_and_preserve_picture_aspect_ratio() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            ctx.set_pixels_per_point(scale);
            let center = pos2(100.0, 100.0);
            let texture = ctx.load_texture(
                "wide-page-icon",
                ColorImage::filled([40, 20], Color32::WHITE),
                TextureOptions::LINEAR,
            );
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                for glyph in [
                    icon::LIST,
                    icon::CARET_RIGHT,
                    icon::CARET_DOWN,
                    icon::GEAR,
                    "1",
                    "10",
                    "100",
                ] {
                    centered_glyph(ui, center, glyph, 15.0, TEXT);
                }
                paint_page_picture(ui.painter(), &texture, center, 20.0, Color32::WHITE);
            });
            output.textures_delta.clear();
            let mut glyphs = 0;
            let mut pictures = 0;
            for shape in output.shapes {
                match shape.shape {
                    egui::Shape::Text(text) => {
                        let ink = text.galley.mesh_bounds.translate(text.pos.to_vec2());
                        assert!(
                            (ink.center() - center).abs().max_elem() <= 0.5 / scale + 0.01,
                            "scale {scale}: {ink:?}"
                        );
                        glyphs += 1;
                    }
                    egui::Shape::Mesh(mesh) if mesh.texture_id == texture.id() => {
                        let bounds = mesh.calc_bounds();
                        // Edges sit on device pixels, so up to one pixel off.
                        let pixel = 1.0 / scale;
                        assert!((bounds.center() - center).abs().max_elem() <= pixel);
                        assert!((bounds.size() - vec2(20.0, 10.0)).abs().max_elem() <= pixel);
                        let device = bounds.min * scale;
                        assert!((device - device.round()).abs().max_elem() < 1e-3);
                        pictures += 1;
                    }
                    _ => {}
                }
            }
            assert_eq!(glyphs, 7);
            assert_eq!(pictures, 1);
        }
    }

    #[test]
    fn right_click_selects_the_page_and_delete_requires_confirmation() {
        for can_delete in [true, false] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            let mut prefs = Preferences::new();
            prefs.profile = Some("other".into());
            let page = PageRow {
                id: "page".into(),
                name: String::new(),
                title: "Page 2".into(),
                icon: None,
            };
            let pass = |prefs: &mut Preferences, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
                        events,
                        ..Default::default()
                    },
                    |root| {
                        egui::CentralPanel::no_frame().show(root, |ui| {
                            prefs.page_row(ui, "profile", &page, 2, can_delete)
                        });
                    },
                );
                output.textures_delta.clear();
                output
            };
            pass(&mut prefs, vec![]);
            pass(&mut prefs, vec![]);
            let point = pos2(100.0, 16.0);
            let click = |prefs: &mut Preferences, pos, button| {
                pass(
                    prefs,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button,
                            pressed: true,
                            modifiers: Default::default(),
                        },
                    ],
                );
                pass(
                    prefs,
                    vec![egui::Event::PointerButton {
                        pos,
                        button,
                        pressed: false,
                        modifiers: Default::default(),
                    }],
                )
            };
            click(&mut prefs, point, egui::PointerButton::Secondary);
            assert_eq!(prefs.profile.as_deref(), Some("profile"));
            assert_eq!(prefs.page.as_deref(), Some("page"));
            assert!(prefs.confirm_delete_page.is_none());
            let output = pass(&mut prefs, vec![]);
            let delete = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text().contains("Delete Page") => {
                        Some(text.pos + text.galley.rect.center().to_vec2())
                    }
                    _ => None,
                })
                .unwrap();
            click(&mut prefs, delete, egui::PointerButton::Primary);
            assert_eq!(prefs.confirm_delete_page.is_some(), can_delete);
        }
    }

    fn moved_ids(from: usize, to: usize) -> Vec<String> {
        let ids = ["a", "b", "c", "d"].map(str::to_owned).to_vec();
        moved(ids, from, to)
    }

    #[test]
    fn a_profile_lands_before_the_row_it_is_dropped_on() {
        assert_eq!(moved_ids(0, 2), ["b", "a", "c", "d"]);
        assert_eq!(moved_ids(3, 1), ["a", "d", "b", "c"]);
        // Past the last row.
        assert_eq!(moved_ids(1, 4), ["a", "c", "d", "b"]);
        // On itself or just below itself it stays.
        assert_eq!(moved_ids(1, 1), ["a", "b", "c", "d"]);
        assert_eq!(moved_ids(1, 2), ["a", "b", "c", "d"]);
    }
}

//! The Preferences window.
//!
//! A separate window with General, Devices, Profiles and Style tabs. Layout,
//! sizes and colours follow the official app's Preferences window
//! (docs/clean-room/2026-10-26-preferences.md); controls are placed at the
//! measured positions, relative to the window's client area.

use crate::actions;
use crate::backup;
use crate::cropper::{Cropper, Purpose};
use crate::logo;
use crate::os::autostart;
use crate::plugins;
use crate::render::{self, Live, Renderer};
use crate::runtime::{self, Msg, Shared};
use crate::store;
use crate::theme::*;
use crate::transfer::{self, Job, PluginKind, SharedJob, Summary};
use deckly_core::profile::MAX_PAGE_NAME_CHARS;
use deckly_core::settings::{self, MAX_DEVICE_NAME_CHARS, SLEEP_CHOICES};
use deckly_core::{
    DeviceKind, DeviceModel, KeyConfig, KeyPress, Orientation, Page, Profile, TitleAlign,
    TitleStyle,
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
const LABEL_X: f32 = 20.0;
const LABEL_WIDTH: f32 = 182.0;
const VALUE_X: f32 = 224.0;
const MAX_PROFILE_NAME_CHARS: usize = 64;

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
            style: None,
            autostart: None,
            official: None,
            job: SharedJob::default(),
            import_profiles: true,
            import_plugins: true,
            preview: None,
            advanced: None,
            cropper: Cropper::default(),
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
        });
        if !self.open {
            self.advanced = None;
        }
        self.show_advanced(ctx, env);
        if let Some((purpose, image)) = self.cropper.show(ctx) {
            use_picture(env, purpose, image);
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

        label(ui, o, 234.0, 22.0, "About:");
        let text = "Deckly is an independent app. It is not made, approved or \
                    supported by Elgato or Corsair.";
        value_dim(ui, o, VALUE_X, 237.0, 440.0, text);

        footer(ui, o, "Deckly is built with Rust and egui.");
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
                    ui.heading("Import from Stream Deck");
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
                        let any = self.import_profiles || (self.import_plugins && ready > 0);
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
                        ui.painter().text(
                            caret.center(),
                            Align2::CENTER_CENTER,
                            if open {
                                icon::CARET_DOWN
                            } else {
                                icon::CARET_RIGHT
                            },
                            font(12.0),
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
                        ui.painter().text(
                            handle.center(),
                            Align2::CENTER_CENTER,
                            icon::LIST,
                            font(15.0),
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
                        }
                        if resp.double_clicked() {
                            start = Some((id.clone(), name.clone()));
                        }
                        resp.context_menu(|ui| {
                            if ui
                                .button(format!("{}  Rename", icon::PENCIL_SIMPLE))
                                .clicked()
                            {
                                start = Some((id.clone(), name.clone()));
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    can_delete,
                                    egui::Button::new(format!("{}  Delete", icon::TRASH)),
                                )
                                .clicked()
                            {
                                self.confirm_delete = Some(id.clone());
                                ui.close();
                            }
                        });
                        if open {
                            for (number, page) in p.pages.iter().enumerate() {
                                self.page_row(ui, id, page, number + 1);
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
        if icon_button(ui, minus, icon::MINUS, can_delete && self.page.is_none())
            .on_hover_text("Delete profile")
            .clicked()
        {
            self.confirm_delete = self.profile.clone();
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
            page_details(ui, o, env, &profile.id, page, index + 1);
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

    /// A page of an open profile in the list; `number` counts from 1.
    fn page_row(&mut self, ui: &mut Ui, profile: &str, page: &PageRow, number: usize) {
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
        if resp.clicked() {
            self.profile = Some(profile.to_owned());
            self.page = Some(page.id.clone());
            self.confirm_delete = None;
        }
    }

    /// Saves the name being typed, if any.
    fn finish_rename(&mut self, env: &mut Env<'_>) {
        if let Some(r) = self.renaming.take() {
            rename_profile(env, &r.id, &r.text);
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
) {
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
    if change.clicked()
        && let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["svg", "png", "jpg", "jpeg"])
            .pick_file()
    {
        edit_page(env, profile, &page.id, |p| p.icon = Some(path));
    }
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
        edit_page(env, profile, &page.id, |p| p.icon = None);
    }
}

/// Changes a page and saves its profile.
fn edit_page(env: &mut Env<'_>, profile: &str, page: &str, edit: impl FnOnce(&mut Page)) {
    let mut s = runtime::lock(env.state);
    let Some(p) = s.profile_mut(profile) else {
        return;
    };
    let Some(target) = p.pages.iter_mut().find(|q| q.id == page) else {
        return;
    };
    edit(target);
    let result = store::save_profile(p);
    if let Err(e) = result {
        s.notify(format!("Cannot save profile: {e}"));
    }
    drop(s);
    let _ = env.runtime.send(Msg::Refresh);
}

/// A page's picture, or a frame with its number.
fn page_glyph(ui: &Ui, center: Pos2, size: f32, page: &PageRow, number: usize) {
    let area = Rect::from_center_size(center, Vec2::splat(size));
    let picture = page
        .icon
        .as_deref()
        .and_then(|path| page_picture(ui.ctx(), path));
    if let Some(picture) = picture {
        let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter()
            .image(picture.id(), area, whole, Color32::WHITE);
    } else {
        ui.painter().rect_stroke(
            area,
            CornerRadius::same(3),
            Stroke::new(1.5, TEXT),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            number.to_string(),
            font(size * 0.6),
            TEXT,
        );
    }
}

/// The picture at `path` as a texture. Both windows ask every frame, so it
/// stays in egui's memory, also when it cannot be read.
pub(crate) fn page_picture(ctx: &egui::Context, path: &Path) -> Option<TextureHandle> {
    let id = egui::Id::new(("page-picture", path));
    if let Some(known) = ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        return known;
    }
    let picture = plugins::decode_picture(path).ok().map(|image| {
        let size = [image.width() as usize, image.height() as usize];
        let pixels = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
        ctx.load_texture("page-picture", pixels, TextureOptions::LINEAR)
    });
    ctx.data_mut(|d| d.insert_temp(id, picture.clone()));
    picture
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
    let galley = ui
        .painter()
        .layout(text.to_owned(), font(13.0), TEXT_DIM, w);
    ui.painter().galley(o + vec2(x, y), galley, TEXT_DIM);
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
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        font(14.0),
        color,
    );
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

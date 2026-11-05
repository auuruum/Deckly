//! The same image workflow on every desktop: native file selection, an
//! offline Phosphor catalog and a small layered creator, all drawn by egui.

mod composition;
mod history;

use crate::history::Direction;
use crate::store;
use crate::theme::*;
use composition::{Composition, Content, Fonts, Layer, MAX_LAYERS, MAX_TEXT, SIZE};
use deckly_core::{KeyConfig, Page};
use eframe::egui::{
    self, Color32, ColorImage, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui,
    pos2, vec2,
};
use egui_phosphor::regular as icon;
use history::{Group, History, Snapshot};
use image::RgbaImage;
use std::path::PathBuf;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, TryRecvError},
};

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Library,
    Create,
    Video,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageChoice {
    Upload,
    Video,
    Library,
    Create,
    Reset,
}

/// One image menu for keys and pages, backed by the same editor.
pub fn image_menu(ui: &mut Ui, has_image: bool, allow_video: bool) -> Option<ImageChoice> {
    let mut choice = None;
    for (glyph, label, pick) in [
        (icon::UPLOAD_SIMPLE, "Upload image…", ImageChoice::Upload),
        (icon::VIDEO_CAMERA, "Import video…", ImageChoice::Video),
        (icon::SQUARES_FOUR, "Icon library…", ImageChoice::Library),
        (icon::PAINT_BRUSH, "Create image…", ImageChoice::Create),
    ] {
        if pick == ImageChoice::Video && !allow_video {
            continue;
        }
        if ui.button(format!("{glyph}  {label}")).clicked() {
            choice = Some(pick);
            ui.close();
        }
    }
    ui.separator();
    if ui
        .add_enabled(has_image, egui::Button::new("Reset to default"))
        .clicked()
    {
        choice = Some(ImageChoice::Reset);
        ui.close();
    }
    choice
}

#[derive(Debug, PartialEq)]
enum Original {
    Key(KeyConfig),
    Page { profile: String, page: Page },
}

impl Original {
    fn is_page(&self) -> bool {
        matches!(self, Self::Page { .. })
    }

    fn image(&self) -> Option<PathBuf> {
        match self {
            Self::Key(key) => key.image.clone(),
            Self::Page { page, .. } => page.icon.clone(),
        }
    }
}

enum Work {
    Loaded(String, RgbaImage),
    /// A key picture that moves; it is kept as its file.
    Animated(PathBuf),
    Saved(PathBuf),
}

pub enum Selection {
    Pending,
    Cancelled,
    Image(PathBuf),
}

pub struct Editor {
    pub context: String,
    original: Original,
    mode: Mode,
    query: String,
    logos_only: bool,
    selected_icon: Option<usize>,
    adding_icon: bool,
    draft: Composition,
    selected_layer: Option<usize>,
    fonts: Result<Fonts, String>,
    preview: Option<TextureHandle>,
    dirty: bool,
    error: Option<String>,
    receiver: Option<Receiver<Result<Work, String>>>,
    saving: bool,
    focus_search: bool,
    history: History,
    gesture: u64,
}

impl Editor {
    pub fn new(context: String, key: &KeyConfig, mode: Mode) -> Self {
        Self::with_original(context, Original::Key(key.clone()), mode)
    }

    pub fn new_page(profile: &str, page: &Page, mode: Mode) -> Self {
        Self::with_original(
            format!("page-icon/{profile}/{}", page.id),
            Original::Page {
                profile: profile.to_owned(),
                page: page.clone(),
            },
            mode,
        )
    }

    fn with_original(context: String, original: Original, mode: Mode) -> Self {
        let transparent = original.is_page();
        Self {
            context,
            original,
            mode,
            query: String::new(),
            logos_only: false,
            selected_icon: None,
            adding_icon: false,
            draft: Composition {
                transparent,
                ..Default::default()
            },
            selected_layer: None,
            fonts: Fonts::new(),
            preview: None,
            dirty: true,
            error: None,
            receiver: None,
            saving: false,
            focus_search: true,
            history: History::default(),
            gesture: 0,
        }
    }

    /// A modal never applies a result to a different or replaced action.
    pub fn matches(&self, context: &str, key: &KeyConfig) -> bool {
        self.context == context
            && matches!(&self.original, Original::Key(original) if original == key)
    }

    pub fn matches_page(&self, profile: &str, page: &Page) -> bool {
        matches!(&self.original, Original::Page { profile: owner, page: original } if owner == profile && original == page)
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        work: impl FnOnce() -> Result<Work, String> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::sync_channel(1);
        let repaint = ctx.clone();
        match std::thread::Builder::new()
            .name("deckly-key-image".into())
            .spawn(move || {
                let _ = tx.send(work());
                repaint.request_repaint();
            }) {
            Ok(_) => {
                self.receiver = Some(rx);
                self.error = None;
            }
            Err(error) => {
                self.error = Some(format!("Cannot start image processing: {error}"));
                self.saving = false;
            }
        }
    }

    pub fn upload(&mut self, ctx: &egui::Context) -> bool {
        if self.receiver.is_some() || self.draft.layers.len() >= MAX_LAYERS {
            return false;
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Images",
                &["svg", "png", "apng", "jpg", "jpeg", "webp", "gif", "bmp"],
            )
            .pick_file()
        {
            self.load(ctx, path);
            return true;
        }
        false
    }

    pub fn upload_video(&mut self, ctx: &egui::Context, profile: &str) -> bool {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import a video for this key")
            .add_filter("Videos", crate::video::EXTENSIONS)
            .pick_file()
        else {
            return false;
        };
        let profile = profile.to_owned();
        self.mode = Mode::Video;
        self.saving = true;
        self.start(ctx, move || {
            crate::video::import(&path, &profile).map(Work::Saved)
        });
        true
    }

    fn load(&mut self, ctx: &egui::Context, path: PathBuf) {
        // Keys play animations; page icons and layers use the first frame.
        let animate = !self.original.is_page();
        self.start(ctx, move || {
            if animate && crate::animation::load(&path)?.is_some() {
                return Ok(Work::Animated(path));
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().chars().take(80).collect())
                .unwrap_or_else(|| "Picture".into());
            composition::load_picture(&path).map(|image| Work::Loaded(name, image))
        });
    }

    fn receive(&mut self, ctx: &egui::Context, profile: &str) -> Option<PathBuf> {
        let receiver = self.receiver.as_ref()?;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("Image processing stopped. Please try again.".into())
            }
        };
        self.receiver = None;
        self.saving = false;
        ctx.request_repaint();
        match result {
            Ok(Work::Saved(path)) => return Some(path),
            Ok(Work::Animated(source)) => {
                let profile = profile.to_owned();
                self.saving = true;
                self.start(ctx, move || {
                    store::save_key_animation(&profile, &source).map(Work::Saved)
                });
            }
            Ok(Work::Loaded(name, image)) => {
                let mut layer = Layer::new(Content::Picture(name, Arc::new(image)));
                if self.draft.layers.is_empty() {
                    layer.size = 1.0;
                }
                self.add(layer);
                self.mode = Mode::Create;
            }
            Err(error) => self.error = Some(error),
        }
        None
    }

    fn add(&mut self, layer: Layer) {
        match self.draft.add(layer) {
            Ok(index) => {
                self.selected_layer = Some(index);
                self.dirty = true;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn render(&self) -> Result<RgbaImage, String> {
        self.draft
            .render(self.fonts.as_ref().map_err(Clone::clone)?)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            draft: self.draft.clone(),
            selected_layer: self.selected_layer,
            selected_icon: self.selected_icon,
        }
    }

    fn restore(&mut self, direction: Direction) {
        if let Some(snapshot) = self.history.step(direction) {
            self.draft = snapshot.draft;
            self.selected_layer = snapshot.selected_layer;
            self.selected_icon = snapshot.selected_icon;
            self.adding_icon = false;
            self.dirty = true;
        }
    }

    fn update_preview(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        match self.render() {
            Ok(image) => {
                self.error = None;
                let image = ColorImage::from_rgba_unmultiplied([SIZE as usize; 2], image.as_raw());
                match &mut self.preview {
                    Some(texture) => texture.set(image, TextureOptions::LINEAR),
                    None => {
                        self.preview =
                            Some(ctx.load_texture("image-creator", image, TextureOptions::LINEAR))
                    }
                }
            }
            Err(error) => {
                self.preview = None;
                self.error = Some(error);
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, profile: &str) -> Selection {
        if self.mode == Mode::Video {
            return self.show_video(ctx, profile);
        }
        let before = self.snapshot();
        let mut cancel = false;
        let mut apply = false;
        let busy = self.receiver.is_some();
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.gesture = self.gesture.wrapping_add(1);
        }
        let searching = (self.mode == Mode::Library || self.adding_icon) && ctx.text_edit_focused();
        let mut history_action = crate::history::shortcut(ctx, !busy && !searching);
        let modal_id = egui::Id::new(("image-editor", &self.context));
        let modal = egui::Modal::new(modal_id).show(ctx, |ui| {
            ui.set_width(684.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::IMAGE).size(23.0));
                ui.heading(if self.original.is_page() {
                    "Page icon"
                } else {
                    "Key image"
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (direction, glyph, label, enabled, key, shift) in [
                        (
                            Direction::Redo,
                            icon::ARROW_CLOCKWISE,
                            "Redo",
                            self.history.can_redo(),
                            if matches!(ctx.os(), egui::os::OperatingSystem::Mac) {
                                egui::Key::Z
                            } else {
                                egui::Key::Y
                            },
                            matches!(ctx.os(), egui::os::OperatingSystem::Mac),
                        ),
                        (
                            Direction::Undo,
                            icon::ARROW_COUNTER_CLOCKWISE,
                            "Undo",
                            self.history.can_undo(),
                            egui::Key::Z,
                            false,
                        ),
                    ] {
                        let modifiers = egui::Modifiers {
                            shift,
                            ..egui::Modifiers::COMMAND
                        };
                        let shortcut =
                            ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers, key));
                        if ui
                            .add_enabled(
                                !busy && enabled,
                                egui::Button::new(format!("{glyph}  {label}")),
                            )
                            .on_hover_text(format!("{label} ({shortcut})"))
                            .clicked()
                        {
                            history_action = Some(direction);
                        }
                    }
                });
            });
            ui.add_space(8.0);
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    for (mode, label) in [
                        (Mode::Library, "Icon library"),
                        (Mode::Create, "Create image"),
                    ] {
                        if ui.selectable_label(self.mode == mode, label).clicked() {
                            self.mode = mode;
                            self.adding_icon = false;
                        }
                    }
                });
                ui.separator();
                if self.mode == Mode::Library {
                    self.library(ui);
                } else if self.adding_icon {
                    if ui
                        .button(format!("{}  Back to image", icon::ARROW_LEFT))
                        .clicked()
                    {
                        self.adding_icon = false;
                    }
                    self.library(ui);
                } else {
                    self.creator(ui);
                }
            });
            ui.add_space(8.0);
            if let Some(error) = &self.error {
                ui.label(RichText::new(error).color(WARNING));
            }
            ui.separator();
            ui.horizontal(|ui| {
                cancel = ui
                    .add_enabled(!self.saving, egui::Button::new("Cancel"))
                    .clicked();
                if busy {
                    ui.spinner();
                    ui.label(if self.saving {
                        "Saving image…"
                    } else {
                        "Loading image…"
                    });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let valid = !busy
                        && self.preview.is_some()
                        && (self.mode == Mode::Create || self.selected_icon.is_some())
                        && !self.adding_icon;
                    apply = ui
                        .add_enabled(
                            valid,
                            egui::Button::new(if self.original.is_page() {
                                "Set icon"
                            } else {
                                "Set image"
                            }),
                        )
                        .clicked();
                    ui.label(
                        RichText::new(if self.original.is_page() {
                            "Changes apply only to this page."
                        } else {
                            "Changes apply only to this key."
                        })
                        .color(TEXT_DIM),
                    );
                });
            });
        });
        if let Some(path) = self.receive(ctx, profile) {
            return Selection::Image(path);
        }
        let group = if ctx.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
            Some(Group::Pointer(self.gesture))
        } else if ctx.text_edit_focused() {
            ctx.memory(|m| m.focused())
                .map(|field| Group::Typing(field, self.selected_layer))
        } else {
            None
        };
        if self.history.record(before, self.snapshot(), group) {
            ctx.request_repaint();
        }
        if let Some(direction) = history_action {
            self.restore(direction);
        }
        if apply {
            match self.render() {
                Ok(image) => {
                    let profile = profile.to_owned();
                    self.saving = true;
                    self.start(ctx, move || {
                        store::save_key_image(&profile, &image).map(Work::Saved)
                    });
                }
                Err(error) => self.error = Some(error),
            }
        }
        if self.dirty {
            ctx.request_repaint();
        }
        if cancel || (!self.saving && modal.should_close()) {
            Selection::Cancelled
        } else {
            Selection::Pending
        }
    }

    fn show_video(&mut self, ctx: &egui::Context, profile: &str) -> Selection {
        if let Some(path) = self.receive(ctx, profile) {
            return Selection::Image(path);
        }
        let modal =
            egui::Modal::new(egui::Id::new(("video-import", &self.context))).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.heading("Import video");
                if let Some(error) = &self.error {
                    ui.label(RichText::new(error).color(WARNING));
                    ui.button("Close").clicked()
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Converting video…");
                    });
                    false
                }
            });
        if !self.saving && (modal.inner || modal.should_close()) {
            Selection::Cancelled
        } else {
            Selection::Pending
        }
    }

    fn library(&mut self, ui: &mut Ui) {
        let mut reset_scroll = false;
        ui.horizontal(|ui| {
            ui.label(icon::MAGNIFYING_GLASS);
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search icons: music, camera, folder…")
                    .desired_width(440.0)
                    .char_limit(100),
            );
            if std::mem::take(&mut self.focus_search) {
                search.request_focus();
            }
            reset_scroll = search.changed();
            reset_scroll |= ui.checkbox(&mut self.logos_only, "Logos only").changed();
        });
        let matches = search_icons(&self.query, self.logos_only);
        ui.label(
            RichText::new(format!(
                "{} icons · Phosphor · available offline",
                matches.len()
            ))
            .small()
            .color(TEXT_DIM),
        );
        let columns = 8;
        let side = 76.0;
        let spacing = ui.spacing().item_spacing;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("key-icon-library")
            .min_scrolled_height(240.0)
            .max_height(240.0)
            .auto_shrink([false, false]);
        if reset_scroll {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show_rows(ui, side, matches.len().div_ceil(columns), |ui, rows| {
            for row in rows {
                ui.horizontal(|ui| {
                    for column in 0..columns {
                        let Some(&index) = matches.get(row * columns + column) else {
                            break;
                        };
                        let Some(&(name, glyph)) = icon::ICONS.get(index) else {
                            continue;
                        };
                        let (rect, response) =
                            ui.allocate_exact_size(vec2(side, side), Sense::click());
                        let selected = self.selected_icon == Some(index);
                        ui.painter().rect_filled(
                            rect.shrink(1.0),
                            5.0,
                            if selected {
                                ACCENT
                            } else if response.hovered() {
                                LIST_HOVER
                            } else {
                                LIST
                            },
                        );
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            glyph,
                            font(36.0),
                            TEXT,
                        );
                        if response.clicked() {
                            self.selected_icon = Some(index);
                            if self.adding_icon {
                                self.add(Layer::new(Content::Icon(name, glyph)));
                                self.adding_icon = false;
                            } else {
                                self.draft.layers.clear();
                                self.add(Layer::new(Content::Icon(name, glyph)));
                            }
                        }
                        response.on_hover_text(display_name(name));
                    }
                });
            }
        });
        ui.add_space(spacing.y);
        ui.horizontal(|ui| {
            self.preview(ui, 88.0, false);
            ui.vertical(|ui| {
                let name = self
                    .selected_icon
                    .and_then(|i| icon::ICONS.get(i))
                    .map(|(name, _)| display_name(name));
                ui.label(name.unwrap_or_else(|| "Choose an icon above".into()));
                if !self.adding_icon {
                    ui.horizontal(|ui| {
                        self.background_control(ui);
                        if let Some(layer) = self.draft.layers.first_mut() {
                            ui.label("Icon");
                            self.dirty |= ui.color_edit_button_srgb(&mut layer.color).changed();
                        }
                    });
                    if ui
                        .add_enabled(
                            self.selected_icon.is_some(),
                            egui::Button::new("Customize in creator"),
                        )
                        .clicked()
                    {
                        self.mode = Mode::Create;
                    }
                }
            });
        });
    }

    fn preview(&mut self, ui: &mut Ui, side: f32, draggable: bool) {
        self.update_preview(ui.ctx());
        let sense = if draggable {
            Sense::drag()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(vec2(side, side), sense);
        if let Some(texture) = &self.preview {
            ui.painter().image(
                texture.id(),
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        ui.painter().rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, KEY_BORDER),
            egui::StrokeKind::Outside,
        );
        if draggable {
            if let Some(layer) = self
                .selected_layer
                .and_then(|i| self.draft.layers.get_mut(i))
            {
                let centre = rect.min + vec2(layer.x, layer.y) * side;
                let selection = Rect::from_center_size(centre, vec2(layer.size, layer.size) * side);
                ui.painter().with_clip_rect(rect).rect_stroke(
                    selection,
                    0.0,
                    Stroke::new(1.0, ACCENT),
                    egui::StrokeKind::Inside,
                );
                if response.dragged() {
                    // A drag starts on press. The pointer may have travelled
                    // from a control to the canvas in this same frame; only
                    // motion after the press belongs to the gesture.
                    let delta = if response.drag_started() {
                        ui.input(|i| {
                            i.pointer
                                .interact_pos()
                                .zip(i.pointer.press_origin())
                                .map(|(pos, origin)| pos - origin)
                                .unwrap_or_default()
                        })
                    } else {
                        response.drag_delta()
                    } / side;
                    layer.x = (layer.x + delta.x).clamp(0.0, 1.0);
                    layer.y = (layer.y + delta.y).clamp(0.0, 1.0);
                    self.dirty = true;
                }
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        // Controls below the preview change it later in this pass.
        if self.dirty {
            ui.ctx().request_repaint();
        }
    }

    fn background_control(&mut self, ui: &mut Ui) {
        ui.label("Background");
        ui.add_enabled_ui(!self.draft.transparent, |ui| {
            self.dirty |= ui
                .color_edit_button_srgb(&mut self.draft.background)
                .changed();
        });
        self.dirty |= ui
            .checkbox(&mut self.draft.transparent, "Transparent")
            .changed();
    }

    fn creator(&mut self, ui: &mut Ui) {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(270.0);
                self.preview(ui, 260.0, true);
                ui.label(
                    RichText::new("Drag to move the selected layer.")
                        .small()
                        .color(TEXT_DIM),
                );
                ui.label(RichText::new("PNG · 288 × 288 px").small().color(TEXT_DIM));
                ui.horizontal(|ui| {
                    self.background_control(ui);
                });
                if let Some(path) = self.original.image()
                    && ui
                        .add_enabled(
                            self.draft.layers.len() < MAX_LAYERS,
                            egui::Button::new(if self.original.is_page() {
                                "Add current page icon"
                            } else {
                                "Add current key image"
                            }),
                        )
                        .clicked()
                {
                    self.load(ui.ctx(), path);
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("Add layer").strong());
                ui.add_enabled_ui(self.draft.layers.len() < MAX_LAYERS, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(format!("{}  Add text", icon::PLUS)).clicked() {
                            self.add(Layer::new(Content::Text("Text".into())));
                        }
                        if ui.button(format!("{}  Add icon", icon::PLUS)).clicked() {
                            self.adding_icon = true;
                            self.focus_search = true;
                        }
                        if ui.button(format!("{}  Add image…", icon::PLUS)).clicked() {
                            self.upload(ui.ctx());
                        }
                    });
                });
                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(LIST)
                    .stroke(Stroke::new(1.0, KEY_BORDER))
                    .corner_radius(6)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Layers").strong());
                            ui.label(
                                RichText::new(format!(
                                    "{} / {MAX_LAYERS}",
                                    self.draft.layers.len()
                                ))
                                .color(TEXT_DIM),
                            );
                        });
                        ui.label(
                            RichText::new("Top layer appears in front")
                                .small()
                                .color(TEXT_DIM),
                        );
                        egui::ScrollArea::vertical()
                            .id_salt("creator-layers")
                            .min_scrolled_height(94.0)
                            .max_height(94.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (i, layer) in self.draft.layers.iter().enumerate().rev() {
                                    let (glyph, kind) = match &layer.content {
                                        Content::Text(_) => (icon::TEXT_T, "Text"),
                                        Content::Icon(..) => (icon::SQUARES_FOUR, "Icon"),
                                        Content::Picture(..) => (icon::IMAGE, "Image"),
                                    };
                                    let name: String = layer
                                        .name()
                                        .chars()
                                        .take(32)
                                        .map(|c| if c.is_whitespace() { ' ' } else { c })
                                        .collect();
                                    let label = format!("{glyph}  {kind} · {name}");
                                    if ui
                                        .add_sized(
                                            vec2(ui.available_width(), 26.0),
                                            egui::Button::selectable(
                                                self.selected_layer == Some(i),
                                                label,
                                            )
                                            .truncate(),
                                        )
                                        .clicked()
                                    {
                                        self.selected_layer = Some(i);
                                    }
                                }
                                if self.draft.layers.is_empty() {
                                    ui.label(RichText::new("No layers yet").color(TEXT_DIM));
                                }
                            });
                    });
                ui.add_space(6.0);
                ui.label(RichText::new("Edit selected layer").strong());
                self.layer_controls(ui);
            });
        });
        if self.dirty {
            ui.ctx().request_repaint();
        }
    }

    fn layer_controls(&mut self, ui: &mut Ui) {
        let Some(index) = self.selected_layer else {
            ui.label(RichText::new("Select a layer to edit it.").color(TEXT_DIM));
            return;
        };
        let Some(layer) = self.draft.layers.get_mut(index) else {
            return;
        };
        if let Content::Text(text) = &mut layer.content {
            self.dirty |= ui
                .add(
                    egui::TextEdit::multiline(text)
                        .desired_width(350.0)
                        .desired_rows(2)
                        .char_limit(MAX_TEXT)
                        .hint_text("Text (up to 4 lines)"),
                )
                .changed();
        }
        if !matches!(layer.content, Content::Picture(..)) {
            ui.horizontal(|ui| {
                ui.label("Color");
                self.dirty |= ui.color_edit_button_srgb(&mut layer.color).changed();
            });
        }
        self.dirty |= ui
            .add(egui::Slider::new(&mut layer.size, 0.05..=1.5).text("Size"))
            .changed();
        ui.horizontal(|ui| {
            self.dirty |= ui
                .add(
                    egui::DragValue::new(&mut layer.x)
                        .range(0.0..=1.0)
                        .speed(0.005)
                        .prefix("X "),
                )
                .changed();
            self.dirty |= ui
                .add(
                    egui::DragValue::new(&mut layer.y)
                        .range(0.0..=1.0)
                        .speed(0.005)
                        .prefix("Y "),
                )
                .changed();
            if ui.button("Center").clicked() {
                layer.x = 0.5;
                layer.y = 0.5;
                self.dirty = true;
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    index > 0,
                    egui::Button::new(format!("{} Back", icon::ARROW_DOWN)),
                )
                .clicked()
            {
                self.draft.layers.swap(index, index - 1);
                self.selected_layer = Some(index - 1);
                self.dirty = true;
            }
            if ui
                .add_enabled(
                    index + 1 < self.draft.layers.len(),
                    egui::Button::new(format!("{} Front", icon::ARROW_UP)),
                )
                .clicked()
            {
                self.draft.layers.swap(index, index + 1);
                self.selected_layer = Some(index + 1);
                self.dirty = true;
            }
            if ui
                .button(icon::TRASH)
                .on_hover_text("Remove layer")
                .clicked()
            {
                self.draft.layers.remove(index);
                self.selected_layer = self.draft.layers.len().checked_sub(1);
                self.dirty = true;
            }
        });
    }
}

fn search_icons(query: &str, logos_only: bool) -> Vec<usize> {
    let query = query.to_ascii_uppercase().replace(['-', '_'], " ");
    icon::ICONS
        .iter()
        .enumerate()
        .filter_map(|(index, (name, _))| {
            let name = name.replace('_', " ");
            ((!logos_only || name.contains("LOGO"))
                && query.split_whitespace().all(|word| name.contains(word)))
            .then_some(index)
        })
        .collect()
}

fn display_name(name: &str) -> String {
    name.to_ascii_lowercase().replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_icon_editor_uses_page_labels_transparency_and_guards_its_destination() {
        let mut page = Page::new();
        page.icon = Some("existing.svg".into());
        for mode in [Mode::Library, Mode::Create] {
            let mut editor = Editor::new_page("profile", &page, mode);
            assert!(editor.matches_page("profile", &page));
            assert!(!editor.matches_page("other", &page));
            let mut replaced = page.clone();
            replaced.icon = Some("new.png".into());
            assert!(!editor.matches_page("profile", &replaced));
            assert!(!editor.matches(
                "page.0",
                &KeyConfig::new("com.example.a", serde_json::json!({}))
            ));
            assert!(editor.draft.transparent);
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            pass(&mut editor, &ctx, vec![]);
            let shapes = pass(&mut editor, &ctx, vec![]);
            for label in [
                "Page icon",
                "Set icon",
                "Cancel",
                "Changes apply only to this page.",
                "Transparent",
            ] {
                let position = text_position(&shapes, label);
                assert!(
                    (0.0..852.0).contains(&position.x) && (0.0..742.0).contains(&position.y),
                    "{label}: {position:?}"
                );
            }
            assert!(editor.render().unwrap().pixels().all(|p| p.0[3] == 0));
        }
    }

    fn pass(
        editor: &mut Editor,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(852.0, 742.0))),
                events,
                ..Default::default()
            },
            |_| {
                editor.show(ctx, "test");
            },
        );
        output.textures_delta.clear();
        output.shapes
    }

    fn text_position(shapes: &[egui::epaint::ClippedShape], label: &str) -> egui::Pos2 {
        shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.job.text == label
                {
                    Some(text.pos + text.galley.size() / 2.0)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing {label}"))
    }

    fn click(editor: &mut Editor, ctx: &egui::Context, pos: egui::Pos2) {
        for pressed in [true, false] {
            pass(
                editor,
                ctx,
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
    fn undo_redo_buttons_and_platform_shortcuts_restore_layers_without_changing_the_key() {
        for mac in [false, true] {
            let key = KeyConfig::new(crate::actions::WEBSITE, serde_json::json!({}));
            let mut editor = Editor::new("page.0".into(), &key, Mode::Create);
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            ctx.set_os(if mac {
                egui::os::OperatingSystem::Mac
            } else {
                egui::os::OperatingSystem::Windows
            });
            pass(&mut editor, &ctx, Vec::new());
            let shapes = pass(&mut editor, &ctx, Vec::new());
            click(
                &mut editor,
                &ctx,
                text_position(&shapes, &format!("{}  Add text", icon::PLUS)),
            );
            assert_eq!(editor.draft.layers.len(), 1);
            assert!(editor.history.can_undo());
            let shapes = pass(&mut editor, &ctx, Vec::new());
            click(
                &mut editor,
                &ctx,
                text_position(&shapes, &format!("{}  Undo", icon::ARROW_COUNTER_CLOCKWISE)),
            );
            assert!(editor.draft.layers.is_empty());
            assert!(editor.history.can_redo());
            let shapes = pass(&mut editor, &ctx, Vec::new());
            click(
                &mut editor,
                &ctx,
                text_position(&shapes, &format!("{}  Redo", icon::ARROW_CLOCKWISE)),
            );
            assert_eq!(editor.draft.layers.len(), 1);
            pass(&mut editor, &ctx, vec![shortcut(egui::Key::Z, mac, false)]);
            assert!(editor.draft.layers.is_empty());
            let redo = if mac {
                shortcut(egui::Key::Z, true, true)
            } else {
                shortcut(egui::Key::Y, false, false)
            };
            pass(&mut editor, &ctx, vec![redo]);
            assert_eq!(editor.draft.layers.len(), 1);

            // Completed imports are edits too, and Undo retains their pixels
            // for Redo even when the original file no longer exists.
            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(Ok(Work::Loaded(
                "Logo".into(),
                RgbaImage::from_pixel(20, 10, image::Rgba([20, 40, 60, 255])),
            )))
            .unwrap();
            editor.receiver = Some(rx);
            pass(&mut editor, &ctx, Vec::new());
            assert_eq!(editor.draft.layers.len(), 2);
            pass(&mut editor, &ctx, vec![shortcut(egui::Key::Z, mac, false)]);
            assert_eq!(editor.draft.layers.len(), 1);
            pass(&mut editor, &ctx, vec![shortcut(egui::Key::Y, mac, false)]);
            assert_eq!(editor.draft.layers.len(), 2);
            let imported = editor.draft.clone();
            let shapes = pass(&mut editor, &ctx, Vec::new());
            click(&mut editor, &ctx, text_position(&shapes, icon::TRASH));
            assert_eq!(editor.draft.layers.len(), 1);
            pass(&mut editor, &ctx, vec![shortcut(egui::Key::Z, mac, false)]);
            assert!(editor.draft == imported);
            assert_eq!(editor.selected_layer, Some(1));
            assert!(editor.render().is_ok());
            assert_eq!(editor.original, Original::Key(key));
        }
    }

    #[test]
    fn focused_text_uses_draft_undo_and_redo_for_a_typing_burst() {
        let key = KeyConfig::new(crate::actions::WEBSITE, serde_json::json!({}));
        let mut editor = Editor::new("page.0".into(), &key, Mode::Create);
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        pass(&mut editor, &ctx, Vec::new());
        let shapes = pass(&mut editor, &ctx, Vec::new());
        click(
            &mut editor,
            &ctx,
            text_position(&shapes, &format!("{}  Add text", icon::PLUS)),
        );
        let shapes = pass(&mut editor, &ctx, Vec::new());
        click(&mut editor, &ctx, text_position(&shapes, "Text"));
        pass(
            &mut editor,
            &ctx,
            vec![shortcut(egui::Key::A, false, false)],
        );
        pass(&mut editor, &ctx, vec![egui::Event::Text("Bim".into())]);
        pass(&mut editor, &ctx, vec![egui::Event::Text("!".into())]);
        assert!(matches!(&editor.draft.layers[0].content, Content::Text(text) if text == "Bim!"));
        assert!(ctx.text_edit_focused());
        pass(
            &mut editor,
            &ctx,
            vec![shortcut(egui::Key::Z, false, false)],
        );
        assert!(matches!(&editor.draft.layers[0].content, Content::Text(text) if text == "Text"));
        pass(
            &mut editor,
            &ctx,
            vec![shortcut(egui::Key::Y, false, false)],
        );
        assert!(matches!(&editor.draft.layers[0].content, Content::Text(text) if text == "Bim!"));
        assert_eq!(editor.original, Original::Key(key));
    }

    #[test]
    fn a_preview_drag_undoes_as_one_change() {
        let key = KeyConfig::new(crate::actions::WEBSITE, serde_json::json!({}));
        let mut editor = Editor::new("page.0".into(), &key, Mode::Create);
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        pass(&mut editor, &ctx, Vec::new());
        let shapes = pass(&mut editor, &ctx, Vec::new());
        click(
            &mut editor,
            &ctx,
            text_position(&shapes, &format!("{}  Add text", icon::PLUS)),
        );
        let shapes = pass(&mut editor, &ctx, Vec::new());
        let texture = editor.preview.as_ref().unwrap().id();
        let rect = shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Mesh(mesh) = &s.shape
                    && mesh.texture_id == texture
                {
                    Some(mesh.calc_bounds())
                } else {
                    None
                }
            })
            .unwrap();
        let start = rect.center();
        pass(
            &mut editor,
            &ctx,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        for offset in [vec2(20.0, -10.0), vec2(40.0, -20.0)] {
            pass(
                &mut editor,
                &ctx,
                vec![egui::Event::PointerMoved(start + offset)],
            );
        }
        pass(
            &mut editor,
            &ctx,
            vec![egui::Event::PointerButton {
                pos: start + vec2(40.0, -20.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(editor.draft.layers[0].x > 0.6);
        let moved = editor.draft.clone();
        pass(
            &mut editor,
            &ctx,
            vec![shortcut(egui::Key::Z, false, false)],
        );
        assert_eq!(editor.draft.layers[0].x, 0.5);
        assert_eq!(editor.draft.layers[0].y, 0.5);
        pass(
            &mut editor,
            &ctx,
            vec![shortcut(egui::Key::Y, false, false)],
        );
        assert!(editor.draft == moved);
    }

    #[test]
    fn library_and_creator_controls_fit_minimum_window_and_edit_only_the_draft() {
        let key = KeyConfig::new(crate::actions::WEBSITE, serde_json::json!({}));
        let mut editor = Editor::new("page.0".into(), &key, Mode::Library);
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        pass(&mut editor, &ctx, Vec::new());
        let shapes = pass(&mut editor, &ctx, vec![egui::Event::Text("camera".into())]);
        assert_eq!(editor.query, "camera");
        let glyph = icon::ICONS[search_icons("camera", false)[0]].1;
        click(&mut editor, &ctx, text_position(&shapes, glyph));
        assert_eq!(editor.draft.layers.len(), 1);
        let shapes = pass(&mut editor, &ctx, Vec::new());
        click(
            &mut editor,
            &ctx,
            text_position(&shapes, "Customize in creator"),
        );
        assert!(editor.mode == Mode::Create);
        let shapes = pass(&mut editor, &ctx, Vec::new());
        click(
            &mut editor,
            &ctx,
            text_position(&shapes, &format!("{}  Add text", icon::PLUS)),
        );
        assert_eq!(editor.draft.layers.len(), 2);
        let shapes = pass(&mut editor, &ctx, Vec::new());
        for label in [
            "Set image",
            "Cancel",
            "Center",
            "Layers",
            "Add layer",
            "Edit selected layer",
        ] {
            let pos = text_position(&shapes, label);
            assert!(
                (0.0..852.0).contains(&pos.x) && (0.0..742.0).contains(&pos.y),
                "{label}: {pos:?}"
            );
        }
        assert!(editor.render().is_ok());
        assert_eq!(Original::Key(key), editor.original);
    }

    #[test]
    fn searches_entire_catalog_and_normalizes_words() {
        assert!(search_icons("", false).len() > 1000);
        assert_eq!(
            search_icons("arrow-left", false),
            search_icons("ARROW_left", false)
        );
        assert!(
            search_icons("camera", false)
                .iter()
                .all(|&i| icon::ICONS[i].0.contains("CAMERA"))
        );
        assert!(
            search_icons("", true)
                .iter()
                .all(|&i| icon::ICONS[i].0.contains("LOGO"))
        );
        assert!(search_icons("no-such-deckly-icon", false).is_empty());
    }

    #[test]
    fn guards_against_changed_key_or_destination() {
        let key = KeyConfig::new(crate::actions::WEBSITE, serde_json::json!({}));
        let editor = Editor::new("page.0".into(), &key, Mode::Create);
        assert!(editor.matches("page.0", &key));
        assert!(!editor.matches("page.1", &key));
        let mut replaced = key.clone();
        replaced.image = Some("new.png".into());
        assert!(!editor.matches("page.0", &replaced));
    }

    #[test]
    fn cancelling_keeps_key_unchanged() {
        let key = KeyConfig::new(crate::actions::WEBSITE, serde_json::json!({}));
        let mut editor = Editor::new("page.0".into(), &key, Mode::Create);
        editor.add(Layer::new(Content::Text("BimBim".into())));
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let mut result = Selection::Pending;
        let mut warmup = ctx.run_ui(egui::RawInput::default(), |_| {
            editor.show(&ctx, "test");
        });
        warmup.textures_delta.clear();
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |_| result = editor.show(&ctx, "test"),
        );
        output.textures_delta.clear();
        assert!(matches!(result, Selection::Cancelled));
        assert_eq!(editor.original, Original::Key(key));
    }
}

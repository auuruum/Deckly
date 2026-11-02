//! The same image workflow on every desktop: native file selection, an
//! offline Phosphor catalog and a small layered creator, all drawn by egui.

mod composition;

use crate::store;
use crate::theme::*;
use composition::{Composition, Content, Fonts, Layer, MAX_LAYERS, MAX_TEXT, SIZE};
use deckly_core::KeyConfig;
use eframe::egui::{
    self, Color32, ColorImage, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui,
    pos2, vec2,
};
use egui_phosphor::regular as icon;
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
}

enum Work {
    Loaded(String, RgbaImage),
    Saved(PathBuf),
}

pub enum Selection {
    Pending,
    Cancelled,
    Image(PathBuf),
}

pub struct Editor {
    pub context: String,
    original: KeyConfig,
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
}

impl Editor {
    pub fn new(context: String, key: &KeyConfig, mode: Mode) -> Self {
        Self {
            context,
            original: key.clone(),
            mode,
            query: String::new(),
            logos_only: false,
            selected_icon: None,
            adding_icon: false,
            draft: Composition::default(),
            selected_layer: None,
            fonts: Fonts::new(),
            preview: None,
            dirty: true,
            error: None,
            receiver: None,
            saving: false,
            focus_search: true,
        }
    }

    /// A modal never applies a result to a different or replaced action.
    pub fn matches(&self, context: &str, key: &KeyConfig) -> bool {
        self.context == context && self.original == *key
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
                "Images (first frame)",
                &["png", "jpg", "jpeg", "webp", "gif", "bmp"],
            )
            .pick_file()
        {
            self.load(ctx, path);
            return true;
        }
        false
    }

    fn load(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.start(ctx, move || {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().chars().take(80).collect())
                .unwrap_or_else(|| "Picture".into());
            composition::load_picture(&path).map(|image| Work::Loaded(name, image))
        });
    }

    fn receive(&mut self) -> Option<PathBuf> {
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
        match result {
            Ok(Work::Saved(path)) => return Some(path),
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
        if let Some(path) = self.receive() {
            return Selection::Image(path);
        }
        let mut cancel = false;
        let mut apply = false;
        let busy = self.receiver.is_some();
        let modal = egui::Modal::new(egui::Id::new("key-image-editor")).show(ctx, |ui| {
            ui.set_width(684.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::IMAGE).size(23.0));
                ui.heading("Key image");
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
                        .add_enabled(valid, egui::Button::new("Set image"))
                        .clicked();
                    ui.label(RichText::new("Changes apply only to this key.").color(TEXT_DIM));
                });
            });
        });
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
                        ui.label("Background");
                        self.dirty |= ui
                            .color_edit_button_srgb(&mut self.draft.background)
                            .changed();
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
                    let delta = ui.input(|i| i.pointer.delta()) / side;
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
                    ui.label("Background");
                    self.dirty |= ui
                        .color_edit_button_srgb(&mut self.draft.background)
                        .changed();
                });
                if let Some(path) = self.original.image.clone()
                    && ui
                        .add_enabled(
                            self.draft.layers.len() < MAX_LAYERS,
                            egui::Button::new("Add current key image"),
                        )
                        .clicked()
                {
                    self.load(ui.ctx(), path);
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.set_width(380.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Layers").strong());
                    ui.label(
                        RichText::new(format!("{} / {MAX_LAYERS}", self.draft.layers.len()))
                            .color(TEXT_DIM),
                    );
                });
                ui.add_enabled_ui(self.draft.layers.len() < MAX_LAYERS, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(format!("{}  Text", icon::TEXT_T)).clicked() {
                            self.add(Layer::new(Content::Text("Text".into())));
                        }
                        if ui.button(format!("{}  Icon", icon::SQUARES_FOUR)).clicked() {
                            self.adding_icon = true;
                            self.focus_search = true;
                        }
                        if ui.button(format!("{}  Image…", icon::IMAGE)).clicked() {
                            self.upload(ui.ctx());
                        }
                    });
                });
                egui::ScrollArea::vertical()
                    .id_salt("creator-layers")
                    .min_scrolled_height(104.0)
                    .max_height(104.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, layer) in self.draft.layers.iter().enumerate().rev() {
                            let name: String = layer.name().chars().take(32).collect();
                            if ui
                                .selectable_label(
                                    self.selected_layer == Some(i),
                                    format!("{}  {name}", i + 1),
                                )
                                .clicked()
                            {
                                self.selected_layer = Some(i);
                            }
                        }
                        if self.draft.layers.is_empty() {
                            ui.label(
                                RichText::new("Add text, icons or your own pictures.")
                                    .color(TEXT_DIM),
                            );
                        }
                    });
                ui.separator();
                self.layer_controls(ui);
            });
        });
        if self.dirty {
            ui.ctx().request_repaint();
        }
    }

    fn layer_controls(&mut self, ui: &mut Ui) {
        let Some(index) = self.selected_layer else {
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
            text_position(&shapes, &format!("{}  Text", icon::TEXT_T)),
        );
        assert_eq!(editor.draft.layers.len(), 2);
        let shapes = pass(&mut editor, &ctx, Vec::new());
        for label in ["Set image", "Cancel", "Center", "Layers"] {
            let pos = text_position(&shapes, label);
            assert!(
                (0.0..852.0).contains(&pos.x) && (0.0..742.0).contains(&pos.y),
                "{label}: {pos:?}"
            );
        }
        assert!(editor.render().is_ok());
        assert_eq!(key, editor.original);
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
        assert_eq!(editor.original, key);
    }
}

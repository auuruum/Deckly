//! Searchable installed application picker. The worker returns plain data;
//! only confirming a row edits the serializable Open action settings.

use crate::actions;
use crate::os::applications::{self, Application, Catalog};
use crate::theme::*;
use deckly_core::KeyConfig;
use eframe::egui::{self, Align2, CornerRadius, RichText, Sense, pos2, vec2};
use egui_phosphor::regular as icon;
use std::sync::mpsc::{self, Receiver, TryRecvError};
mod icons;

pub struct Picker {
    pub context: String,
    receiver: Option<Receiver<Result<Catalog, String>>>,
    catalog: Catalog,
    error: Option<String>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    focus: bool,
    scroll_y: f32,
    reset_scroll: bool,
    icons: icons::Cache,
}

pub enum Selection {
    Pending,
    Cancelled,
    Application(Application),
}

impl Picker {
    pub fn new(ctx: &egui::Context, context: String) -> Self {
        let (tx, rx) = mpsc::sync_channel(1);
        let repaint = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("deckly-applications".into())
            .spawn(move || {
                let _ = tx.send(applications::discover());
                repaint.request_repaint();
            });
        Self {
            context,
            receiver: worker.as_ref().ok().map(|_| rx),
            catalog: Catalog::default(),
            error: worker
                .err()
                .map(|e| format!("Cannot read installed applications: {e}")),
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            focus: true,
            scroll_y: 0.0,
            reset_scroll: true,
            icons: icons::Cache::new(ctx),
        }
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        self.matches = self
            .catalog
            .applications
            .iter()
            .enumerate()
            .filter_map(|(index, app)| app.matches(&query).then_some(index))
            .collect();
        self.selected = 0;
        self.reset_scroll = true;
    }

    fn receive(&mut self) {
        let Some(receiver) = &self.receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(catalog)) => {
                self.catalog = catalog;
                self.receiver = None;
                self.filter();
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                self.receiver = None;
            }
            Err(TryRecvError::Disconnected) => {
                self.error =
                    Some("Application discovery stopped. Close this window and try again.".into());
                self.receiver = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Selection {
        self.receive();
        self.icons.receive(ctx);
        let mut confirm = false;
        let mut cancel = false;
        let mut moved = false;
        let modal = egui::Modal::new(egui::Id::new("application-picker")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::APP_WINDOW).size(24.0).color(TEXT_DIM));
                ui.heading("Choose an application");
            });
            ui.label(
                RichText::new("Find an installed app by name, then select it.").color(TEXT_DIM),
            );
            ui.add_space(12.0);
            // Consume these before the text field: Up/Down navigate results
            // while focus stays in search, Enter confirms the highlighted row.
            let search_id = egui::Id::new("application-search");
            let navigate = ui.memory(|m| m.focused().is_none() || m.has_focus(search_id));
            let (up, down, enter) = ui.input_mut(|i| {
                (
                    navigate && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    navigate && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    navigate && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                )
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::MAGNIFYING_GLASS).color(TEXT_DIM));
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id(search_id)
                        .hint_text("Search applications…")
                        .desired_width(f32::INFINITY),
                );
                if self.focus {
                    search.request_focus();
                    self.focus = false;
                }
                if search.changed() {
                    self.filter();
                }
            });
            if up || down {
                self.selected = next_selection(self.selected, self.matches.len(), down);
                moved = true;
            }
            confirm |= enter && !self.matches.is_empty();
            ui.add_space(8.0);
            let row_height = 52.0;
            let pitch = row_height + ui.spacing().item_spacing.y;
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt("installed-applications")
                .min_scrolled_height(260.0)
                .max_height(260.0)
                .auto_shrink([false, false]);
            if std::mem::take(&mut self.reset_scroll) {
                scroll = scroll.vertical_scroll_offset(0.0);
            } else if moved {
                let top = self.selected as f32 * pitch;
                let offset = self.scroll_y.min(top).max(top + pitch - 260.0).max(0.0);
                scroll = scroll.vertical_scroll_offset(offset);
            }
            let output = scroll.show_rows(ui, row_height, self.matches.len(), |ui, rows| {
                for row in rows {
                    let Some(app) = self
                        .matches
                        .get(row)
                        .and_then(|index| self.catalog.applications.get(*index))
                    else {
                        continue;
                    };
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(ui.available_width(), row_height),
                        Sense::click(),
                    );
                    let selected = self.selected == row;
                    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                    if selected || response.hovered() {
                        painter.rect_filled(
                            rect.shrink(1.0),
                            CornerRadius::same(5),
                            if selected {
                                ACCENT.gamma_multiply(0.45)
                            } else {
                                LIST_HOVER
                            },
                        );
                    }
                    if let Some(texture) = self.icons.texture(app) {
                        let image_rect = egui::Rect::from_center_size(
                            pos2(rect.left() + 22.0, rect.center().y),
                            vec2(28.0, 28.0),
                        );
                        painter.image(
                            texture,
                            image_rect,
                            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    } else {
                        painter.text(
                            pos2(rect.left() + 22.0, rect.center().y),
                            Align2::CENTER_CENTER,
                            icon::APP_WINDOW,
                            font(24.0),
                            ITEM_ICON,
                        );
                    }
                    painter.text(
                        pos2(rect.left() + 46.0, rect.top() + 8.0),
                        Align2::LEFT_TOP,
                        &app.name,
                        bold(14.0),
                        TEXT,
                    );
                    painter.text(
                        pos2(rect.left() + 46.0, rect.top() + 28.0),
                        Align2::LEFT_TOP,
                        &app.detail,
                        font(11.0),
                        TEXT_DIM,
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            &app.name,
                        )
                    });
                    if response.clicked() {
                        self.selected = row;
                    }
                    confirm |= response.double_clicked();
                    response.on_hover_text(&app.target);
                }
            });
            self.scroll_y = output.state.offset.y;
            if self.receiver.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for applications…");
                });
            } else if let Some(error) = &self.error {
                ui.label(RichText::new(error).color(WARNING));
            } else if self.matches.is_empty() {
                ui.label(if self.query.trim().is_empty() {
                    "No applications found. You can choose a file in the Open action."
                } else {
                    "No matching applications. Try another name."
                });
            } else {
                ui.label(
                    RichText::new(format!(
                        "{} {} · Up / Down to navigate · Enter to select",
                        self.matches.len(),
                        if self.matches.len() == 1 {
                            "application"
                        } else {
                            "applications"
                        }
                    ))
                    .size(11.0)
                    .color(TEXT_DIM),
                );
            }
            if self.catalog.truncated {
                ui.label(
                    RichText::new(
                        "The application list reached its size limit. You can also choose a file.",
                    )
                    .color(WARNING),
                );
            }
            ui.add_space(8.0);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                confirm |= ui
                    .add_enabled(!self.matches.is_empty(), egui::Button::new("Select"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if cancel || modal.should_close() {
            return Selection::Cancelled;
        }
        if confirm
            && let Some(app) = self
                .matches
                .get(self.selected)
                .and_then(|index| self.catalog.applications.get(*index))
        {
            return Selection::Application(app.clone());
        }
        Selection::Pending
    }
}

fn next_selection(current: usize, count: usize, down: bool) -> usize {
    if down {
        current.saturating_add(1).min(count.saturating_sub(1))
    } else {
        current.saturating_sub(1).min(count.saturating_sub(1))
    }
}

pub fn apply(key: &mut KeyConfig, app: &Application) {
    actions::set_setting(&mut key.settings, "path", serde_json::json!(app.target));
    actions::set_setting(
        &mut key.settings,
        "application_name",
        serde_json::json!(app.name),
    );
    if key.title.is_empty() {
        // Launcher/channel qualifiers stay in the inspector; the small key
        // gets the app's name (e.g. Spotify instead of Spotify (Launcher)).
        key.title = app
            .name
            .split(" (")
            .next()
            .unwrap_or(&app.name)
            .chars()
            .take(deckly_core::profile::MAX_TITLE_CHARS)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Picker {
        let applications = ["Firefox", "Spotify"]
            .map(|name| Application {
                name: name.into(),
                target: format!("/apps/{name}.desktop"),
                detail: "Installed application".into(),
                keywords: String::new(),
                icon: String::new(),
            })
            .into();
        let mut picker = Picker {
            context: "test-key".into(),
            receiver: None,
            catalog: Catalog {
                applications,
                truncated: false,
            },
            error: None,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            focus: true,
            scroll_y: 0.0,
            reset_scroll: true,
            icons: icons::Cache::disabled(),
        };
        picker.filter();
        picker
    }

    fn key_event(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn frame(ctx: &egui::Context, picker: &mut Picker, events: Vec<egui::Event>) -> Selection {
        let mut result = Selection::Pending;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    pos2(0.0, 0.0),
                    vec2(852.0, 742.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                result = picker.show(ui.ctx());
            },
        );
        output.textures_delta.clear();
        result
    }

    #[test]
    fn search_focus_keyboard_selection_and_escape_work_in_the_modal() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let mut picker = ready();
        assert!(matches!(
            frame(&ctx, &mut picker, vec![]),
            Selection::Pending
        ));
        frame(&ctx, &mut picker, vec![egui::Event::Text("SpOtIfY".into())]);
        assert_eq!(picker.matches.len(), 1);
        let Selection::Application(app) =
            frame(&ctx, &mut picker, vec![key_event(egui::Key::Enter)])
        else {
            panic!("Enter selects the match")
        };
        assert_eq!(app.name, "Spotify");

        let mut picker = ready();
        frame(&ctx, &mut picker, vec![]);
        frame(&ctx, &mut picker, vec![key_event(egui::Key::ArrowDown)]);
        assert_eq!(picker.selected, 1);
        assert!(matches!(
            frame(&ctx, &mut picker, vec![key_event(egui::Key::Escape)]),
            Selection::Cancelled
        ));
    }

    #[test]
    fn empty_search_and_worker_failure_leave_nothing_to_confirm() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let mut picker = ready();
        frame(&ctx, &mut picker, vec![]);
        frame(
            &ctx,
            &mut picker,
            vec![egui::Event::Text("missing app".into())],
        );
        assert!(matches!(
            frame(&ctx, &mut picker, vec![key_event(egui::Key::Enter)]),
            Selection::Pending
        ));
        let (tx, rx) = mpsc::channel();
        drop(tx);
        picker.receiver = Some(rx);
        picker.receive();
        assert!(picker.error.is_some());
        assert!(picker.receiver.is_none());
    }

    #[test]
    fn keyboard_navigation_is_bounded_even_with_no_results() {
        assert_eq!(next_selection(0, 0, true), 0);
        assert_eq!(next_selection(0, 5, false), 0);
        assert_eq!(next_selection(0, 5, true), 1);
        assert_eq!(next_selection(4, 5, true), 4);
        assert_eq!(next_selection(100, 2, false), 1);
    }

    #[test]
    fn selection_uses_existing_open_settings_and_preserves_custom_title() {
        let mut key = actions::new_key(actions::find(actions::OPEN).unwrap());
        let app = Application {
            name: "Spotify".into(),
            target: "/apps/spotify.desktop".into(),
            detail: String::new(),
            keywords: String::new(),
            icon: String::new(),
        };
        apply(&mut key, &app);
        assert_eq!(key.title, "Spotify");
        assert_eq!(actions::str_setting(&key.settings, "path"), app.target);
        assert_eq!(
            actions::str_setting(&key.settings, "application_name"),
            "Spotify"
        );
        key.title = "Music".into();
        apply(&mut key, &app);
        assert_eq!(key.title, "Music");
        let saved = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<KeyConfig>(&saved).unwrap(), key);

        key.title.clear();
        let mut launcher = app;
        launcher.name = "Spotify (Launcher)".into();
        apply(&mut key, &launcher);
        assert_eq!(key.title, "Spotify");
        assert_eq!(
            actions::str_setting(&key.settings, "application_name"),
            "Spotify (Launcher)"
        );
    }
}

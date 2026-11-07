//! The Feedback Marks section of the Style tab: how the failure mark, the
//! success mark and the Auto Exit ring look on every key
//! (docs/clean-room/2026-11-07-feedback-marks.md). Changes apply at once,
//! like the key press effect; a drag is saved when the mouse is let go.

use super::{Env, Preferences, Sample, at, button, combo, key_preview, label, toggle, value_dim};
use crate::actions;
use crate::plugins::{Mark, Overlay};
use crate::render::{self, Live};
use crate::runtime::{self, Msg};
use crate::theme::{TEXT_DIM, font};
use deckly_core::{FeedbackMarks, KeyConfig, MARK_ICONS, MarkStyle, RingStyle};
use eframe::egui::{self, Pos2, RichText, Ui, vec2};
use serde_json::json;

/// Where the section starts on the Style tab.
pub(super) const TOP: f32 = 492.0;
/// Height of one mark's controls.
const BLOCK: f32 = 96.0;
/// How far the Style tab reaches with this section.
pub(super) const BOTTOM: f32 = TOP + 3.0 * BLOCK + 36.0;
/// The Auto Exit preview shows the ring this far.
const PREVIEW_PERCENT: u8 = 65;
const DURATION_STEP_MS: u32 = 250;

impl Preferences {
    pub(super) fn feedback_marks(&mut self, ui: &mut Ui, o: Pos2, env: &mut Env<'_>) {
        let saved = runtime::lock(env.state).settings.feedback_marks.clone();
        let mut marks = saved.clone();
        let failure = MarkBlock {
            label: "Failure Mark:",
            show: "Show when an action fails",
            id: "failure",
            mark: Mark::Alert,
            action: actions::HOTKEY,
        };
        let success = MarkBlock {
            label: "Success Mark:",
            show: "Show after an action ran",
            id: "success",
            mark: Mark::Ok,
            action: actions::MULTI_ACTION,
        };
        let [first, second, third] = &mut self.mark_previews;
        let y = TOP;
        failure.show(
            ui,
            o,
            y,
            &mut marks.failure,
            MarkStyle::failure(),
            first,
            env,
        );
        success.show(
            ui,
            o,
            y + BLOCK,
            &mut marks.success,
            MarkStyle::success(),
            second,
            env,
        );
        ring(ui, o, y + 2.0 * BLOCK, &mut marks.folder_exit, third, env);
        value_dim(
            ui,
            o,
            219.0,
            y + 3.0 * BLOCK,
            420.0,
            "The marks look the same on every profile and key.",
        );
        self.save_marks(ui, marks, &saved, env);
    }

    /// Puts changed marks in place at once; saves them once nothing is
    /// dragged.
    fn save_marks(
        &mut self,
        ui: &Ui,
        mut marks: FeedbackMarks,
        saved: &FeedbackMarks,
        env: &mut Env<'_>,
    ) {
        let dragging = ui.input(|i| i.pointer.any_down());
        marks.sanitize();
        if marks != *saved {
            env.renderer.marks.clone_from(&marks);
            let mut s = runtime::lock(env.state);
            s.settings.feedback_marks = marks;
            if !dragging {
                s.save_settings();
            }
            drop(s);
            self.marks_unsaved = dragging;
            let _ = env.runtime.send(Msg::Settings);
        } else if self.marks_unsaved && !dragging {
            runtime::lock(env.state).save_settings();
            self.marks_unsaved = false;
        }
    }
}

/// The controls of the failure or success mark.
struct MarkBlock {
    label: &'static str,
    show: &'static str,
    id: &'static str,
    mark: Mark,
    /// The action of the preview key.
    action: &'static str,
}

impl MarkBlock {
    #[allow(clippy::too_many_arguments)]
    fn show(
        &self,
        ui: &mut Ui,
        o: Pos2,
        y: f32,
        style: &mut MarkStyle,
        default: MarkStyle,
        preview: &mut Option<(u64, egui::TextureHandle)>,
        env: &mut Env<'_>,
    ) {
        label(ui, o, y, 18.0, self.label);
        toggle(
            ui,
            at(o, 219.0, y, 260.0, 18.0),
            &mut style.visible,
            self.show,
        );
        reset(ui, o, y, self.id, style, default);
        ui.add_enabled_ui(style.visible, |ui| {
            let id = format!("mark-icon-{}", self.id);
            let selected = icon_text(&style.icon);
            combo(
                ui,
                at(o, 219.0, y + 30.0, 150.0, 22.0),
                &id,
                &selected,
                |ui| {
                    for name in MARK_ICONS {
                        let text = RichText::new(icon_text(name)).font(font(13.0));
                        ui.selectable_value(&mut style.icon, name.to_owned(), text);
                    }
                },
            );
            color(ui, o, 381.0, y + 31.0, &mut style.color);
            slider_row(ui, o, y + 60.0, "Size", |ui| {
                let range = MarkStyle::MIN_SIZE..=MarkStyle::MAX_SIZE;
                ui.add(egui::Slider::new(&mut style.size, range).show_value(false));
                format!("{}%", style.size)
            });
            let duration = at(o, 424.0, y + 60.0, 60.0, 20.0);
            ui.painter().text(
                duration.left_center(),
                egui::Align2::LEFT_CENTER,
                "Duration",
                font(13.0),
                TEXT_DIM,
            );
            let slider = at(o, 488.0, y + 60.0, 90.0, 20.0);
            ui.scope_builder(egui::UiBuilder::new().max_rect(slider), |ui| {
                ui.spacing_mut().slider_width = slider.width();
                let range = MarkStyle::MIN_DURATION_MS..=MarkStyle::MAX_DURATION_MS;
                ui.add(
                    egui::Slider::new(&mut style.duration_ms, range)
                        .step_by(f64::from(DURATION_STEP_MS))
                        .show_value(false),
                );
            });
            super::value(ui, o, 584.0, y + 60.0, 20.0, &seconds(style.duration_ms));
        });
        let key = KeyConfig::new(self.action, json!({}));
        let overlay = Overlay {
            mark: Some(self.mark),
            ..Overlay::default()
        };
        let sample = Sample {
            name: self.id,
            key: &key,
            live: Live::default(),
            overlay: Some(&overlay),
        };
        key_preview(
            ui,
            at(o, 648.0, y, 64.0, 64.0),
            preview,
            &sample,
            env.renderer,
        );
    }
}

/// The controls of the Auto Exit ring.
fn ring(
    ui: &mut Ui,
    o: Pos2,
    y: f32,
    style: &mut RingStyle,
    preview: &mut Option<(u64, egui::TextureHandle)>,
    env: &mut Env<'_>,
) {
    label(ui, o, y, 18.0, "Auto Exit Ring:");
    toggle(
        ui,
        at(o, 219.0, y, 260.0, 18.0),
        &mut style.visible,
        "Show the folder countdown",
    );
    reset(ui, o, y, "ring", style, RingStyle::default());
    ui.add_enabled_ui(style.visible, |ui| {
        color(ui, o, 219.0, y + 31.0, &mut style.color);
        value_dim(
            ui,
            o,
            261.0,
            y + 32.0,
            360.0,
            "Lasts as long as the folder's Exit after delay.",
        );
        slider_row(ui, o, y + 60.0, "Size", |ui| {
            let range = RingStyle::MIN_SIZE..=RingStyle::MAX_SIZE;
            ui.add(egui::Slider::new(&mut style.size, range).show_value(false));
            format!("{}%", style.size)
        });
    });
    let key = KeyConfig::new(actions::PARENT_FOLDER, json!({}));
    let sample = Sample {
        name: "ring",
        key: &key,
        live: Live {
            exit: Some(PREVIEW_PERCENT),
            ..Live::default()
        },
        overlay: None,
    };
    key_preview(
        ui,
        at(o, 648.0, y, 64.0, 64.0),
        preview,
        &sample,
        env.renderer,
    );
}

/// Puts a style back to its default; off while it is the default.
fn reset<T: PartialEq>(ui: &mut Ui, o: Pos2, y: f32, id: &str, style: &mut T, default: T) {
    let changed = *style != default;
    let clicked = ui
        .push_id(("mark-reset", id), |ui| {
            ui.add_enabled_ui(changed, |ui| {
                button(ui, at(o, 566.0, y - 2.0, 64.0, 22.0), "Reset")
            })
            .inner
        })
        .inner
        .on_hover_text("Back to Deckly's default look")
        .clicked();
    if clicked {
        *style = default;
    }
}

/// A colour button at `x`, `y`.
fn color(ui: &mut Ui, o: Pos2, x: f32, y: f32, color: &mut [u8; 3]) {
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(at(o, x, y, 30.0, 20.0)),
        |ui| {
            ui.spacing_mut().interact_size = vec2(30.0, 20.0);
            egui::color_picker::color_edit_button_srgb(ui, color);
        },
    );
}

/// "Size" with its slider and value on one row; `add` adds the slider and
/// returns the value to show.
fn slider_row(ui: &mut Ui, o: Pos2, y: f32, name: &str, add: impl FnOnce(&mut Ui) -> String) {
    let r = at(o, 219.0, y, 36.0, 20.0);
    ui.painter().text(
        r.left_center(),
        egui::Align2::LEFT_CENTER,
        name,
        font(13.0),
        TEXT_DIM,
    );
    let slider = at(o, 256.0, y, 110.0, 20.0);
    let shown = ui
        .scope_builder(egui::UiBuilder::new().max_rect(slider), |ui| {
            ui.spacing_mut().slider_width = slider.width();
            add(ui)
        })
        .inner;
    super::value(ui, o, 372.0, y, 20.0, &shown);
}

/// An icon's glyph and name, as the menu shows it.
fn icon_text(name: &str) -> String {
    format!("{}  {}", render::mark_glyph(name), icon_name(name))
}

/// "warning-circle" as "Warning circle".
fn icon_name(name: &str) -> String {
    let spaced = name.replace('-', " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Milliseconds as seconds: "1.5 s", "0.25 s", "10 s".
fn seconds(ms: u32) -> String {
    let text = format!("{:.2}", f64::from(ms) / 1000.0);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text} s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_as_seconds() {
        assert_eq!(seconds(1500), "1.5 s");
        assert_eq!(seconds(250), "0.25 s");
        assert_eq!(seconds(10_000), "10 s");
        assert_eq!(seconds(1000), "1 s");
    }

    #[test]
    fn icon_names_read_as_words() {
        assert_eq!(icon_name("warning-circle"), "Warning circle");
        assert_eq!(icon_name("x"), "X");
        assert_eq!(icon_name(""), "");
    }
}

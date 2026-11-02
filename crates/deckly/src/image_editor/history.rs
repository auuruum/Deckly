//! Undo for the open image draft. Pixel buffers are immutable and shared;
//! history never writes files or changes the assigned key before Set image.

use super::composition::{Composition, Content};
use crate::history::Direction;
use eframe::egui;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_EDITS: usize = 50;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const TYPING_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct Snapshot {
    pub draft: Composition,
    pub selected_layer: Option<usize>,
    pub selected_icon: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// A unique pointer press, including the release and all drag frames.
    Pointer(u64),
    Typing(egui::Id, Option<usize>),
}

struct Edit {
    before: Snapshot,
    after: Snapshot,
    group: Option<Group>,
    at: Instant,
}

#[derive(Default)]
pub struct History {
    undo: VecDeque<Edit>,
    redo: VecDeque<Edit>,
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn record(&mut self, before: Snapshot, after: Snapshot, group: Option<Group>) -> bool {
        if before.draft == after.draft {
            return false;
        }
        self.redo.clear();
        let now = Instant::now();
        if let Some(last) = self.undo.back_mut()
            && group.is_some()
            && last.group == group
            && last.after.draft == before.draft
            && (matches!(group, Some(Group::Pointer(_)))
                || now.duration_since(last.at) <= TYPING_DELAY)
        {
            last.after = after;
            last.at = now;
            if last.before.draft == last.after.draft {
                self.undo.pop_back();
            }
        } else {
            self.undo.push_back(Edit {
                before,
                after,
                group,
                at: now,
            });
        }
        while self.undo.len() + self.redo.len() > MAX_EDITS || self.bytes() > MAX_BYTES {
            if self.undo.pop_front().is_none() {
                self.redo.pop_front();
            }
        }
        true
    }

    pub fn step(&mut self, direction: Direction) -> Option<Snapshot> {
        let redo = matches!(direction, Direction::Redo);
        let source = if redo { &mut self.redo } else { &mut self.undo };
        let mut edit = source.pop_back()?;
        let snapshot = if redo {
            edit.after.clone()
        } else {
            edit.before.clone()
        };
        // A new gesture or typing after Undo must start a fresh edit.
        edit.group = None;
        if redo {
            self.undo.push_back(edit);
        } else {
            self.redo.push_back(edit);
        }
        Some(snapshot)
    }

    fn bytes(&self) -> usize {
        let mut pictures = HashSet::new();
        let mut bytes = 0;
        for edit in self.undo.iter().chain(&self.redo) {
            bytes += std::mem::size_of::<Edit>();
            for snapshot in [&edit.before, &edit.after] {
                bytes += snapshot.draft.layers.capacity()
                    * std::mem::size_of::<super::composition::Layer>();
                for layer in &snapshot.draft.layers {
                    match &layer.content {
                        Content::Text(text) => bytes += text.capacity(),
                        Content::Picture(name, image) => {
                            bytes += name.capacity();
                            if pictures.insert(Arc::as_ptr(image)) {
                                bytes += image.as_raw().len();
                            }
                        }
                        Content::Icon(..) => {}
                    }
                }
            }
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::super::composition::{Layer, MAX_LAYERS};
    use super::*;
    use image::{Rgba, RgbaImage};

    fn draft() -> Snapshot {
        Snapshot {
            draft: Composition::default(),
            selected_layer: None,
            selected_icon: None,
        }
    }

    #[test]
    fn undo_redo_restores_layers_colors_order_placement_text_and_selection() {
        let before = draft();
        let mut after = before.clone();
        after
            .draft
            .add(Layer::new(Content::Text("Text".into())))
            .unwrap();
        after.selected_layer = Some(0);
        let mut history = History::default();
        history.record(before.clone(), after.clone(), None);
        let mut edited = after.clone();
        edited.draft.layers[0].content = Content::Text("Changed".into());
        edited.draft.layers[0].x = 0.2;
        edited.draft.layers[0].size = 0.8;
        edited.draft.layers[0].color = [30, 60, 90];
        edited.draft.background = [10, 20, 30];
        edited
            .draft
            .add(Layer::new(Content::Icon(
                "Camera",
                egui_phosphor::regular::CAMERA,
            )))
            .unwrap();
        edited.draft.layers.swap(0, 1);
        edited.selected_layer = Some(1);
        history.record(after.clone(), edited.clone(), None);
        let restored = history.step(Direction::Undo).unwrap();
        assert!(restored.draft == after.draft);
        assert_eq!(restored.selected_layer, Some(0));
        assert!(history.step(Direction::Undo).unwrap().draft == before.draft);
        assert!(history.step(Direction::Undo).is_none());
        assert!(history.step(Direction::Redo).unwrap().draft == after.draft);
        let restored = history.step(Direction::Redo).unwrap();
        assert!(restored.draft == edited.draft);
        assert_eq!(restored.selected_layer, Some(1));
    }

    #[test]
    fn one_drag_is_one_edit_but_separate_gestures_stay_separate() {
        let mut before = draft();
        before
            .draft
            .add(Layer::new(Content::Text("Text".into())))
            .unwrap();
        let mut current = before.clone();
        let mut history = History::default();
        for x in [0.1, 0.2, 0.3] {
            let previous = current.clone();
            current.draft.layers[0].x = x;
            history.record(previous, current.clone(), Some(Group::Pointer(1)));
        }
        assert_eq!(history.undo.len(), 1);
        let moved = current.clone();
        current.draft.layers[0].y = 0.8;
        history.record(moved.clone(), current, Some(Group::Pointer(2)));
        assert_eq!(history.undo.len(), 2);
        assert!(history.step(Direction::Undo).unwrap().draft == moved.draft);
        assert!(history.step(Direction::Undo).unwrap().draft == before.draft);
    }

    #[test]
    fn typing_groups_by_field_and_layer_and_new_edits_clear_redo() {
        let mut before = draft();
        before
            .draft
            .add(Layer::new(Content::Text("".into())))
            .unwrap();
        let group = Some(Group::Typing(egui::Id::new("text"), Some(0)));
        let mut history = History::default();
        let mut current = before.clone();
        for text in ["B", "Bi", "Bim"] {
            let previous = current.clone();
            current.draft.layers[0].content = Content::Text(text.into());
            history.record(previous, current.clone(), group);
        }
        assert_eq!(history.undo.len(), 1);
        let restored = history.step(Direction::Undo).unwrap();
        assert!(restored.draft == before.draft);
        assert!(history.can_redo());
        let mut new = restored.clone();
        new.draft.background = [20; 3];
        history.record(restored, new, None);
        assert!(!history.can_redo());
        assert!(history.step(Direction::Redo).is_none());
    }

    #[test]
    fn history_shares_images_and_bounds_retained_edits_and_unique_pixels() {
        let mut current = draft();
        for _ in 0..MAX_LAYERS {
            current
                .draft
                .add(Layer::new(Content::Picture(
                    "Logo".into(),
                    Arc::new(RgbaImage::from_pixel(512, 512, Rgba([0; 4]))),
                )))
                .unwrap();
        }
        let mut history = History::default();
        for i in 0..100 {
            let previous = current.clone();
            current.draft.layers[0].x = i as f32 / 100.0;
            history.record(previous, current.clone(), None);
        }
        assert_eq!(history.undo.len(), MAX_EDITS);
        assert!(history.bytes() < 10 * 1024 * 1024);
        for _ in 0..40 {
            let previous = current.clone();
            current.draft.layers[0].content =
                Content::Picture("New logo".into(), Arc::new(RgbaImage::new(512, 512)));
            history.record(previous, current.clone(), None);
        }
        assert!(history.bytes() <= MAX_BYTES);
        assert!(history.undo.len() < MAX_EDITS);
    }
}

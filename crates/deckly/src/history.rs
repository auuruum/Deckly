//! Session-only editor history, bounded across all profiles.

use deckly_core::Profile;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const MAX_EDITS: usize = 100;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const GROUP_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Location {
    pub page: usize,
    pub path: Vec<String>,
    pub key: Option<u8>,
}

#[derive(Clone, Copy)]
pub enum Direction {
    Undo,
    Redo,
}

struct Edit {
    profile: String,
    before: String,
    after: String,
    before_location: Location,
    after_location: Location,
    group: Option<u64>,
    at: Instant,
}

#[derive(Default)]
pub struct History {
    undo: VecDeque<Edit>,
    redo: VecDeque<Edit>,
}

impl History {
    pub fn record(
        &mut self,
        before: &Profile,
        after: &Profile,
        before_location: Location,
        after_location: Location,
        group: Option<u64>,
    ) -> Result<(), String> {
        if before == after {
            return Ok(());
        }
        let before_text = serde_json::to_string(before).map_err(|e| e.to_string())?;
        let after_text = serde_json::to_string(after).map_err(|e| e.to_string())?;
        // A plugin or another editor may have changed this profile meanwhile.
        if self
            .undo
            .iter()
            .rev()
            .find(|e| e.profile == before.id)
            .is_some_and(|e| e.after != before_text)
        {
            self.forget(&before.id);
        }
        self.redo.retain(|e| e.profile != before.id);
        if before_text.capacity() + after_text.capacity() > MAX_BYTES {
            self.forget(&before.id);
            return Err("This change is too large to keep in undo history".into());
        }
        let now = Instant::now();
        if let Some(last) = self.undo.back_mut()
            && group.is_some()
            && last.group == group
            && last.profile == before.id
            && last.after == before_text
            && last.before.capacity() + after_text.capacity() <= MAX_BYTES
            && now.duration_since(last.at) <= GROUP_DELAY
        {
            last.after = after_text;
            last.after_location = after_location;
            last.at = now;
            if last.before == last.after {
                self.undo.pop_back();
            }
        } else {
            self.undo.push_back(Edit {
                profile: before.id.clone(),
                before: before_text,
                after: after_text,
                before_location,
                after_location,
                group,
                at: now,
            });
        }
        while self.undo.len() + self.redo.len() > MAX_EDITS || self.bytes() > MAX_BYTES {
            if self.undo.pop_front().is_none() {
                self.redo.pop_front();
            }
        }
        Ok(())
    }

    pub fn step(
        &mut self,
        current: &Profile,
        direction: Direction,
    ) -> Result<Option<(Profile, Location)>, String> {
        let redo = matches!(direction, Direction::Redo);
        let source = if redo { &mut self.redo } else { &mut self.undo };
        let Some(index) = source.iter().rposition(|e| e.profile == current.id) else {
            return Ok(None);
        };
        let Some(edit) = source.get(index) else {
            return Ok(None);
        };
        let expected = if redo { &edit.before } else { &edit.after };
        if serde_json::to_string(current).map_err(|e| e.to_string())? != *expected {
            self.forget(&current.id);
            return Err("Undo history cleared: this profile changed outside the editor".into());
        }
        let text = if redo { &edit.after } else { &edit.before };
        let profile =
            serde_json::from_str(text).map_err(|e| format!("Cannot restore edit: {e}"))?;
        let location = if redo {
            &edit.after_location
        } else {
            &edit.before_location
        }
        .clone();
        if let Some(mut edit) = source.remove(index) {
            // Typing after an undo starts a new edit, even in the same field.
            edit.group = None;
            if redo {
                self.undo.push_back(edit);
            } else {
                self.redo.push_back(edit);
            }
        }
        Ok(Some((profile, location)))
    }

    fn forget(&mut self, profile: &str) {
        self.undo.retain(|e| e.profile != profile);
        self.redo.retain(|e| e.profile != profile);
    }

    fn bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|e| e.before.capacity() + e.after.capacity())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deckly_core::profile::FOLDER_ACTION;
    use deckly_core::{DeviceKind, KeyConfig, Page};
    use serde_json::json;

    fn record(history: &mut History, before: &Profile, after: &Profile, group: Option<u64>) {
        history
            .record(
                before,
                after,
                Location::default(),
                Location::default(),
                group,
            )
            .unwrap();
    }

    #[test]
    fn undo_redo_restores_deleted_pages_folders_and_key_settings() {
        let mut before = Profile::new("Test", DeviceKind::Mk2);
        before.pages.push(Page::new());
        before.pages[1]
            .keys
            .insert(2, KeyConfig::new(FOLDER_ACTION, json!({})));
        before.normalize_folders();
        let folder = before.pages[1].keys[&2].folder().unwrap().to_owned();
        before
            .folder_mut(&folder)
            .unwrap()
            .keys
            .insert(1, KeyConfig::new("plugin.action", json!({"value":42})));
        let mut after = before.clone();
        after.pages.pop();
        after.normalize_folders();
        let location = Location {
            page: 1,
            path: vec![folder],
            key: Some(1),
        };
        let mut history = History::default();
        history
            .record(&before, &after, location.clone(), Location::default(), None)
            .unwrap();
        let (restored, selected) = history.step(&after, Direction::Undo).unwrap().unwrap();
        assert_eq!(restored, before);
        assert_eq!(selected, location);
        let (redone, _) = history.step(&restored, Direction::Redo).unwrap().unwrap();
        assert_eq!(redone, after);
    }

    #[test]
    fn histories_are_independent_and_new_edits_invalidate_only_their_redo() {
        let a = Profile::new("A", DeviceKind::Mk2);
        let b = Profile::new("B", DeviceKind::Mk2);
        let mut aa = a.clone();
        aa.name = "AA".into();
        let mut bb = b.clone();
        bb.name = "BB".into();
        let mut history = History::default();
        record(&mut history, &a, &aa, None);
        record(&mut history, &b, &bb, None);
        assert_eq!(history.step(&aa, Direction::Undo).unwrap().unwrap().0, a);
        assert_eq!(history.step(&bb, Direction::Undo).unwrap().unwrap().0, b);
        let mut newer = a.clone();
        newer.name = "New".into();
        record(&mut history, &a, &newer, None);
        assert!(history.step(&newer, Direction::Redo).unwrap().is_none());
        assert_eq!(history.step(&b, Direction::Redo).unwrap().unwrap().0, bb);
    }

    #[test]
    fn typing_groups_but_separate_clicks_and_undo_boundaries_do_not() {
        let a = Profile::new("", DeviceKind::Mk2);
        let mut b = a.clone();
        b.name = "A".into();
        let mut c = b.clone();
        c.name = "AB".into();
        let mut history = History::default();
        record(&mut history, &a, &b, Some(1));
        record(&mut history, &b, &c, Some(1));
        assert_eq!(history.undo.len(), 1);
        assert_eq!(history.step(&c, Direction::Undo).unwrap().unwrap().0, a);
        history.step(&a, Direction::Redo).unwrap();
        let mut d = c.clone();
        d.name = "ABC".into();
        record(&mut history, &c, &d, Some(1));
        assert_eq!(history.undo.len(), 2);
        let mut e = d.clone();
        e.name = "Click".into();
        record(&mut history, &d, &e, None);
        assert_eq!(history.undo.len(), 3);
    }

    #[test]
    fn external_changes_are_not_overwritten_and_old_edits_are_bounded() {
        let a = Profile::new("Test", DeviceKind::Mk2);
        let mut current = a.clone();
        let mut history = History::default();
        for index in 0..MAX_EDITS + 5 {
            let before = current.clone();
            current.name = index.to_string();
            record(&mut history, &before, &current, None);
        }
        assert_eq!(history.undo.len(), MAX_EDITS);
        current.name = "Changed by plugin".into();
        assert!(history.step(&current, Direction::Undo).is_err());
        assert!(history.undo.is_empty());
        assert!(history.redo.is_empty());
    }

    #[test]
    fn oversized_edit_clears_stale_history_and_never_exceeds_the_budget() {
        let a = Profile::new("Test", DeviceKind::Mk2);
        let mut b = a.clone();
        b.name = "Changed".into();
        let mut history = History::default();
        record(&mut history, &a, &b, None);
        let mut huge = b.clone();
        huge.name = "x".repeat(MAX_BYTES);
        assert!(
            history
                .record(&b, &huge, Location::default(), Location::default(), None)
                .is_err()
        );
        assert_eq!(history.bytes(), 0);
    }

    #[test]
    fn snapshot_allocations_evict_old_edits_at_the_byte_limit() {
        let mut current = Profile::new("Test", DeviceKind::Mk2);
        current.pages[0].keys.insert(
            0,
            KeyConfig::new("plugin.action", json!({"data": "x".repeat(1024 * 1024)})),
        );
        let mut history = History::default();
        for index in 0..20 {
            let before = current.clone();
            current.name = index.to_string();
            record(&mut history, &before, &current, None);
            assert!(history.bytes() <= MAX_BYTES);
        }
        assert!(history.undo.len() < 20);
        assert!(history.step(&current, Direction::Undo).unwrap().is_some());
    }
}

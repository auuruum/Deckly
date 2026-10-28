//! A copied key, including a snapshot of the folders it opens.

use deckly_core::profile::{MAX_FILE_BYTES, MAX_FOLDER_DEPTH, MAX_FOLDERS, new_id};
use deckly_core::{KeyConfig, Page, Profile};
use std::collections::HashMap;

const PREFIX: &str = "Deckly action\n";

#[derive(Clone)]
pub struct KeyClipboard {
    profile: Profile,
}

pub fn page<'a>(profile: &'a Profile, id: &str) -> Option<&'a Page> {
    profile
        .pages
        .iter()
        .chain(&profile.folders)
        .find(|p| p.id == id)
}

fn page_mut<'a>(profile: &'a mut Profile, id: &str) -> Option<&'a mut Page> {
    profile
        .pages
        .iter_mut()
        .chain(&mut profile.folders)
        .find(|p| p.id == id)
}

impl KeyClipboard {
    pub fn copy(profile: &Profile, page_id: &str, index: u8) -> Option<Self> {
        let key = page(profile, page_id)?.keys.get(&index)?;
        if key.action == deckly_core::profile::PARENT_FOLDER_ACTION {
            return None;
        }
        let mut snapshot = Profile::new("Copied action", profile.device);
        snapshot.pages.first_mut()?.keys.insert(0, key.clone());
        if key.folder().is_some() {
            snapshot.folders = profile.folders.clone();
            snapshot.normalize_folders();
        }
        Some(Self { profile: snapshot })
    }

    pub fn text(&self) -> Result<String, String> {
        let json = self.profile.to_json().map_err(|e| e.to_string())?;
        if json.len() as u64 > MAX_FILE_BYTES {
            return Err("Copied action is too large".into());
        }
        Ok(format!("{PREFIX}{json}"))
    }

    /// The OS clipboard is untrusted, even when it has our prefix.
    pub fn from_text(text: &str) -> Result<Option<Self>, String> {
        let Some(json) = text.strip_prefix(PREFIX) else {
            return Ok(None);
        };
        if json.len() as u64 > MAX_FILE_BYTES {
            return Err("Copied action is too large".into());
        }
        let profile = Profile::from_json(json).map_err(|e| e.to_string())?;
        let valid = profile.pages.len() == 1
            && profile.pages.first().is_some_and(|p| {
                p.keys.len() == 1
                    && p.keys
                        .get(&0)
                        .is_some_and(|k| k.action != deckly_core::profile::PARENT_FOLDER_ACTION)
            });
        if !valid {
            return Err("Clipboard does not contain a single Deckly action".into());
        }
        Ok(Some(Self { profile }))
    }

    /// Apply to a temporary profile so a limit or a stale target loses no data.
    pub fn paste(
        &self,
        profile: &mut Profile,
        page_id: &str,
        index: u8,
        folder_depth: usize,
    ) -> Result<(), String> {
        if index >= profile.model().key_count() || page(profile, page_id).is_none() {
            return Err("The destination key is no longer available".into());
        }
        if page(profile, page_id)
            .and_then(|p| p.keys.get(&index))
            .is_some_and(|k| k.action == deckly_core::profile::PARENT_FOLDER_ACTION)
        {
            return Err("The Parent Folder key cannot be replaced".into());
        }
        let mut copy = self.profile.clone();
        // Normalize using the destination model's key count as well.
        copy.device = profile.device;
        copy.sanitize();
        let mut key = copy
            .pages
            .first_mut()
            .and_then(|p| p.keys.remove(&0))
            .ok_or("Clipboard action is missing")?;
        if folder_depth + depth(&copy, &key) > MAX_FOLDER_DEPTH {
            return Err("This folder would exceed the folder nesting limit".into());
        }
        let ids: HashMap<String, String> = copy
            .folders
            .iter()
            .map(|p| (p.id.clone(), new_id()))
            .collect();
        remap_folder(&mut key, &ids);
        for folder in &mut copy.folders {
            if let Some(id) = ids.get(&folder.id) {
                folder.id = id.clone();
            }
            for key in folder.keys.values_mut() {
                remap_folder(key, &ids);
            }
        }
        let mut updated = profile.clone();
        if let Some(target) = page_mut(&mut updated, page_id) {
            target.keys.remove(&index);
        }
        updated.normalize_folders();
        if updated.folders.len() + copy.folders.len() > MAX_FOLDERS {
            return Err("This action would exceed the profile's folder limit".into());
        }
        let target = page_mut(&mut updated, page_id)
            .ok_or("The destination folder is no longer available")?;
        target.keys.insert(index, key);
        updated.folders.extend(copy.folders);
        updated.normalize_folders();
        *profile = updated;
        Ok(())
    }
}

fn remap_folder(key: &mut KeyConfig, ids: &HashMap<String, String>) {
    if let Some(id) = key.folder().and_then(|id| ids.get(id)).cloned()
        && let Some(settings) = key.settings.as_object_mut()
    {
        settings.insert("folder".into(), id.into());
    }
}

fn depth(profile: &Profile, key: &KeyConfig) -> usize {
    key.folder()
        .and_then(|id| profile.folder(id))
        .map_or(0, |folder| {
            1 + folder
                .keys
                .values()
                .map(|k| depth(profile, k))
                .max()
                .unwrap_or(0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use deckly_core::profile::{FOLDER_ACTION, PARENT_FOLDER_ACTION};
    use deckly_core::{DeviceKind, TitleAlign, TitleStyle};
    use serde_json::json;

    fn source() -> Profile {
        let mut profile = Profile::new("Source", DeviceKind::Mk2);
        let mut key = KeyConfig::new(
            "org.example.plugin.action",
            json!({"nested": [1, {"on": true}]}),
        );
        key.title = "Copied title".into();
        key.image = Some("custom.png".into());
        key.title_style = Some(TitleStyle {
            align: TitleAlign::Top,
            ..Default::default()
        });
        profile.pages[0].keys.insert(3, key);
        profile
    }

    #[test]
    fn clipboard_round_trip_preserves_the_entire_action_across_profiles_and_pages() {
        let source = source();
        let copy = KeyClipboard::copy(&source, &source.pages[0].id, 3).unwrap();
        let copy = KeyClipboard::from_text(&copy.text().unwrap())
            .unwrap()
            .unwrap();
        let mut target = Profile::new("Target", DeviceKind::Mini);
        target.pages.push(Page::new());
        let page_id = target.pages[1].id.clone();
        copy.paste(&mut target, &page_id, 5, 0).unwrap();
        assert_eq!(target.pages[1].keys[&5], source.pages[0].keys[&3]);
        assert!(target.pages[0].keys.is_empty());
    }

    #[test]
    fn folders_are_independent_snapshots_even_after_the_original_is_deleted() {
        let mut source = source();
        source.pages[0]
            .keys
            .insert(0, KeyConfig::new(FOLDER_ACTION, json!({})));
        source.normalize_folders();
        let outer = source.pages[0].keys[&0].folder().unwrap().to_owned();
        let folder = source.folder_mut(&outer).unwrap();
        folder
            .keys
            .insert(1, KeyConfig::new(FOLDER_ACTION, json!({})));
        source.normalize_folders();
        let inner = source.folder(&outer).unwrap().keys[&1]
            .folder()
            .unwrap()
            .to_owned();
        let action = source.pages[0].keys[&3].clone();
        source
            .folder_mut(&inner)
            .unwrap()
            .keys
            .insert(2, action.clone());
        let copy = KeyClipboard::copy(&source, &source.pages[0].id, 0).unwrap();
        source.pages[0].keys.remove(&0);
        source.normalize_folders();
        assert!(source.folders.is_empty());
        let page_id = source.pages[0].id.clone();
        for index in [0, 1] {
            copy.paste(&mut source, &page_id, index, 0).unwrap();
        }
        assert_eq!(source.folders.len(), 4);
        let a = source.pages[0].keys[&0].folder().unwrap();
        let b = source.pages[0].keys[&1].folder().unwrap();
        assert_ne!(a, b);
        for id in [a, b] {
            assert_ne!(id, outer);
            let folder = source.folder(id).unwrap();
            let inner_id = folder.keys[&1].folder().unwrap();
            assert_ne!(inner_id, inner);
            assert_eq!(source.folder(inner_id).unwrap().keys[&2], action);
        }
    }

    #[test]
    fn invalid_targets_and_parent_folder_keys_never_change_the_profile() {
        let source = source();
        let copy = KeyClipboard::copy(&source, &source.pages[0].id, 3).unwrap();
        assert!(KeyClipboard::copy(&source, &source.pages[0].id, 4).is_none());
        let mut target = source.clone();
        target.pages[0]
            .keys
            .insert(0, KeyConfig::new(PARENT_FOLDER_ACTION, json!({})));
        let original = target.clone();
        let id = target.pages[0].id.clone();
        assert!(KeyClipboard::copy(&target, &id, 0).is_none());
        for (page, index) in [(id.as_str(), 0), (id.as_str(), 255), ("missing", 1)] {
            assert!(copy.paste(&mut target, page, index, 0).is_err());
            assert_eq!(target, original);
        }
    }

    #[test]
    fn untrusted_clipboard_is_bounded_and_regular_text_is_ignored() {
        assert!(KeyClipboard::from_text("ordinary text").unwrap().is_none());
        assert!(KeyClipboard::from_text("Deckly action\n{broken").is_err());
        assert!(
            KeyClipboard::from_text(&format!(
                "{PREFIX}{}",
                " ".repeat(MAX_FILE_BYTES as usize + 1)
            ))
            .is_err()
        );
        let empty = Profile::new("Empty", DeviceKind::Mk2);
        assert!(KeyClipboard::from_text(&format!("{PREFIX}{}", empty.to_json().unwrap())).is_err());
        let mut parent = empty;
        parent.pages[0]
            .keys
            .insert(0, KeyConfig::new(PARENT_FOLDER_ACTION, json!({})));
        assert!(
            KeyClipboard::from_text(&format!("{PREFIX}{}", parent.to_json().unwrap())).is_err()
        );
    }

    #[test]
    fn folder_limits_fail_without_overwriting_the_destination() {
        let mut source = source();
        source.pages[0]
            .keys
            .insert(0, KeyConfig::new(FOLDER_ACTION, json!({})));
        source.normalize_folders();
        let copy = KeyClipboard::copy(&source, &source.pages[0].id, 0).unwrap();
        let page_id = source.pages[0].id.clone();
        let original = source.clone();
        assert!(
            copy.paste(&mut source, &page_id, 3, MAX_FOLDER_DEPTH)
                .is_err()
        );
        assert_eq!(source, original);

        let mut target = Profile::new("Full", DeviceKind::Mk2);
        for n in 0..MAX_FOLDERS {
            if n % 15 == 0 {
                target.pages.push(Page::new());
            }
            let page = target.pages.last_mut().unwrap();
            page.keys
                .insert((n % 15) as u8, KeyConfig::new(FOLDER_ACTION, json!({})));
        }
        target.normalize_folders();
        assert_eq!(target.folders.len(), MAX_FOLDERS);
        let page_id = target.pages[0].id.clone();
        let original = target.clone();
        assert!(copy.paste(&mut target, &page_id, 1, 0).is_err());
        assert_eq!(target, original);
        // Replacing a folder can reuse the capacity it releases.
        let page_id = target.pages[1].id.clone();
        copy.paste(&mut target, &page_id, 0, 0).unwrap();
        assert_eq!(target.folders.len(), MAX_FOLDERS);
    }
}

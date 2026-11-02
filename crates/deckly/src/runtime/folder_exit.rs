//! Folder Auto Exit. Preferences belong to keys; deadlines belong to the
//! runtime and never get saved. See docs/clean-room/2026-11-02-folder-auto-exit.md.

use super::{State, actions};
use deckly_core::{AutoExit, KeyConfig};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The countdown ring moves on at most this often, for short delays.
const MIN_FRAME: Duration = Duration::from_millis(50);

#[derive(Default)]
pub(super) struct Timers {
    revisions: HashMap<String, u64>,
    pending: HashMap<String, Timer>,
}

struct Timer {
    source: Source,
    at: Instant,
}

impl Timer {
    fn length(&self) -> Duration {
        Duration::from_secs(u64::from(self.source.policy.delay))
    }
}

/// The location and policy of the actual press, rather than whatever key
/// happens to be at that position when a release or sequence arrives.
#[derive(Clone)]
pub(super) struct Source {
    serial: String,
    profile: String,
    page: String,
    path: Vec<String>,
    revision: u64,
    index: u8,
    action: String,
    policy: AutoExit,
}

impl Timers {
    pub(super) fn invalidate(&mut self, profile: &str) {
        let revision = self.revisions.entry(profile.to_owned()).or_default();
        *revision = revision.saturating_add(1);
        self.pending.remove(profile);
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.pending.values().map(|t| t.at).min()
    }

    /// How far a profile's countdown is, in percent.
    pub(super) fn progress(&self, profile: &str, now: Instant) -> Option<u8> {
        let timer = self.pending.get(profile)?;
        let left = timer.at.saturating_duration_since(now).as_millis() * 100;
        let left = left.checked_div(timer.length().as_millis())?;
        // left is at most 100.
        Some(100u128.saturating_sub(left) as u8)
    }

    /// When a countdown's ring next changes: one percent on, so a key image
    /// goes out no more often than it differs.
    pub(super) fn next_frame(&self, now: Instant) -> Option<Instant> {
        let frame = self.pending.values().map(|t| t.length() / 100).min()?;
        Some(now + frame.max(MIN_FRAME))
    }

    fn revision(&self, profile: &str) -> u64 {
        self.revisions.get(profile).copied().unwrap_or_default()
    }
}

fn eligible(action: &str) -> bool {
    !matches!(
        action,
        actions::CREATE_FOLDER
            | actions::PARENT_FOLDER
            | actions::SWITCH_PROFILE
            | actions::NEXT_PAGE
            | actions::PREVIOUS_PAGE
            | actions::GO_TO_PAGE
            | actions::PAGE_INDICATOR
            | actions::DELAY
    )
}

impl State {
    pub(super) fn folder_exit_source(
        &self,
        serial: &str,
        index: u8,
        key: &KeyConfig,
    ) -> Option<Source> {
        if !eligible(&key.action) {
            return None;
        }
        let profile = self.assigned(serial)?;
        let path = self.folder_path(profile);
        let folder = profile.folder(path.last()?)?;
        let mut policy = key.auto_exit.unwrap_or_else(|| {
            folder
                .keys
                .values()
                .find(|key| key.action == actions::PARENT_FOLDER)
                .and_then(|key| key.auto_exit)
                .unwrap_or_default()
        });
        policy.sanitize();
        Some(Source {
            serial: serial.to_owned(),
            profile: profile.id.clone(),
            page: profile.pages.get(self.page(profile))?.id.clone(),
            path,
            revision: self.folder_exit.revision(&profile.id),
            index,
            action: key.action.clone(),
            policy,
        })
    }

    fn valid_folder_exit(&self, source: &Source) -> bool {
        if self.gate.locked_out()
            || !self.settings.device(&source.serial).enabled
            || self.folder_exit.revision(&source.profile) != source.revision
            || !self.devices.iter().any(|d| d.serial == source.serial)
        {
            return false;
        }
        let Some(profile) = self.assigned(&source.serial) else {
            return false;
        };
        if profile.id != source.profile
            || profile.pages.get(self.page(profile)).map(|p| &p.id) != Some(&source.page)
            || self.folder_path(profile) != source.path
        {
            return false;
        }
        let Some(key) = self.shown(profile).and_then(|p| p.keys.get(&source.index)) else {
            return false;
        };
        key.action == source.action
            && self
                .folder_exit_source(&source.serial, source.index, key)
                .is_some_and(|current| current.policy == source.policy)
    }

    pub(super) fn pause_folder_exit(&mut self, source: &mut Source) {
        // A newer press supersedes unfinished sequences and earlier held
        // keys too, so their eventual completion cannot replace its policy.
        self.folder_exit.invalidate(&source.profile);
        source.revision = self.folder_exit.revision(&source.profile);
    }

    pub(super) fn arm_folder_exit(&mut self, source: Source, now: Instant) {
        if !self.valid_folder_exit(&source) {
            return;
        }
        self.folder_exit.pending.remove(&source.profile);
        if source.policy.enabled {
            let at = now + Duration::from_secs(u64::from(source.policy.delay));
            self.folder_exit
                .pending
                .insert(source.profile.clone(), Timer { source, at });
        }
    }

    pub(super) fn cancel_device_folder_exit(&mut self, serial: &str) {
        if let Some(profile) = self.assigned(serial).map(|p| p.id.clone()) {
            self.folder_exit.invalidate(&profile);
        }
    }

    /// Called before the event loop blocks. A single deadline wakes it; no
    /// polling, sleeping thread, or UI repaint loop is needed.
    pub(super) fn expire_folder_exits(&mut self, now: Instant) -> bool {
        let pending = std::mem::take(&mut self.folder_exit.pending);
        let mut changed = false;
        for (profile, timer) in pending {
            if !self.valid_folder_exit(&timer.source) {
                continue;
            }
            if now < timer.at {
                self.folder_exit.pending.insert(profile, timer);
                continue;
            }
            let mut path = timer.source.path;
            path.pop();
            self.set_folder_path(&profile, path);
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deckly_core::{DeviceKind, Profile, Settings};
    use deckly_device::ConnectedDevice;
    use serde_json::json;

    fn setup(delay: u8) -> (State, String, String) {
        let mut p = Profile::new("Auto Exit", DeviceKind::Mk2);
        p.pages[0]
            .keys
            .insert(1, KeyConfig::new(actions::CREATE_FOLDER, json!({})));
        p.normalize_folders();
        let folder = p.pages[0].keys[&1].folder().unwrap().to_owned();
        let f = p.folder_mut(&folder).unwrap();
        f.keys.get_mut(&0).unwrap().auto_exit = Some(AutoExit {
            enabled: true,
            delay,
        });
        f.keys
            .insert(1, KeyConfig::new(actions::TEXT, json!({"text":"test"})));
        let id = p.id.clone();
        let device = ConnectedDevice {
            handle: 1,
            serial: "TEST".into(),
            model: p.model(),
            firmware: None,
        };
        let mut s = State::new(vec![p], Settings::default());
        s.devices.push(device);
        s.settings.assignments.insert("TEST".into(), id.clone());
        s.set_folder_path(&id, vec![folder.clone()]);
        (s, id, folder)
    }

    fn source(s: &State) -> Source {
        let key = s.key("TEST", 1).unwrap();
        s.folder_exit_source("TEST", 1, &key).unwrap()
    }

    #[test]
    fn entering_a_folder_does_not_arm_a_timer_and_an_action_uses_seconds() {
        let (mut s, id, folder) = setup(3);
        let now = Instant::now();
        assert_eq!(s.folder_exit.deadline(), None);
        assert!(!s.expire_folder_exits(now + Duration::from_secs(100)));
        s.arm_folder_exit(source(&s), now);
        assert_eq!(s.folder_exit.deadline(), Some(now + Duration::from_secs(3)));
        assert!(!s.expire_folder_exits(now + Duration::from_millis(2999)));
        assert_eq!(s.folders[&id], [folder]);
        assert!(s.expire_folder_exits(now + Duration::from_secs(3)));
        assert!(s.folders.is_empty());
        assert_eq!(s.folder_exit.deadline(), None);
    }

    #[test]
    fn a_countdown_reports_its_progress_and_stops_with_its_timer() {
        let (mut s, id, _) = setup(10);
        let now = Instant::now();
        let progress = |s: &State, ms| s.folder_exit.progress(&id, now + Duration::from_millis(ms));
        assert_eq!(progress(&s, 0), None);
        assert_eq!(s.folder_exit.next_frame(now), None);
        s.arm_folder_exit(source(&s), now);
        assert_eq!(progress(&s, 0), Some(0));
        assert_eq!(progress(&s, 5000), Some(50));
        assert_eq!(progress(&s, 10_000), Some(100));
        assert_eq!(progress(&s, 60_000), Some(100));
        assert_eq!(
            s.live(s.profile(&id).unwrap()).exit.map(|p| p < 100),
            Some(true)
        );
        // Ten seconds move one percent every tenth of a second.
        assert_eq!(
            s.folder_exit.next_frame(now),
            Some(now + Duration::from_millis(100))
        );
        let mut again = source(&s);
        s.pause_folder_exit(&mut again);
        assert_eq!(progress(&s, 0), None);
    }

    #[test]
    fn a_short_countdown_is_limited_in_frames_and_zero_has_none() {
        let now = Instant::now();
        let (mut s, _, _) = setup(1);
        s.arm_folder_exit(source(&s), now);
        assert_eq!(s.folder_exit.next_frame(now), Some(now + MIN_FRAME));
        let (mut s, id, _) = setup(0);
        s.arm_folder_exit(source(&s), now);
        assert_eq!(s.folder_exit.progress(&id, now), None);
    }

    #[test]
    fn zero_is_immediate_and_returns_only_one_level() {
        let (mut s, id, outer) = setup(0);
        let p = s.profile_mut(&id).unwrap();
        p.folder_mut(&outer)
            .unwrap()
            .keys
            .insert(2, KeyConfig::new(actions::CREATE_FOLDER, json!({})));
        p.normalize_folders();
        let inner = p.folder(&outer).unwrap().keys[&2]
            .folder()
            .unwrap()
            .to_owned();
        let f = p.folder_mut(&inner).unwrap();
        f.keys.get_mut(&0).unwrap().auto_exit = Some(AutoExit {
            enabled: true,
            delay: 0,
        });
        f.keys.insert(1, KeyConfig::new(actions::TEXT, json!({})));
        s.set_folder_path(&id, vec![outer.clone(), inner]);
        let now = Instant::now();
        s.arm_folder_exit(source(&s), now);
        assert!(s.expire_folder_exits(now));
        assert_eq!(s.folders[&id], [outer]);
        assert!(!s.expire_folder_exits(now + Duration::from_secs(100)));
    }

    #[test]
    fn another_action_restarts_the_countdown_and_a_disabled_override_cancels_it() {
        let (mut s, id, folder) = setup(3);
        let now = Instant::now();
        s.arm_folder_exit(source(&s), now);
        let mut again = source(&s);
        s.pause_folder_exit(&mut again);
        assert_eq!(s.folder_exit.deadline(), None);
        s.arm_folder_exit(again, now + Duration::from_secs(2));
        assert!(!s.expire_folder_exits(now + Duration::from_secs(3)));
        s.profile_mut(&id)
            .unwrap()
            .folder_mut(&folder)
            .unwrap()
            .keys
            .get_mut(&1)
            .unwrap()
            .auto_exit = Some(AutoExit::default());
        s.arm_folder_exit(source(&s), now + Duration::from_secs(3));
        assert_eq!(s.folder_exit.deadline(), None);
        assert!(!s.expire_folder_exits(now + Duration::from_secs(10)));
    }

    #[test]
    fn an_override_can_enable_exit_when_the_folder_is_disabled() {
        let (mut s, id, folder) = setup(3);
        let f = s.profile_mut(&id).unwrap().folder_mut(&folder).unwrap();
        f.keys.get_mut(&0).unwrap().auto_exit = None;
        f.keys.get_mut(&1).unwrap().auto_exit = Some(AutoExit {
            enabled: true,
            delay: 1,
        });
        let now = Instant::now();
        s.arm_folder_exit(source(&s), now);
        assert!(s.expire_folder_exits(now + Duration::from_secs(1)));
    }

    #[test]
    fn leaving_and_reentering_a_folder_invalidates_even_an_unfinished_action() {
        let (mut s, id, folder) = setup(0);
        let old = source(&s);
        s.set_folder_path(&id, Vec::new());
        s.set_folder_path(&id, vec![folder]);
        s.arm_folder_exit(old, Instant::now());
        assert_eq!(s.folder_exit.deadline(), None);
    }

    #[test]
    fn editing_disabling_or_removing_the_source_cancels_a_pending_exit() {
        for remove in [false, true] {
            let (mut s, id, folder) = setup(3);
            let now = Instant::now();
            s.arm_folder_exit(source(&s), now);
            let f = s.profile_mut(&id).unwrap().folder_mut(&folder).unwrap();
            if remove {
                f.keys.remove(&1);
            } else {
                f.keys.get_mut(&0).unwrap().auto_exit = None;
            }
            assert!(!s.expire_folder_exits(now + Duration::from_secs(3)));
            assert_eq!(s.folder_exit.deadline(), None);
        }
    }

    #[test]
    fn disconnect_disable_and_profile_switch_cancel_deadlines() {
        for mode in 0..3 {
            let (mut s, _, _) = setup(1);
            let now = Instant::now();
            s.arm_folder_exit(source(&s), now);
            match mode {
                0 => s.devices.clear(),
                1 => s.settings.device_mut("TEST").enabled = false,
                _ => {
                    s.settings.assignments.insert("TEST".into(), "gone".into());
                }
            }
            assert!(!s.expire_folder_exits(now + Duration::from_secs(1)));
            assert_eq!(s.folder_exit.deadline(), None);
        }
    }

    #[test]
    fn navigation_and_page_indicators_never_start_auto_exit() {
        let (s, _, _) = setup(0);
        for action in [
            actions::CREATE_FOLDER,
            actions::PARENT_FOLDER,
            actions::NEXT_PAGE,
            actions::PREVIOUS_PAGE,
            actions::GO_TO_PAGE,
            actions::SWITCH_PROFILE,
            actions::PAGE_INDICATOR,
        ] {
            assert!(
                s.folder_exit_source("TEST", 1, &KeyConfig::new(action, json!({})))
                    .is_none()
            );
        }
    }

    #[test]
    fn newer_interactions_invalidate_older_completions_and_releases() {
        let (mut s, id, folder) = setup(0);
        let mut older = source(&s);
        s.pause_folder_exit(&mut older);
        let f = s.profile_mut(&id).unwrap().folder_mut(&folder).unwrap();
        f.keys.insert(2, KeyConfig::new(actions::TEXT, json!({})));
        f.keys.get_mut(&2).unwrap().auto_exit = Some(AutoExit::default());
        let key = s.key("TEST", 2).unwrap();
        let mut newer = s.folder_exit_source("TEST", 2, &key).unwrap();
        s.pause_folder_exit(&mut newer);
        let now = Instant::now();
        // Completion of an earlier sequence while the new key is held.
        s.arm_folder_exit(older.clone(), now);
        assert_eq!(s.folder_exit.deadline(), None);
        // A disabled override wins over an earlier key released later too.
        s.arm_folder_exit(newer, now);
        s.arm_folder_exit(older, now);
        assert_eq!(s.folder_exit.deadline(), None);
    }

    #[test]
    fn release_keeps_its_original_action_after_navigation_and_empty_keys_clear() {
        use super::super::{Link, Links, key_event, lock};
        use std::sync::{Arc, Mutex};
        let (s, id, _) = setup(0);
        let links = Links::from([(1, Link::new(&s.devices[0]))]);
        let state = Arc::new(Mutex::new(s));
        let mut links = links;
        let devices = deckly_device::DeviceManager::disabled();
        let hit = key_event(&state, &devices, &mut links, 1, 1, true).unwrap();
        lock(&state).set_folder_path(&id, Vec::new());
        let released = key_event(&state, &devices, &mut links, 1, 1, false).unwrap();
        assert_eq!(released.config, hit.config);
        assert_eq!(released.context, hit.context);
        assert_eq!(released.config.action, actions::TEXT);
        assert!(lock(&state).pressed.is_empty());
        assert!(key_event(&state, &devices, &mut links, 1, 14, true).is_none());
        assert!(key_event(&state, &devices, &mut links, 1, 14, false).is_none());
        assert!(lock(&state).pressed.is_empty());
    }

    #[test]
    fn sequences_arm_after_completion_and_cancellation_never_arms() {
        use super::super::{Groups, KeyHit, Msg, Tools, finish_group_run, lock, start_run};
        use std::sync::{Arc, Mutex, mpsc::channel};
        for cancelled in [false, true] {
            let (mut s, id, folder) = setup(0);
            let mut key = KeyConfig::new(actions::MULTI_ACTION, json!({}));
            key.groups[0].push(KeyConfig::new(actions::DELAY, json!({"delay":1})));
            s.profile_mut(&id)
                .unwrap()
                .folder_mut(&folder)
                .unwrap()
                .keys
                .insert(1, key.clone());
            let mut exit = source(&s);
            s.pause_folder_exit(&mut exit);
            let hit = KeyHit {
                config: key,
                index: 1,
                plugin: false,
                context: "folder.1".into(),
                exit: Some(exit),
            };
            let state = Arc::new(Mutex::new(s));
            let (actions, _) = channel();
            let (results, rx) = channel();
            let links = HashMap::new();
            let tools = Tools {
                state: &state,
                actions: &actions,
                host: None,
                links: &links,
                results: &results,
            };
            let mut groups = Groups::default();
            start_run(&tools, &mut groups, 1, &hit);
            assert_eq!(lock(&state).folder_exit.deadline(), None);
            let Msg::RunDone { context, .. } = rx.recv_timeout(Duration::from_secs(2)).unwrap()
            else {
                panic!("sequence did not finish")
            };
            let now = Instant::now();
            finish_group_run(&state, &mut groups, &context, cancelled, now);
            assert_eq!(lock(&state).expire_folder_exits(now), !cancelled);
        }
    }

    #[test]
    fn key_logic_double_press_uses_the_latest_press_and_exits_after_release() {
        use super::super::{Groups, KeyHit, Tools, group_key, lock};
        use std::sync::{Arc, Mutex, mpsc::channel};
        let (mut s, id, folder) = setup(0);
        let mut key = KeyConfig::new(actions::KEY_LOGIC, json!({}));
        key.groups[crate::sequence::DOUBLE].push(KeyConfig::new(actions::BRIGHTNESS, json!({})));
        s.profile_mut(&id)
            .unwrap()
            .folder_mut(&folder)
            .unwrap()
            .keys
            .insert(1, key.clone());
        let state = Arc::new(Mutex::new(s));
        let (actions, rx) = channel();
        let (results, _) = channel();
        let links = HashMap::new();
        let tools = Tools {
            state: &state,
            actions: &actions,
            host: None,
            links: &links,
            results: &results,
        };
        let mut groups = Groups::default();
        for pressed in [true, false, true, false] {
            let mut exit = source(&lock(&state));
            if pressed {
                lock(&state).pause_folder_exit(&mut exit);
            }
            let hit = KeyHit {
                config: key.clone(),
                index: 1,
                plugin: false,
                context: "folder.1".into(),
                exit: Some(exit),
            };
            group_key(&tools, &mut groups, 1, hit, pressed);
            if pressed {
                assert_eq!(lock(&state).folder_exit.deadline(), None);
            }
        }
        assert_eq!(rx.try_recv().unwrap().2.action, actions::BRIGHTNESS);
        assert!(lock(&state).expire_folder_exits(Instant::now()));
    }
}

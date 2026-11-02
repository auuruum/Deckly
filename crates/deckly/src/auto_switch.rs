//! Shows a profile while an application it lists is in the foreground
//! (docs/clean-room/2026-11-02-active-window-profiles.md).
//!
//! Watchers in `os::active_window` report the foreground here and the runtime
//! asks what each device should show. A rule's profile lies over the device's
//! assignment and is never saved as one: going back is dropping it, a switch
//! by hand replaces it, and a restart finds the user's own choice.

use deckly_core::{DeviceKind, Profile};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long the foreground must stay put to count, so that alt-tab does not
/// re-image a device for every window it passes.
pub const SETTLE: Duration = Duration::from_millis(250);

pub struct Follower {
    /// Device serial -> the profile a rule shows on it.
    shown: HashMap<String, String>,
    /// The latest report and when it counts.
    pending: Option<(Vec<String>, Instant)>,
    /// The names of the settled foreground application.
    foreground: Vec<String>,
    enabled: bool,
    /// Rules were edited; the next report counts even for the same application.
    rules_changed: bool,
    /// Counts the changes devices must catch up with.
    turn: u64,
    /// Device serial -> the turn it followed last. A device that cannot be
    /// shown anything stays behind until it can.
    followed: HashMap<String, u64>,
    /// Why the foreground cannot be followed on this system.
    error: Option<String>,
}

impl Follower {
    pub fn new(enabled: bool) -> Self {
        Self {
            shown: HashMap::new(),
            pending: None,
            foreground: Vec::new(),
            enabled,
            rules_changed: false,
            turn: 0,
            followed: HashMap::new(),
            error: None,
        }
    }

    pub fn report(&mut self, names: Vec<String>, now: Instant) {
        self.pending = Some((names, now + SETTLE));
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// When a report settles, for the runtime to wake up.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, at)| *at)
    }

    /// Takes in a change of the setting and a report that has settled. Windows
    /// seen while the computer is locked do not count: the lock screen is no
    /// application left for another, and the same one is back after it.
    pub fn settle(&mut self, enabled: bool, locked: bool, now: Instant) {
        if locked {
            self.pending = None;
            return;
        }
        if self.enabled != enabled {
            self.enabled = enabled;
            self.turn += 1;
        }
        let due = self.pending.take_if(|(_, at)| *at <= now);
        if let Some((names, _)) = due
            && (std::mem::take(&mut self.rules_changed) || names != self.foreground)
        {
            self.foreground = names;
            self.turn += 1;
        }
    }

    /// Rules take effect when the user is back from editing them. Deckly's
    /// own windows are not reported, so that may be the same application.
    pub fn rules_changed(&mut self) {
        self.rules_changed = true;
    }

    /// The profile a rule shows on a device.
    pub fn shown(&self, serial: &str) -> Option<&str> {
        self.shown.get(serial).map(String::as_str)
    }

    /// The user picked a profile, which wins until the foreground changes.
    /// Returns the profile a rule was showing.
    pub fn manual(&mut self, serial: &str) -> Option<String> {
        self.shown.remove(serial)
    }

    /// A deleted profile no longer shows anywhere.
    pub fn forget_profile(&mut self, id: &str) {
        self.shown.retain(|_, shown| shown != id);
    }

    /// Brings a device up to date with the foreground, once per change, and
    /// returns whether what a rule shows on it changed.
    pub fn follow(
        &mut self,
        serial: &str,
        kind: DeviceKind,
        assigned: Option<&str>,
        profiles: &[Profile],
    ) -> bool {
        if self.followed.get(serial) == Some(&self.turn) {
            return false;
        }
        self.followed.insert(serial.to_owned(), self.turn);
        let wanted = self
            .enabled
            .then(|| winner(profiles, kind, &self.foreground))
            .flatten()
            .filter(|id| Some(*id) != assigned);
        if self.shown(serial) == wanted {
            return false;
        }
        match wanted {
            Some(id) => self.shown.insert(serial.to_owned(), id.to_owned()),
            None => self.shown.remove(serial),
        };
        true
    }
}

/// The first profile of this model, in list order, that lists the application.
fn winner<'a>(profiles: &'a [Profile], kind: DeviceKind, names: &[String]) -> Option<&'a str> {
    profiles
        .iter()
        .filter(|p| p.device == kind)
        .find(|p| p.applications.iter().any(|rule| rule.matches(names)))
        .map(|p| p.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deckly_core::AppRule;

    const SERIAL: &str = "S1";

    fn profile(id: &str, kind: DeviceKind, apps: &[&str]) -> Profile {
        let mut profile = Profile::new(id, kind);
        profile.id = id.to_owned();
        profile.applications = apps.iter().map(|a| AppRule::new(a, a)).collect();
        profile
    }

    /// A device with a chosen profile and three profiles of its model:
    /// `home` lists nothing, `game` lists game.exe, `chat` lists chat.exe.
    struct Desk {
        follower: Follower,
        profiles: Vec<Profile>,
        assigned: Option<String>,
        now: Instant,
    }

    impl Desk {
        fn new() -> Self {
            Self {
                follower: Follower::new(true),
                profiles: vec![
                    profile("home", DeviceKind::Mk2, &[]),
                    profile("game", DeviceKind::Mk2, &["game.exe"]),
                    profile("chat", DeviceKind::Mk2, &["chat.exe"]),
                ],
                assigned: Some("home".to_owned()),
                now: Instant::now(),
            }
        }

        /// Lets one application reach the foreground and stay there.
        fn focus(&mut self, app: &str) {
            self.follower.report(vec![app.to_owned()], self.now);
            self.settle();
        }

        fn settle(&mut self) {
            self.now += SETTLE;
            self.follower.settle(true, false, self.now);
            self.follow();
        }

        fn follow(&mut self) -> bool {
            let assigned = self.assigned.as_deref();
            self.follower
                .follow(SERIAL, DeviceKind::Mk2, assigned, &self.profiles)
        }

        fn by_hand(&mut self, id: &str) {
            self.follower.manual(SERIAL);
            self.assigned = Some(id.to_owned());
        }

        fn showing(&self) -> Option<&str> {
            self.follower.shown(SERIAL).or(self.assigned.as_deref())
        }
    }

    #[test]
    fn a_rule_shows_its_profile_and_another_application_goes_back() {
        let mut desk = Desk::new();
        desk.focus("C:\\Games\\game.exe");
        assert_eq!(desk.showing(), Some("game"));
        desk.focus("C:\\Windows\\explorer.exe");
        assert_eq!(desk.showing(), Some("home"));
    }

    #[test]
    fn going_from_rule_to_rule_still_returns_to_the_chosen_profile() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.focus("chat.exe");
        assert_eq!(desk.showing(), Some("chat"));
        desk.focus("other.exe");
        assert_eq!(desk.showing(), Some("home"));
    }

    #[test]
    fn a_switch_by_hand_stays_through_the_next_application() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.by_hand("chat");
        assert_eq!(desk.showing(), Some("chat"));
        desk.focus("other.exe");
        assert_eq!(desk.showing(), Some("chat"));
    }

    #[test]
    fn a_switch_by_hand_wins_until_the_foreground_changes() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.by_hand("home");
        // The same application reported again is no change.
        desk.focus("game.exe");
        assert_eq!(desk.showing(), Some("home"));
        desk.focus("chat.exe");
        assert_eq!(desk.showing(), Some("chat"));
        desk.focus("other.exe");
        assert_eq!(desk.showing(), Some("home"));
    }

    #[test]
    fn picking_the_shown_profile_by_hand_keeps_it_after_the_application() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.by_hand("game");
        desk.focus("other.exe");
        assert_eq!(desk.showing(), Some("game"));
    }

    #[test]
    fn a_profile_already_chosen_is_nothing_to_go_back_from() {
        let mut desk = Desk::new();
        desk.by_hand("game");
        desk.focus("game.exe");
        assert_eq!(desk.follower.shown(SERIAL), None);
        desk.focus("other.exe");
        assert_eq!(desk.showing(), Some("game"));
    }

    #[test]
    fn rapid_flips_only_count_where_they_end() {
        let mut desk = Desk::new();
        for app in ["chat.exe", "explorer.exe", "game.exe"] {
            desk.follower.report(vec![app.to_owned()], desk.now);
            desk.now += SETTLE / 10;
            desk.follower.settle(true, false, desk.now);
            assert!(!desk.follow(), "{app} was only passing through");
        }
        assert_eq!(
            desk.follower.deadline(),
            Some(desk.now - SETTLE / 10 + SETTLE)
        );
        desk.now += SETTLE;
        desk.follower.settle(true, false, desk.now);
        assert!(desk.follow());
        assert_eq!(desk.showing(), Some("game"));
        assert_eq!(desk.follower.deadline(), None);
    }

    #[test]
    fn returning_to_the_same_application_changes_nothing() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.follower.report(vec!["chat.exe".to_owned()], desk.now);
        desk.follower.report(vec!["game.exe".to_owned()], desk.now);
        desk.now += SETTLE;
        desk.follower.settle(true, false, desk.now);
        assert!(!desk.follow());
    }

    #[test]
    fn an_unnamed_application_counts_as_one_without_a_rule() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.follower.report(Vec::new(), desk.now);
        desk.settle();
        assert_eq!(desk.showing(), Some("home"));
    }

    #[test]
    fn profiles_of_another_model_are_ignored() {
        let mut desk = Desk::new();
        desk.profiles = vec![profile("mini", DeviceKind::Mini, &["game.exe"])];
        desk.focus("game.exe");
        assert_eq!(desk.showing(), Some("home"));
    }

    #[test]
    fn the_first_profile_in_the_list_wins_an_application_two_list() {
        let mut desk = Desk::new();
        desk.profiles
            .insert(1, profile("first", DeviceKind::Mk2, &["GAME.EXE"]));
        desk.focus("game.exe");
        assert_eq!(desk.showing(), Some("first"));
        desk.by_hand("home");
        desk.profiles.swap(1, 2);
        desk.focus("other.exe");
        desk.focus("game.exe");
        assert_eq!(desk.showing(), Some("game"));
    }

    #[test]
    fn a_deleted_profile_stops_showing() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.profiles.retain(|p| p.id != "game");
        desk.follower.forget_profile("game");
        assert_eq!(desk.showing(), Some("home"));
        desk.focus("game.exe");
        assert_eq!(desk.showing(), Some("home"));
    }

    #[test]
    fn devices_follow_on_their_own() {
        let mut desk = Desk::new();
        desk.follower.report(vec!["game.exe".to_owned()], desk.now);
        desk.now += SETTLE;
        desk.follower.settle(true, false, desk.now);
        let profiles = desk.profiles.clone();
        let f = &mut desk.follower;
        assert!(f.follow("A", DeviceKind::Mk2, Some("home"), &profiles));
        assert!(!f.follow("B", DeviceKind::Mk2, Some("game"), &profiles));
        assert!(f.follow("C", DeviceKind::Mk2, Some("chat"), &profiles));
        f.manual("A");
        assert_eq!(f.shown("A"), None);
        assert_eq!(f.shown("B"), None);
        assert_eq!(f.shown("C"), Some("game"));
    }

    /// The runtime does not ask for a device that sleeps or a locked computer.
    #[test]
    fn a_device_left_behind_catches_up_with_where_the_foreground_ended() {
        let mut desk = Desk::new();
        desk.follow();
        for app in ["chat.exe", "game.exe"] {
            desk.follower.report(vec![app.to_owned()], desk.now);
            desk.now += SETTLE;
            desk.follower.settle(true, false, desk.now);
        }
        assert_eq!(desk.follower.shown(SERIAL), None);
        assert!(desk.follow());
        assert_eq!(desk.showing(), Some("game"));
        assert!(!desk.follow(), "once per change");
    }

    #[test]
    fn the_lock_screen_does_not_count_as_an_application() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.follower
            .report(vec!["LockApp.exe".to_owned()], desk.now);
        desk.now += SETTLE;
        desk.follower.settle(true, true, desk.now);
        assert_eq!(desk.follower.deadline(), None);
        desk.follower.settle(true, false, desk.now);
        assert!(!desk.follow());
        assert_eq!(desk.showing(), Some("game"));
        // Unlocking, the application comes back with a report of its own.
        desk.focus("game.exe");
        assert_eq!(desk.showing(), Some("game"));
        desk.focus("other.exe");
        assert_eq!(desk.showing(), Some("home"));
    }

    #[test]
    fn turning_the_setting_off_goes_back_and_on_comes_back() {
        let mut desk = Desk::new();
        desk.focus("game.exe");
        desk.follower.settle(false, false, desk.now);
        assert!(desk.follow());
        assert_eq!(desk.showing(), Some("home"));
        desk.follower.settle(true, false, desk.now);
        assert!(desk.follow());
        assert_eq!(desk.showing(), Some("game"));
    }

    #[test]
    fn with_the_setting_off_nothing_switches() {
        let mut desk = Desk::new();
        desk.follower = Follower::new(false);
        desk.follower.report(vec!["game.exe".to_owned()], desk.now);
        desk.now += SETTLE;
        desk.follower.settle(false, false, desk.now);
        assert!(!desk.follow());
        assert_eq!(desk.showing(), Some("home"));
    }

    /// A rule added for the application the user came from, with only
    /// Deckly's own windows in between.
    #[test]
    fn edited_rules_count_on_the_next_report_of_the_same_application() {
        let mut desk = Desk::new();
        desk.focus("editor.exe");
        desk.profiles[1]
            .applications
            .push(AppRule::new("Editor", "editor.exe"));
        desk.follower.rules_changed();
        assert!(!desk.follow(), "not while the user is still editing");
        desk.focus("editor.exe");
        assert_eq!(desk.showing(), Some("game"));
    }
}

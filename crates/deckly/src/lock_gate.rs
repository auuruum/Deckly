//! Keeps keys from acting on the machine while the computer is locked
//! (docs/clean-room/2026-11-02-lock-keys.md).
//!
//! Platform watchers in `os::screen_lock` report the lock state here; the
//! runtime and sequences ask before running an action. The watchers report
//! only what they know: the gate starts unlocked and keeps the last known
//! state, so keys stay usable where the lock screen cannot be detected.

use crate::actions;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, Default)]
pub struct Gate(Arc<Flags>);

#[derive(Debug, Default)]
struct Flags {
    enabled: AtomicBool,
    locked: AtomicBool,
    undetected: AtomicBool,
}

impl Gate {
    pub fn new(enabled: bool) -> Self {
        let gate = Self::default();
        gate.set_enabled(enabled);
        gate
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn set_locked(&self, locked: bool) {
        self.0.locked.store(locked, Ordering::Relaxed);
    }

    /// This system cannot tell Deckly when it locks.
    pub fn set_undetected(&self) {
        self.0.undetected.store(true, Ordering::Relaxed);
    }

    pub fn undetected(&self) -> bool {
        self.0.undetected.load(Ordering::Relaxed)
    }

    /// Whether an action must not run now.
    pub fn blocks(&self, action: &str) -> bool {
        self.0.enabled.load(Ordering::Relaxed)
            && self.0.locked.load(Ordering::Relaxed)
            && !actions::runs_while_locked(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_computer_blocks_what_acts_on_the_machine() {
        let gate = Gate::new(true);
        gate.set_locked(true);
        for action in [actions::TEXT, actions::HOTKEY, "com.example.plugin"] {
            assert!(gate.blocks(action), "{action}");
        }
        assert!(!gate.blocks(actions::NEXT_PAGE));
    }

    #[test]
    fn nothing_is_blocked_while_unlocked() {
        let gate = Gate::new(true);
        assert!(!gate.blocks(actions::TEXT));
        assert!(!gate.blocks("com.example.plugin"));
    }

    #[test]
    fn keys_work_again_after_unlocking_without_a_restart() {
        let gate = Gate::new(true);
        for locked in [true, false, true, false] {
            gate.set_locked(locked);
            assert_eq!(gate.blocks(actions::HOTKEY), locked);
        }
    }

    #[test]
    fn the_setting_turns_the_gate_off_and_on_while_locked() {
        let gate = Gate::new(false);
        gate.set_locked(true);
        assert!(!gate.blocks(actions::TEXT));
        gate.set_enabled(true);
        assert!(gate.blocks(actions::TEXT));
        gate.set_enabled(false);
        assert!(!gate.blocks(actions::TEXT));
    }

    /// Fail-open on purpose: without a signal the keys would never work.
    #[test]
    fn an_undetectable_lock_screen_leaves_keys_working() {
        let gate = Gate::new(true);
        gate.set_undetected();
        assert!(gate.undetected());
        assert!(!gate.blocks(actions::TEXT));
    }

    #[test]
    fn a_lost_watcher_keeps_the_last_known_state() {
        let gate = Gate::new(true);
        gate.set_locked(true);
        gate.set_undetected();
        assert!(gate.blocks(actions::TEXT));
    }

    #[test]
    fn clones_share_their_state() {
        let gate = Gate::new(true);
        gate.clone().set_locked(true);
        assert!(gate.blocks(actions::TEXT));
    }
}

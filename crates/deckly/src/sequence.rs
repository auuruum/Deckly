//! Running what a Multi Action or Key Logic key holds
//! (docs/clean-room/2026-10-30-multi-actions.md).
//!
//! A sequence runs on a thread of its own so a long Delay holds up nothing
//! else; the runtime keeps a [`Sender`] per run, and sending on it or dropping
//! it cancels the run. [`Press`] tells apart the presses of a Key Logic key.

use crate::actions;
use crate::runtime::Msg;
use deckly_core::{ActionDelays, KeyConfig};
use deckly_device::Handle;
use std::hash::{BuildHasher, RandomState};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

/// A second press this soon after the first is a double press.
const DOUBLE_PRESS: Duration = Duration::from_millis(300);
/// Held this long, a press is a hold.
const HOLD: Duration = Duration::from_millis(500);
/// How long a single press holds its action's virtual key down.
pub const TAP: u32 = 60;

/// The slots of a Key Logic key, in `KeyConfig::groups` order.
pub const SINGLE: usize = 0;
pub const DOUBLE: usize = 1;
pub const HOLD_SLOT: usize = 2;

/// Tells apart the presses of a Key Logic key. Each change returns the slot
/// to trigger, if one should be.
#[derive(Debug, Default)]
pub struct Press {
    /// When an undecided press began.
    started: Option<Instant>,
    held: bool,
    /// A slot triggered while the key stays down; its action waits for the
    /// release.
    down: Option<usize>,
}

impl Press {
    pub fn down(&mut self, now: Instant) -> Option<usize> {
        self.held = true;
        match self.started {
            Some(first) if now - first < DOUBLE_PRESS => {
                self.started = None;
                self.down = Some(DOUBLE);
                Some(DOUBLE)
            }
            _ => {
                self.started = Some(now);
                None
            }
        }
    }

    /// The official app ignores a press released between the double-press
    /// window and the hold time; here it counts as a single press.
    pub fn up(&mut self, now: Instant) -> Option<usize> {
        self.held = false;
        let first = self.started?;
        if now - first < DOUBLE_PRESS {
            return None;
        }
        self.started = None;
        Some(SINGLE)
    }

    /// The slot whose action was held down until now.
    pub fn take_down(&mut self) -> Option<usize> {
        self.down.take()
    }

    /// When [`Press::tick`] should run next.
    pub fn deadline(&self) -> Option<Instant> {
        let wait = if self.held { HOLD } else { DOUBLE_PRESS };
        Some(self.started? + wait)
    }

    pub fn tick(&mut self, now: Instant) -> Option<usize> {
        if self.deadline().is_none_or(|at| now < at) {
            return None;
        }
        self.started = None;
        if self.held {
            self.down = Some(HOLD_SLOT);
            Some(HOLD_SLOT)
        } else {
            Some(SINGLE)
        }
    }

    pub fn idle(&self) -> bool {
        self.started.is_none() && self.down.is_none()
    }
}

/// An action of a sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub key: KeyConfig,
    /// What its plugin knows it by.
    pub context: String,
    pub plugin: bool,
}

/// The list a Multi Action key runs now: the side a switch is on, or one
/// action picked at random. Returns its index and the steps taken from it.
pub fn chosen(key: &KeyConfig) -> Option<(usize, Vec<usize>)> {
    let list = if key.action == actions::MULTI_ACTION_SWITCH {
        actions::switch_side(key)
    } else {
        0
    };
    let size = key.groups.get(list)?.len();
    if size == 0 {
        return None;
    }
    let members = if key.action == actions::RANDOM_ACTION {
        vec![pick(size)]
    } else {
        (0..size).collect()
    };
    Some((list, members))
}

fn pick(size: usize) -> usize {
    RandomState::new().hash_one(0u8) as usize % size.max(1)
}

/// How long to wait after the virtual key press and after its release.
pub fn timing(key: &KeyConfig) -> (Duration, Duration) {
    if key.action == actions::DELAY {
        let ms = actions::int_setting(&key.settings, "delay", 0).clamp(0, actions::MAX_DELAY_MS);
        return (Duration::ZERO, Duration::from_millis(ms as u64));
    }
    let d: ActionDelays = key.delays.unwrap_or_default();
    (
        Duration::from_millis(u64::from(d.press)),
        Duration::from_millis(u64::from(d.next.max(1))),
    )
}

/// Runs `steps` in order, then reports [`Msg::RunDone`] for `context`.
/// Returns what cancels the run; `None` if no thread could be started.
pub fn start(
    steps: Vec<Step>,
    handle: Handle,
    context: String,
    results: Sender<Msg>,
) -> Option<Sender<()>> {
    let (cancel, cancelled) = channel::<()>();
    let waited = move |wait: Duration| {
        matches!(cancelled.recv_timeout(wait), Err(RecvTimeoutError::Timeout))
    };
    let run = move || {
        let mut finished = true;
        for step in &steps {
            let (hold, next) = timing(&step.key);
            press(step, true, handle, &results);
            let stay = waited(hold);
            // The release goes out even when cancelled: a plugin must not
            // be left with its key down.
            press(step, false, handle, &results);
            if !stay || !waited(next) {
                finished = false;
                break;
            }
        }
        let _ = results.send(Msg::RunDone {
            context,
            cancelled: !finished,
        });
    };
    std::thread::Builder::new()
        .name("deckly-sequence".into())
        .spawn(run)
        .map_err(|e| eprintln!("deckly: cannot start a sequence: {e}"))
        .ok()?;
    Some(cancel)
}

fn press(step: &Step, pressed: bool, handle: Handle, results: &Sender<Msg>) {
    if step.key.action == actions::DELAY {
        return;
    }
    let msg = if step.plugin {
        Msg::PluginStep {
            handle,
            step: step.clone(),
            pressed,
        }
    } else if pressed {
        Msg::Outcome(handle, actions::run(&step.key))
    } else {
        return;
    };
    let _ = results.send(msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_tap_is_a_single_press_once_the_window_closes() {
        let t = Instant::now();
        let mut p = Press::default();
        assert_eq!(p.down(t), None);
        assert_eq!(p.up(t + ms(80)), None);
        assert_eq!(p.deadline(), Some(t + DOUBLE_PRESS));
        assert_eq!(p.tick(t + ms(299)), None);
        assert_eq!(p.tick(t + ms(300)), Some(SINGLE));
        assert!(p.idle());
    }

    #[test]
    fn a_second_press_inside_the_window_is_a_double_press() {
        let t = Instant::now();
        let mut p = Press::default();
        p.down(t);
        p.up(t + ms(50));
        assert_eq!(p.down(t + ms(200)), Some(DOUBLE));
        assert_eq!(p.tick(t + ms(900)), None);
        // Its action stays down until the key comes up.
        assert_eq!(p.up(t + ms(260)), None);
        assert_eq!(p.take_down(), Some(DOUBLE));
        assert!(p.idle());
    }

    #[test]
    fn a_press_held_past_the_hold_time_is_a_hold() {
        let t = Instant::now();
        let mut p = Press::default();
        p.down(t);
        assert_eq!(p.tick(t + ms(300)), None);
        assert_eq!(p.tick(t + ms(500)), Some(HOLD_SLOT));
        assert_eq!(p.up(t + ms(2000)), None);
        assert_eq!(p.take_down(), Some(HOLD_SLOT));
        assert!(p.idle());
    }

    #[test]
    fn a_press_released_between_the_two_times_is_a_single_press() {
        let t = Instant::now();
        let mut p = Press::default();
        p.down(t);
        assert_eq!(p.up(t + ms(400)), Some(SINGLE));
        assert!(p.idle());
    }

    #[test]
    fn delays_default_to_the_official_values() {
        let key = KeyConfig::new("com.example.a", json!({}));
        assert_eq!(timing(&key), (ms(100), ms(100)));
        let mut key = key;
        key.delays = Some(ActionDelays { press: 5, next: 7 });
        assert_eq!(timing(&key), (ms(5), ms(7)));
        let delay = KeyConfig::new(actions::DELAY, json!({"delay": 1500}));
        assert_eq!(timing(&delay), (ms(0), ms(1500)));
        let broken = KeyConfig::new(actions::DELAY, json!({"delay": -4}));
        assert_eq!(timing(&broken), (ms(0), ms(0)));
    }

    fn with_members(action: &str, sides: &[usize]) -> KeyConfig {
        let mut key = KeyConfig::new(action, json!({}));
        for (list, n) in sides.iter().enumerate() {
            key.groups[list] = vec![KeyConfig::new("com.example.a", json!({})); *n];
        }
        key
    }

    #[test]
    fn a_switch_runs_the_side_it_is_on() {
        let mut key = with_members(actions::MULTI_ACTION_SWITCH, &[2, 3]);
        assert_eq!(chosen(&key), Some((0, vec![0, 1])));
        actions::set_switch_side(&mut key, 1);
        assert_eq!(chosen(&key), Some((1, vec![0, 1, 2])));
    }

    #[test]
    fn empty_lists_run_nothing() {
        assert_eq!(chosen(&with_members(actions::MULTI_ACTION, &[0])), None);
        assert_eq!(
            chosen(&with_members(actions::MULTI_ACTION_SWITCH, &[1, 0])).map(|(list, _)| list),
            Some(0)
        );
        assert_eq!(chosen(&with_members(actions::RANDOM_ACTION, &[0])), None);
        assert_eq!(chosen(&KeyConfig::new("plain", json!({}))), None);
    }

    #[test]
    fn a_random_action_runs_one_action_and_reaches_them_all() {
        let key = with_members(actions::RANDOM_ACTION, &[4]);
        let mut seen = [false; 4];
        for _ in 0..500 {
            let (list, members) = chosen(&key).unwrap();
            assert_eq!((list, members.len()), (0, 1));
            seen[members[0]] = true;
        }
        assert_eq!(seen, [true; 4]);
    }

    fn step(action: &str, delays: Option<ActionDelays>, plugin: bool) -> Step {
        let mut key = KeyConfig::new(action, json!({"delay": 40}));
        key.delays = delays;
        Step {
            key,
            context: "page.0.0.0".into(),
            plugin,
        }
    }

    #[test]
    fn a_run_presses_and_releases_each_action_in_order() {
        let (tx, rx) = channel();
        let quick = Some(ActionDelays { press: 1, next: 1 });
        let steps = vec![
            step(actions::NEXT_PAGE, quick, false),
            step("com.example.plugin", quick, true),
            step(actions::DELAY, None, false),
            step(actions::PREVIOUS_PAGE, quick, false),
        ];
        let _cancel = start(steps, 7, "page.0".into(), tx).unwrap();
        let mut seen = Vec::new();
        for msg in rx.iter() {
            match msg {
                Msg::Outcome(7, Ok(o)) => seen.push(format!("{o:?}")),
                Msg::PluginStep { pressed, .. } => seen.push(format!("plugin {pressed}")),
                Msg::RunDone { context, cancelled } => {
                    assert_eq!(context, "page.0");
                    assert!(!cancelled);
                    break;
                }
                _ => panic!("unexpected message"),
            }
        }
        assert_eq!(
            seen,
            ["NextPage", "plugin true", "plugin false", "PreviousPage"]
        );
    }

    #[test]
    fn cancelling_stops_the_run_but_releases_its_key() {
        let (tx, rx) = channel();
        let held = Some(ActionDelays {
            press: 60_000,
            next: 1,
        });
        let steps = vec![
            step("com.example.plugin", held, true),
            step(actions::NEXT_PAGE, None, false),
        ];
        let cancel = start(steps, 1, "page.0".into(), tx).unwrap();
        assert!(matches!(
            rx.recv(),
            Ok(Msg::PluginStep { pressed: true, .. })
        ));
        let _ = cancel.send(());
        assert!(matches!(
            rx.recv(),
            Ok(Msg::PluginStep { pressed: false, .. })
        ));
        assert!(matches!(
            rx.recv(),
            Ok(Msg::RunDone {
                cancelled: true,
                ..
            })
        ));
    }
}

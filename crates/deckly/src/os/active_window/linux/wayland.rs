//! Wayland: the wlr foreign toplevel protocol, made for task bars and open to
//! ordinary clients. The compositor lists every window, XWayland ones too, and
//! flags the focused one. A desktop that does not offer it reports that it
//! cannot.
//!
//! The protocol names a window by its application id only. Hyprland also tells
//! the process through its own socket, which rules on a program need.

use super::{Report, program};
use crate::os::linux::hyprland;
use std::collections::HashMap;
use std::error::Error;
use std::hash::Hash;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self, ZwlrForeignToplevelManagerV1,
};

const UNSUPPORTED: &str = "This desktop does not tell applications which window is active. \
                           Use an X11 session to switch profiles by window.";

/// Where a compositor gives no process, Deckly knows its windows by these.
const OWN_IDS: [&str; 2] = ["deckly", "deckly-web"];

#[derive(Default)]
struct Window {
    app_id: String,
    activated: bool,
}

/// What the compositor said about its windows. A change of focus arrives as
/// two windows that each finish in their own time, in either order.
struct Windows<Id> {
    windows: HashMap<Id, Window>,
    active: Option<Id>,
}

impl<Id: Clone + Eq + Hash> Windows<Id> {
    fn new() -> Self {
        Self {
            windows: HashMap::new(),
            active: None,
        }
    }

    fn set_app_id(&mut self, id: Id, app_id: String) {
        self.windows.entry(id).or_default().app_id = app_id;
    }

    fn set_activated(&mut self, id: Id, activated: bool) {
        self.windows.entry(id).or_default().activated = activated;
    }

    /// A window's changes count once it says they are complete.
    fn done(&mut self, id: &Id) {
        let Some(window) = self.windows.get(id) else {
            return;
        };
        if window.activated {
            self.active = Some(id.clone());
        } else if self.active.as_ref() == Some(id) {
            self.active = None;
        }
    }

    fn closed(&mut self, id: &Id) {
        self.windows.remove(id);
        if self.active.as_ref() == Some(id) {
            self.active = None;
        }
    }

    /// `None` when no window has the focus.
    fn active_app(&self) -> Option<&str> {
        let window = self.windows.get(self.active.as_ref()?)?;
        Some(&window.app_id)
    }
}

/// The state event is an array of 32-bit values in the machine's byte order.
fn is_activated(state: &[u8]) -> bool {
    let activated = zwlr_foreign_toplevel_handle_v1::State::Activated as u32;
    state
        .as_chunks::<4>()
        .0
        .iter()
        .any(|bytes| u32::from_ne_bytes(*bytes) == activated)
}

struct Tracker {
    windows: Windows<ObjectId>,
    finished: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Tracker {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Tracker {
    fn event(
        tracker: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zwlr_foreign_toplevel_manager_v1::Event::Finished) {
            tracker.finished = true;
        }
    }

    event_created_child!(Tracker, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Tracker {
    fn event(
        tracker: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        let id = handle.id();
        match event {
            Event::AppId { app_id } => tracker.windows.set_app_id(id, app_id),
            Event::State { state } => tracker.windows.set_activated(id, is_activated(&state)),
            Event::Done => tracker.windows.done(&id),
            Event::Closed => {
                tracker.windows.closed(&id);
                handle.destroy();
            }
            _ => {}
        }
    }
}

/// Reports the active window until the connection to the compositor ends.
pub fn follow(report: &Report) -> Result<(), Box<dyn Error>> {
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Tracker>(&connection)?;
    let manager: Result<ZwlrForeignToplevelManagerV1, _> = globals.bind(&queue.handle(), 1..=3, ());
    let Ok(_manager) = manager else {
        report(Err(UNSUPPORTED.into()));
        return Ok(());
    };
    let mut tracker = Tracker {
        windows: Windows::new(),
        finished: false,
    };
    // Lists the windows that are open already.
    queue.roundtrip(&mut tracker)?;
    let mut last = None;
    while !tracker.finished {
        let active = tracker.windows.active_app().map(str::to_owned);
        if last.as_ref() != Some(&active) {
            let app_id = active.as_deref().unwrap_or_default();
            if !OWN_IDS.contains(&app_id) {
                report(Ok(application_names(active.as_deref())));
            }
            last = Some(active);
        }
        queue.blocking_dispatch(&mut tracker)?;
    }
    Err("the compositor stopped listing windows".into())
}

fn application_names(app_id: Option<&str>) -> Vec<String> {
    let Some(app_id) = app_id else {
        return Vec::new();
    };
    let mut names = Vec::new();
    if !app_id.is_empty() {
        names.push(app_id.to_owned());
    }
    names.extend(hyprland::active_pid(app_id).and_then(program));
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(windows: &mut Windows<u32>, id: u32, app_id: &str, activated: bool) {
        windows.set_app_id(id, app_id.to_owned());
        windows.set_activated(id, activated);
        windows.done(&id);
    }

    #[test]
    fn the_flagged_window_is_active() {
        let mut windows = Windows::new();
        assert_eq!(windows.active_app(), None);
        opened(&mut windows, 1, "first", false);
        assert_eq!(windows.active_app(), None);
        opened(&mut windows, 2, "second", true);
        assert_eq!(windows.active_app(), Some("second"));
    }

    #[test]
    fn a_change_counts_when_the_window_is_done() {
        let mut windows = Windows::new();
        windows.set_app_id(1, "first".to_owned());
        windows.set_activated(1, true);
        assert_eq!(windows.active_app(), None);
        windows.done(&1);
        assert_eq!(windows.active_app(), Some("first"));
    }

    #[test]
    fn focus_moves_in_either_order() {
        let mut windows = Windows::new();
        opened(&mut windows, 1, "first", true);
        opened(&mut windows, 2, "second", false);

        // The old window lets go first.
        windows.set_activated(1, false);
        windows.done(&1);
        assert_eq!(windows.active_app(), None);
        windows.set_activated(2, true);
        windows.done(&2);
        assert_eq!(windows.active_app(), Some("second"));

        // The new window takes it first.
        windows.set_activated(1, true);
        windows.done(&1);
        windows.set_activated(2, false);
        windows.done(&2);
        assert_eq!(windows.active_app(), Some("first"));
    }

    #[test]
    fn closing_the_active_window_leaves_none() {
        let mut windows = Windows::new();
        opened(&mut windows, 1, "first", true);
        opened(&mut windows, 2, "second", false);
        windows.closed(&2);
        assert_eq!(windows.active_app(), Some("first"));
        windows.closed(&1);
        assert_eq!(windows.active_app(), None);
        assert!(windows.windows.is_empty());
    }

    #[test]
    fn a_done_for_an_unknown_window_changes_nothing() {
        let mut windows = Windows::new();
        opened(&mut windows, 1, "first", true);
        windows.done(&2);
        windows.closed(&2);
        assert_eq!(windows.active_app(), Some("first"));
    }

    #[test]
    fn a_window_can_rename_itself() {
        let mut windows = Windows::new();
        opened(&mut windows, 1, "first", true);
        windows.set_app_id(1, "renamed".to_owned());
        assert_eq!(windows.active_app(), Some("renamed"));
    }

    #[test]
    fn the_state_array_is_read_whole() {
        let state =
            |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_ne_bytes()).collect() };
        assert!(is_activated(&state(&[2])));
        assert!(is_activated(&state(&[0, 1, 2])));
        assert!(!is_activated(&state(&[0, 1, 3])));
        assert!(!is_activated(&[]));
        // A value cut short is no value.
        assert!(!is_activated(&2u32.to_ne_bytes()[..3]));
    }

    #[test]
    fn no_window_has_no_names() {
        assert!(application_names(None).is_empty());
    }

    #[test]
    #[ignore = "needs a compositor with the protocol, and hears the real focus"]
    fn the_compositor_reports_the_active_window() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let report: Report = std::sync::Arc::new(move |names| {
            if let Ok(sender) = sender.lock() {
                let _ = sender.send(names);
            }
        });
        std::thread::spawn(move || follow(&report).map_err(|e| e.to_string()));
        let wait = std::time::Duration::from_secs(5);
        let first = receiver.recv_timeout(wait).unwrap().unwrap();
        println!("{first:?}");
        // Prints focus changes until five seconds pass without one.
        while let Ok(names) = receiver.recv_timeout(wait) {
            println!("{:?}", names.unwrap());
        }
    }
}

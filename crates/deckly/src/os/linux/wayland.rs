//! Keys through the Wayland virtual keyboard protocol, which wlroots
//! compositors and Hyprland offer to ordinary clients. It needs no
//! permission, and the keyboard carries its own layout, so any text types
//! correctly whatever layout is active.
//!
//! The compositor tells status bars of each new virtual keyboard and each
//! layout it loads, and those bars then announce a change of layout. So one
//! keyboard lives as long as Deckly, its layout is replaced only when it lacks
//! a key, and it is named after the layout in use (see `hyprland`).

use super::keys::{self, Key, Layout, NOTHING, Part, SHIFT, Stroke, modifier_bit};
use super::paste;
use super::{Failure, hyprland};
use rustix::fs::{MemfdFlags, memfd_create};
use std::io::Write;
use std::os::fd::AsFd;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

/// `wl_keyboard.keymap_format.xkb_v1`.
const XKB_V1: u32 = 1;
const PRESSED: u32 = 1;
const RELEASED: u32 = 0;
/// Between key events. Clients drop or reorder keys sent any faster.
const KEY_GAP: Duration = Duration::from_millis(2);
/// Lets clients load a new layout.
const LAYOUT_GAP: Duration = Duration::from_millis(25);

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
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

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);

fn unavailable(e: impl std::fmt::Display) -> Failure {
    Failure::Unavailable(e.to_string())
}

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure::Failed(e.to_string())
}

/// Splits strokes into runs that each fit one layout: all in one with the US
/// keys when it fits, as the next press can then often keep that layout.
fn runs(strokes: &[Stroke]) -> Vec<(Layout, &[Stroke])> {
    let mut whole = Layout::us();
    if strokes.iter().all(|stroke| whole.add(stroke)) {
        return vec![(whole, strokes)];
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut layout = Layout::new();
    for (at, stroke) in strokes.iter().enumerate() {
        if !layout.add(stroke) {
            out.push((
                std::mem::replace(&mut layout, Layout::new()),
                &strokes[start..at],
            ));
            start = at;
            layout.add(stroke);
        }
    }
    out.push((layout, &strokes[start..]));
    out
}

struct Sender {
    connection: Connection,
    keyboard: ZwpVirtualKeyboardV1,
    start: Instant,
}

impl Sender {
    fn flush(&self) -> Result<(), Failure> {
        self.connection.flush().map_err(failed)?;
        std::thread::sleep(KEY_GAP);
        Ok(())
    }

    fn key(&self, layout: &Layout, key: Key, state: u32) {
        // The protocol numbers keys from the evdev base, XKB from eight above.
        if let Some(keycode) = layout.keycode(key) {
            let time = self.start.elapsed().as_millis() as u32;
            self.keyboard.key(time, keycode - 8, state);
        }
    }

    /// The compositor swaps in this keyboard and sends its layout to the
    /// focused client on the first key, not on upload, and a client that is
    /// still loading it drops what follows. So the first key is an empty one.
    fn settle(&self, layout: &Layout) -> Result<(), Failure> {
        self.key(layout, NOTHING, PRESSED);
        self.key(layout, NOTHING, RELEASED);
        self.flush()?;
        std::thread::sleep(LAYOUT_GAP);
        Ok(())
    }

    fn stroke(&self, layout: &Layout, stroke: &Stroke) -> Result<(), Failure> {
        let mut mods = stroke.mods.clone();
        if layout.needs_shift(stroke.key) && !mods.contains(&SHIFT) {
            mods.push(SHIFT);
        }
        let mask = mods.iter().fold(0, |m, &key| m | modifier_bit(key));
        for &key in &mods {
            self.key(layout, key, PRESSED);
        }
        if mask != 0 {
            // The compositor does not work the state out from the keys.
            self.keyboard.modifiers(mask, 0, 0, 0);
            self.flush()?;
        }
        self.key(layout, stroke.key, PRESSED);
        self.flush()?;
        self.key(layout, stroke.key, RELEASED);
        self.flush()?;
        if mask != 0 {
            self.keyboard.modifiers(0, 0, 0, 0);
            for &key in mods.iter().rev() {
                self.key(layout, key, RELEASED);
            }
            self.flush()?;
        }
        Ok(())
    }
}

fn upload(keyboard: &ZwpVirtualKeyboardV1, layout: &Layout) -> Result<(), Failure> {
    let mut map = layout
        .keymap(hyprland::active_layout().as_deref())
        .into_bytes();
    map.push(0);
    let file = memfd_create(c"deckly-keymap", MemfdFlags::CLOEXEC).map_err(unavailable)?;
    let mut file = std::fs::File::from(file);
    file.write_all(&map).map_err(unavailable)?;
    keyboard.keymap(XKB_V1, file.as_fd(), map.len() as u32);
    Ok(())
}

struct Session {
    sender: Sender,
    queue: EventQueue<State>,
    /// The layout the compositor has.
    layout: Option<Layout>,
}

/// The keyboard lives as long as Deckly, as a new one would count as a change
/// of layout on every press.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner())
}

impl Session {
    fn open() -> Result<Self, Failure> {
        let connection = Connection::connect_to_env().map_err(unavailable)?;
        let (globals, queue) = registry_queue_init::<State>(&connection).map_err(unavailable)?;
        let handle = queue.handle();
        let seat: WlSeat = globals.bind(&handle, 1..=1, ()).map_err(unavailable)?;
        let manager: ZwpVirtualKeyboardManagerV1 =
            globals.bind(&handle, 1..=1, ()).map_err(unavailable)?;
        let keyboard = manager.create_virtual_keyboard(&seat, &handle, ());
        let sender = Sender {
            connection,
            keyboard,
            start: Instant::now(),
        };
        Ok(Self {
            sender,
            queue,
            layout: None,
        })
    }

    /// False once the compositor has restarted or dropped this connection.
    fn alive(&mut self) -> bool {
        self.queue.roundtrip(&mut State).is_ok()
    }

    fn press(&mut self, layout: &Layout, strokes: &[Stroke]) -> Result<(), Failure> {
        self.sender.settle(layout)?;
        for stroke in strokes {
            self.sender.stroke(layout, stroke)?;
        }
        self.queue.roundtrip(&mut State).map_err(failed)?;
        Ok(())
    }

    /// Sends strokes in order. `typed` tells whether any key went out, which
    /// decides what a failure means.
    fn type_keys(&mut self, strokes: &[Stroke], typed: &mut bool) -> Result<(), Failure> {
        if let Some(layout) = self.layout.take().filter(|l| l.holds(strokes)) {
            *typed = true;
            let pressed = self.press(&layout, strokes);
            self.layout = Some(layout);
            return pressed;
        }
        for (layout, run) in runs(strokes) {
            upload(&self.sender.keyboard, &layout)?;
            // A refusal shows up here. Before any key that means trying
            // another way; later ones would repeat what was typed.
            match self.queue.roundtrip(&mut State) {
                Err(e) if !*typed => return Err(unavailable(e)),
                Err(e) => return Err(failed(e)),
                Ok(_) => {}
            }
            *typed = true;
            self.press(&layout, run)?;
            self.layout = Some(layout);
        }
        Ok(())
    }

    fn send(&mut self, strokes: &[Stroke]) -> Result<(), Failure> {
        let mut typed = false;
        for part in keys::parts(strokes) {
            match part {
                Part::Keys(run) => self.type_keys(run, &mut typed)?,
                Part::Paste(run) => {
                    let press = [keys::paste_stroke()];
                    let text = keys::string(run);
                    let pasted = paste::paste(&text, || self.type_keys(&press, &mut typed));
                    match pasted {
                        // Still right everywhere but in Chromium.
                        Err(Failure::Unavailable(_)) => self.type_keys(run, &mut typed)?,
                        other => other?,
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn send(strokes: &[Stroke]) -> Result<(), Failure> {
    let mut slot = lock();
    if !slot.as_mut().is_some_and(Session::alive) {
        *slot = Some(Session::open()?);
    }
    let Some(session) = slot.as_mut() else {
        return Err(unavailable("no virtual keyboard"));
    };
    let result = session.send(strokes);
    if result.is_err() {
        // Next time starts with a fresh keyboard.
        *slot = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::linux::keys;

    #[test]
    fn long_text_in_many_scripts_is_split_into_layouts() {
        let text: String = (0..1000u32)
            .filter_map(|n| char::from_u32(0x4e00 + n))
            .collect();
        let strokes = keys::text(&text);
        let runs = runs(&strokes);
        assert!(runs.len() >= 4);
        assert_eq!(runs.iter().map(|r| r.1.len()).sum::<usize>(), strokes.len());
        for (layout, run) in &runs {
            assert!(run.iter().all(|s| layout.keycode(s.key).is_some()));
        }
    }

    #[test]
    fn short_text_needs_one_layout() {
        assert_eq!(runs(&keys::text("hello world")).len(), 1);
        assert_eq!(runs(&keys::text("Привет, мир!")).len(), 1);
    }

    #[test]
    fn a_layout_for_one_text_serves_the_next() {
        let first = keys::text("hello");
        let later = keys::text("Other text! 123\n");
        let runs = runs(&first);
        assert!(runs.iter().all(|(layout, _)| layout.holds(&later)));
    }
}

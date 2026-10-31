//! Keys through the Wayland virtual keyboard protocol, which wlroots
//! compositors and Hyprland offer to ordinary clients. It needs no
//! permission, and the keyboard carries its own layout, so any text types
//! correctly whatever layout is active.

use super::Failure;
use super::keys::{Key, Layout, NOTHING, Stroke, modifier_bit};
use rustix::fs::{MemfdFlags, memfd_create};
use std::io::Write;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
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

/// Splits strokes into runs that each fit one layout.
fn runs(strokes: &[Stroke]) -> Vec<(Layout, &[Stroke])> {
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

struct Sender<'a> {
    connection: &'a Connection,
    keyboard: &'a ZwpVirtualKeyboardV1,
    start: Instant,
}

impl Sender<'_> {
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
        let mask = stroke.mods.iter().fold(0, |m, &key| m | modifier_bit(key));
        for &key in &stroke.mods {
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
            for &key in stroke.mods.iter().rev() {
                self.key(layout, key, RELEASED);
            }
            self.flush()?;
        }
        Ok(())
    }
}

fn upload(keyboard: &ZwpVirtualKeyboardV1, layout: &Layout) -> Result<(), Failure> {
    let mut map = layout.keymap().into_bytes();
    map.push(0);
    let file = memfd_create(c"deckly-keymap", MemfdFlags::CLOEXEC).map_err(unavailable)?;
    let mut file = std::fs::File::from(file);
    file.write_all(&map).map_err(unavailable)?;
    keyboard.keymap(XKB_V1, file.as_fd(), map.len() as u32);
    Ok(())
}

pub fn send(strokes: &[Stroke]) -> Result<(), Failure> {
    let connection = Connection::connect_to_env().map_err(unavailable)?;
    let (globals, mut queue) = registry_queue_init::<State>(&connection).map_err(unavailable)?;
    let handle = queue.handle();
    let seat: WlSeat = globals.bind(&handle, 1..=1, ()).map_err(unavailable)?;
    let manager: ZwpVirtualKeyboardManagerV1 =
        globals.bind(&handle, 1..=1, ()).map_err(unavailable)?;
    let keyboard = manager.create_virtual_keyboard(&seat, &handle, ());
    let mut state = State;
    let sender = Sender {
        connection: &connection,
        keyboard: &keyboard,
        start: Instant::now(),
    };
    for (index, (layout, run)) in runs(strokes).iter().enumerate() {
        upload(&keyboard, layout)?;
        // A refusal shows up here. Before any key that means trying another
        // way; later ones would repeat what was typed.
        let sent = queue.roundtrip(&mut state);
        match sent {
            Err(e) if index == 0 => return Err(unavailable(e)),
            Err(e) => return Err(failed(e)),
            Ok(_) => {}
        }
        sender.settle(layout)?;
        for stroke in *run {
            sender.stroke(layout, stroke)?;
        }
        queue.roundtrip(&mut state).map_err(failed)?;
    }
    // Clients still handle the last keys with this layout.
    std::thread::sleep(LAYOUT_GAP);
    keyboard.destroy();
    connection.flush().map_err(failed)
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
    }
}

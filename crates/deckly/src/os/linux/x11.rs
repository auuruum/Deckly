//! Keys through the XTEST extension on X11 sessions. A symbol the active
//! layout lacks is put on a spare key for the time it is pressed.

use super::Failure;
use super::keys::{Key, SHIFT, Stroke};
use std::time::Duration;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Between key events.
const KEY_GAP: Duration = Duration::from_millis(2);
/// Lets clients take in a changed key.
const REMAP_GAP: Duration = Duration::from_millis(15);
const NO_SYMBOL: u32 = 0;

/// The active layout: symbols by key, `per_key` columns each.
struct Mapping {
    first: u8,
    per_key: usize,
    syms: Vec<u32>,
}

impl Mapping {
    /// A key with `sym` on its first level, or on its second with Shift.
    /// Later columns belong to other groups, which pressing a key can't reach.
    fn find(&self, sym: u32) -> Option<(u8, bool)> {
        self.keys().find_map(|(keycode, row)| {
            let level = row.iter().take(2).position(|&s| s == sym)?;
            Some((keycode, level == 1))
        })
    }

    /// A key with nothing on it.
    fn spare(&self) -> Option<u8> {
        self.keys()
            .filter(|(_, row)| row.iter().all(|&s| s == NO_SYMBOL))
            .map(|(keycode, _)| keycode)
            .last()
    }

    fn keys(&self) -> impl Iterator<Item = (u8, &[u32])> {
        (self.first..=u8::MAX).zip(self.syms.chunks_exact(self.per_key.max(1)))
    }
}

struct Session {
    connection: RustConnection,
    root: u32,
    mapping: Mapping,
}

impl Session {
    fn fake(&self, keycode: u8, press: bool) -> Result<(), Failure> {
        let kind = if press {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        };
        self.connection
            .xtest_fake_input(kind, keycode, 0, self.root, 0, 0, 0)
            .map_err(failed)?
            .check()
            .map_err(failed)?;
        self.connection.flush().map_err(failed)?;
        std::thread::sleep(KEY_GAP);
        Ok(())
    }

    /// Writes `sym` over a spare key, in every column.
    fn set_spare(&self, keycode: u8, sym: u32) -> Result<(), Failure> {
        let row = vec![sym; self.mapping.per_key];
        self.connection
            .change_keyboard_mapping(1, keycode, self.mapping.per_key as u8, &row)
            .map_err(failed)?
            .check()
            .map_err(failed)?;
        self.connection.flush().map_err(failed)?;
        std::thread::sleep(REMAP_GAP);
        Ok(())
    }

    /// Presses a key, with Shift when its symbol is on the second level.
    fn press(&self, keycode: u8, shift: Option<u8>, down: bool) -> Result<(), Failure> {
        match (shift, down) {
            (Some(shift), true) => {
                self.fake(shift, true)?;
                self.fake(keycode, true)
            }
            (Some(shift), false) => {
                self.fake(keycode, false)?;
                self.fake(shift, false)
            }
            (None, _) => self.fake(keycode, down),
        }
    }

    fn modifier(&self, key: Key) -> Result<u8, Failure> {
        match self.mapping.find(key.sym) {
            Some((keycode, _)) => Ok(keycode),
            None => Err(failed(format!("no key for modifier {:#x}", key.sym))),
        }
    }

    fn tap(&self, key: Key) -> Result<(), Failure> {
        if let Some((keycode, shifted)) = self.mapping.find(key.sym) {
            let shift = if shifted {
                Some(self.modifier(SHIFT)?)
            } else {
                None
            };
            self.press(keycode, shift, true)?;
            return self.press(keycode, shift, false);
        }
        let spare = self
            .mapping
            .spare()
            .ok_or_else(|| failed("the keyboard has no spare key"))?;
        self.set_spare(spare, key.sym)?;
        let result = self
            .press(spare, None, true)
            .and_then(|()| self.press(spare, None, false));
        std::thread::sleep(REMAP_GAP);
        let restored = self.set_spare(spare, NO_SYMBOL);
        result.and(restored)
    }

    /// The server treats the XTEST keyboard as a device of its own and swaps
    /// it in on its first event, while clients are told to reload the layout.
    /// A modifier sent that early is lost, turning a capital into a small
    /// letter, so an empty key goes first.
    fn settle(&self) -> Result<(), Failure> {
        if let Some(spare) = self.mapping.spare() {
            self.fake(spare, true)?;
            self.fake(spare, false)?;
            std::thread::sleep(REMAP_GAP);
        }
        Ok(())
    }

    fn stroke(&self, stroke: &Stroke) -> Result<(), Failure> {
        let mods: Vec<u8> = stroke
            .mods
            .iter()
            .map(|&m| self.modifier(m))
            .collect::<Result<_, _>>()?;
        for &keycode in &mods {
            self.fake(keycode, true)?;
        }
        let result = self.tap(stroke.key);
        for &keycode in mods.iter().rev() {
            self.fake(keycode, false)?;
        }
        result
    }
}

fn unavailable(e: impl std::fmt::Display) -> Failure {
    Failure::Unavailable(e.to_string())
}

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure::Failed(e.to_string())
}

fn connect() -> Result<Session, Failure> {
    let (connection, screen) = x11rb::connect(None).map_err(unavailable)?;
    let present = connection
        .extension_information(xtest::X11_EXTENSION_NAME)
        .map_err(unavailable)?;
    if present.is_none() {
        return Err(unavailable("the X server has no XTEST extension"));
    }
    let setup = connection.setup();
    let root = setup
        .roots
        .get(screen)
        .map(|s| s.root)
        .ok_or_else(|| unavailable("no X screen"))?;
    let (first, last) = (setup.min_keycode, setup.max_keycode);
    let reply = connection
        .get_keyboard_mapping(first, last - first + 1)
        .map_err(unavailable)?
        .reply()
        .map_err(unavailable)?;
    let mapping = Mapping {
        first,
        per_key: usize::from(reply.keysyms_per_keycode),
        syms: reply.keysyms,
    };
    Ok(Session {
        connection,
        root,
        mapping,
    })
}

pub fn send(strokes: &[Stroke]) -> Result<(), Failure> {
    let session = connect()?;
    session.settle()?;
    for stroke in strokes {
        session.stroke(stroke)?;
    }
    session.connection.sync().map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two groups per key: keys 8 ("a" "A" | "ф" "Ф"), 9 (empty), 10 ("b").
    fn mapping() -> Mapping {
        Mapping {
            first: 8,
            per_key: 4,
            syms: vec![
                0x61,
                0x41,
                0x0100_0444,
                0x0100_0424, // first key
                0,
                0,
                0,
                0, // spare
                0x62,
                0x42,
                0,
                0,
            ],
        }
    }

    #[test]
    fn finds_symbols_and_their_level() {
        let m = mapping();
        assert_eq!(m.find(0x61), Some((8, false)));
        assert_eq!(m.find(0x41), Some((8, true)));
        assert_eq!(m.find(0x42), Some((10, true)));
    }

    #[test]
    fn symbols_of_another_group_count_as_missing() {
        assert_eq!(mapping().find(0x0100_0444), None);
    }

    #[test]
    fn finds_a_spare_key() {
        assert_eq!(mapping().spare(), Some(9));
        let full = Mapping {
            first: 8,
            per_key: 1,
            syms: vec![0x61],
        };
        assert_eq!(full.spare(), None);
    }

    #[test]
    fn an_empty_map_is_harmless() {
        let empty = Mapping {
            first: 8,
            per_key: 0,
            syms: Vec::new(),
        };
        assert_eq!(empty.find(0x61), None);
        assert_eq!(empty.spare(), None);
    }
}

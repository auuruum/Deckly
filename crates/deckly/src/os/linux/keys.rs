//! What a simulated key is: its meaning (an X keysym, which the Wayland, X11
//! and portal paths send) and its place on a US keyboard (a Linux input code,
//! which uinput sends and which keeps `code:`-style bindings working).
//! Pure tables and text, so all of it is tested without a session.

use crate::os::{Hotkey, MediaKey};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub sym: u32,
    /// `None` for what a US keyboard cannot type.
    pub code: Option<u16>,
    /// A US keyboard needs Shift for this symbol at `code`.
    pub shifted: bool,
}

impl Key {
    const fn at(sym: u32, code: u16) -> Self {
        Self {
            sym,
            code: Some(code),
            shifted: false,
        }
    }

    /// The key for a character of text, `None` for control characters.
    pub fn for_char(c: char) -> Option<Self> {
        match c {
            '\n' => return Some(ENTER),
            '\t' => return Some(TAB),
            c if c.is_control() => return None,
            _ => {}
        }
        let unicode = u32::from(c);
        // Latin-1 keysyms equal their code point; the rest have a Unicode range.
        let sym = if unicode < 0x100 {
            unicode
        } else {
            0x0100_0000 | unicode
        };
        let (code, shifted) = match us_place(c) {
            Some((code, shifted)) => (Some(code), shifted),
            None => (None, false),
        };
        Some(Self { sym, code, shifted })
    }

    pub fn media(key: MediaKey) -> Self {
        match key {
            MediaKey::PlayPause => Self::at(0x1008_ff14, 164),
            MediaKey::Stop => Self::at(0x1008_ff15, 166),
            MediaKey::Previous => Self::at(0x1008_ff16, 165),
            MediaKey::Next => Self::at(0x1008_ff17, 163),
            MediaKey::Mute => Self::at(0x1008_ff12, 113),
            MediaKey::VolumeDown => Self::at(0x1008_ff11, 114),
            MediaKey::VolumeUp => Self::at(0x1008_ff13, 115),
        }
    }
}

pub const SHIFT: Key = Key::at(0xffe1, 42);
pub const CTRL: Key = Key::at(0xffe3, 29);
pub const ALT: Key = Key::at(0xffe9, 56);
pub const SUPER: Key = Key::at(0xffeb, 125);
/// A key that means nothing to anyone.
pub const NOTHING: Key = Key {
    sym: 0x00ff_ffff,
    code: None,
    shifted: false,
};
const ENTER: Key = Key::at(0xff0d, 28);
const TAB: Key = Key::at(0xff09, 15);

/// Modifier state bits of the core X and Wayland protocols, by modifier key.
pub const fn modifier_bit(key: Key) -> u32 {
    match key.sym {
        0xffe1 => 1,
        0xffe3 => 1 << 2,
        0xffe9 => 1 << 3,
        0xffeb => 1 << 6,
        _ => 0,
    }
}

/// One key press, with the modifiers held around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub mods: Vec<Key>,
    pub key: Key,
}

impl Stroke {
    pub fn single(key: Key) -> Self {
        Self {
            mods: Vec::new(),
            key,
        }
    }
}

/// Letters from A to Z, by input code.
const LETTERS: [u16; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45,
    21, 44,
];

/// Where a character sits on a US keyboard, and whether it needs Shift.
fn us_place(c: char) -> Option<(u16, bool)> {
    let plain = |code| Some((code, false));
    let shifted = |code| Some((code, true));
    match c {
        'a'..='z' => plain(LETTERS[c as usize - 'a' as usize]),
        'A'..='Z' => shifted(LETTERS[c as usize - 'A' as usize]),
        '1'..='9' => plain(c as u16 - '1' as u16 + 2),
        '0' => plain(11),
        ' ' => plain(57),
        '-' => plain(12),
        '=' => plain(13),
        '[' => plain(26),
        ']' => plain(27),
        '\\' => plain(43),
        ';' => plain(39),
        '\'' => plain(40),
        '`' => plain(41),
        ',' => plain(51),
        '.' => plain(52),
        '/' => plain(53),
        '!' => shifted(2),
        '@' => shifted(3),
        '#' => shifted(4),
        '$' => shifted(5),
        '%' => shifted(6),
        '^' => shifted(7),
        '&' => shifted(8),
        '*' => shifted(9),
        '(' => shifted(10),
        ')' => shifted(11),
        '_' => shifted(12),
        '+' => shifted(13),
        '{' => shifted(26),
        '}' => shifted(27),
        '|' => shifted(43),
        ':' => shifted(39),
        '"' => shifted(40),
        '~' => shifted(41),
        '<' => shifted(51),
        '>' => shifted(52),
        '?' => shifted(53),
        _ => None,
    }
}

fn function_key(n: u32) -> Option<Key> {
    let code = match n {
        1..=10 => Some(58 + n as u16),
        11 => Some(87),
        12 => Some(88),
        13..=24 => Some(170 + n as u16),
        25..=35 => None,
        _ => return None,
    };
    Some(Key {
        sym: 0xffbe + n - 1,
        code,
        shifted: false,
    })
}

/// A key by its egui name, the notation hotkeys are stored in.
fn named(name: &str) -> Option<Key> {
    let key = match name {
        "Backspace" => Key::at(0xff08, 14),
        "Tab" => TAB,
        "Enter" => ENTER,
        "Escape" => Key::at(0xff1b, 1),
        "Space" => Key::at(0x20, 57),
        "Home" => Key::at(0xff50, 102),
        "Left" => Key::at(0xff51, 105),
        "Up" => Key::at(0xff52, 103),
        "Right" => Key::at(0xff53, 106),
        "Down" => Key::at(0xff54, 108),
        "PageUp" => Key::at(0xff55, 104),
        "PageDown" => Key::at(0xff56, 109),
        "End" => Key::at(0xff57, 107),
        "Insert" => Key::at(0xff63, 110),
        "Delete" => Key::at(0xffff, 111),
        "Copy" => Key::at(0x1008_ff57, 133),
        "Cut" => Key::at(0x1008_ff58, 137),
        "Paste" => Key::at(0x1008_ff6d, 135),
        "BrowserBack" => Key::at(0x1008_ff26, 158),
        "Semicolon" => return Key::for_char(';'),
        "Colon" => return Key::for_char(':'),
        "Equals" => return Key::for_char('='),
        "Plus" => return Key::for_char('+'),
        "Comma" => return Key::for_char(','),
        "Minus" => return Key::for_char('-'),
        "Period" => return Key::for_char('.'),
        "Slash" => return Key::for_char('/'),
        "Questionmark" => return Key::for_char('?'),
        "Exclamationmark" => return Key::for_char('!'),
        "Backtick" => return Key::for_char('`'),
        "OpenBracket" => return Key::for_char('['),
        "CloseBracket" => return Key::for_char(']'),
        "OpenCurlyBracket" => return Key::for_char('{'),
        "CloseCurlyBracket" => return Key::for_char('}'),
        "Backslash" => return Key::for_char('\\'),
        "Pipe" => return Key::for_char('|'),
        "Quote" => return Key::for_char('\''),
        _ => {
            let mut chars = name.chars();
            return match (chars.next(), chars.next()) {
                // Hotkeys name letters in capitals but mean the key itself.
                (Some(c), None) if c.is_ascii_uppercase() => Key::for_char(c.to_ascii_lowercase()),
                (Some(c), None) if c.is_ascii_digit() => Key::for_char(c),
                (Some('F'), Some(_)) => function_key(name.get(1..)?.parse().ok()?),
                _ => None,
            };
        }
    };
    Some(key)
}

pub fn hotkey(hotkey: &Hotkey) -> Option<Stroke> {
    let key = named(&hotkey.key)?;
    let mods = [
        (hotkey.ctrl, CTRL),
        (hotkey.shift, SHIFT),
        (hotkey.alt, ALT),
        (hotkey.meta, SUPER),
    ]
    .into_iter()
    .filter_map(|(on, key)| on.then_some(key))
    .collect();
    Some(Stroke { mods, key })
}

pub fn text(text: &str) -> Vec<Stroke> {
    text.chars()
        .filter_map(Key::for_char)
        .map(Stroke::single)
        .collect()
}

/// The lowest and highest key numbers an XKB keymap can use.
const MIN_KEYCODE: u32 = 9;
const MAX_KEYCODE: u32 = 255;

/// A private keyboard layout: every key a run of strokes needs, on its own
/// key number. Text in any language works this way, whatever layout the
/// user has active.
#[derive(Debug)]
pub struct Layout {
    syms: BTreeMap<u32, u32>,
    keycodes: HashMap<u32, u32>,
}

impl Layout {
    pub fn new() -> Self {
        let mut layout = Self {
            syms: BTreeMap::new(),
            keycodes: HashMap::new(),
        };
        for key in [SHIFT, CTRL, ALT, SUPER, NOTHING] {
            layout.place(key);
        }
        layout
    }

    /// Finds a key number for a key: its real one when free, so a hotkey
    /// looks like the physical key. `false` when the layout is full.
    fn place(&mut self, key: Key) -> bool {
        if self.keycodes.contains_key(&key.sym) {
            return true;
        }
        let real = key
            .code
            .filter(|_| !key.shifted)
            .map(|code| u32::from(code) + 8)
            .filter(|keycode| !self.syms.contains_key(keycode));
        let free = || {
            (MIN_KEYCODE..=MAX_KEYCODE)
                .rev()
                .find(|k| !self.syms.contains_key(k))
        };
        let Some(keycode) = real.or_else(free) else {
            return false;
        };
        self.syms.insert(keycode, key.sym);
        self.keycodes.insert(key.sym, keycode);
        true
    }

    /// Adds everything a stroke needs; `false` when it did not all fit.
    pub fn add(&mut self, stroke: &Stroke) -> bool {
        stroke.mods.iter().all(|&m| self.place(m)) && self.place(stroke.key)
    }

    /// The XKB key number of a key that was added.
    pub fn keycode(&self, key: Key) -> Option<u32> {
        self.keycodes.get(&key.sym).copied()
    }

    /// The layout as an XKB keymap.
    pub fn keymap(&self) -> String {
        let mut out =
            String::from("xkb_keymap {\nxkb_keycodes \"deckly\" {\nminimum = 8;\nmaximum = 255;\n");
        for keycode in self.syms.keys() {
            let _ = writeln!(out, "<K{keycode}> = {keycode};");
        }
        out.push_str(
            "};\nxkb_types \"deckly\" { include \"complete\" };\n\
             xkb_compatibility \"deckly\" { include \"complete\" };\n\
             xkb_symbols \"deckly\" {\n",
        );
        for (keycode, sym) in &self.syms {
            // Letters get both cases, so Shift in a hotkey acts as it should.
            match char::from_u32(*sym).filter(char::is_ascii_lowercase) {
                Some(c) => {
                    let upper = u32::from(c.to_ascii_uppercase());
                    let _ = writeln!(out, "key <K{keycode}> {{ [ {sym:#x}, {upper:#x} ] }};");
                }
                None => {
                    let _ = writeln!(out, "key <K{keycode}> {{ [ {sym:#x} ] }};");
                }
            }
        }
        for (key, name) in [
            (SHIFT, "Shift"),
            (CTRL, "Control"),
            (ALT, "Mod1"),
            (SUPER, "Mod4"),
        ] {
            if let Some(keycode) = self.keycode(key) {
                let _ = writeln!(out, "modifier_map {name} {{ <K{keycode}> }};");
            }
        }
        out.push_str("};\n};\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(key: &str) -> Hotkey {
        Hotkey {
            key: key.into(),
            ..Hotkey::default()
        }
    }

    #[test]
    fn every_key_the_editor_can_record_has_a_key() {
        let names = [
            "Down",
            "Left",
            "Right",
            "Up",
            "Escape",
            "Tab",
            "Backspace",
            "Enter",
            "Insert",
            "Delete",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Copy",
            "Cut",
            "Paste",
            "Space",
            "Colon",
            "Comma",
            "Backslash",
            "Slash",
            "Pipe",
            "Questionmark",
            "Exclamationmark",
            "OpenBracket",
            "CloseBracket",
            "OpenCurlyBracket",
            "CloseCurlyBracket",
            "Backtick",
            "Minus",
            "Period",
            "Plus",
            "Equals",
            "Semicolon",
            "Quote",
            "BrowserBack",
            "0",
            "9",
            "A",
            "Z",
            "F1",
            "F24",
            "F35",
        ];
        for name in names {
            assert!(named(name).is_some(), "{name}");
        }
        assert_eq!(named("F36"), None);
        assert_eq!(named("F0"), None);
        assert_eq!(named("Fx"), None);
        assert_eq!(named("a"), None);
        assert_eq!(named(""), None);
    }

    #[test]
    fn keys_land_on_the_us_keyboard() {
        assert_eq!(named("A").map(|k| (k.sym, k.code)), Some((0x61, Some(30))));
        assert_eq!(named("Z").map(|k| (k.sym, k.code)), Some((0x7a, Some(44))));
        assert_eq!(named("1").and_then(|k| k.code), Some(2));
        assert_eq!(named("9").and_then(|k| k.code), Some(10));
        assert_eq!(named("0").and_then(|k| k.code), Some(11));
        assert_eq!(
            named("F1").map(|k| (k.sym, k.code)),
            Some((0xffbe, Some(59)))
        );
        assert_eq!(named("F10").and_then(|k| k.code), Some(68));
        assert_eq!(
            named("F12").map(|k| (k.sym, k.code)),
            Some((0xffc9, Some(88)))
        );
        assert_eq!(named("F13").and_then(|k| k.code), Some(183));
        assert_eq!(named("F24").and_then(|k| k.code), Some(194));
        assert_eq!(named("F35").map(|k| (k.sym, k.code)), Some((0xffe0, None)));
    }

    #[test]
    fn hotkeys_keep_modifier_order() {
        let stroke = hotkey(&Hotkey {
            ctrl: true,
            shift: true,
            alt: true,
            meta: true,
            key: "S".into(),
        })
        .unwrap();
        assert_eq!(stroke.mods, [CTRL, SHIFT, ALT, SUPER]);
        assert_eq!(hotkey(&hk("nonsense")), None);
    }

    #[test]
    fn text_keeps_every_language_and_drops_control_characters() {
        let keys: Vec<Key> = text("aB€я\n\r\t").into_iter().map(|s| s.key).collect();
        assert_eq!(keys.len(), 6);
        assert_eq!(keys[0], Key::at(0x61, 30));
        assert_eq!(
            (keys[1].sym, keys[1].code, keys[1].shifted),
            (0x42, Some(48), true)
        );
        assert_eq!((keys[2].sym, keys[2].code), (0x0100_20ac, None));
        assert_eq!((keys[3].sym, keys[3].code), (0x0100_044f, None));
        assert_eq!((keys[4], keys[5]), (ENTER, TAB));
    }

    #[test]
    fn layouts_use_real_key_numbers_and_tell_cases_apart() {
        let mut layout = Layout::new();
        let a = Key::for_char('a').unwrap();
        let upper = Key::for_char('A').unwrap();
        let euro = Key::for_char('€').unwrap();
        assert!(layout.add(&Stroke::single(a)));
        assert!(layout.add(&Stroke::single(upper)));
        assert!(layout.add(&Stroke::single(euro)));
        assert_eq!(layout.keycode(a), Some(38));
        assert_eq!(layout.keycode(SHIFT), Some(50));
        assert!(layout.keycode(upper).is_some_and(|k| k != 38));
        assert!(layout.keycode(euro).is_some());
        assert_eq!(layout.keycode(Key::for_char('z').unwrap()), None);
        let map = layout.keymap();
        assert!(map.contains("key <K38> { [ 0x61, 0x41 ] };"));
        assert!(map.contains("[ 0x10020ac ]"));
        assert!(map.contains("modifier_map Mod4 { <K133> };"));
    }

    #[test]
    fn a_full_layout_says_so() {
        let mut layout = Layout::new();
        let mut placed = 0;
        for n in 0..1000 {
            let Some(c) = char::from_u32(0x4e00 + n) else {
                continue;
            };
            let Some(key) = Key::for_char(c) else {
                continue;
            };
            if !layout.add(&Stroke::single(key)) {
                break;
            }
            placed += 1;
        }
        assert_eq!(placed as u32, MAX_KEYCODE - MIN_KEYCODE + 1 - 5);
    }
}

//! What a simulated key is: its meaning (an X keysym, which the Wayland, X11
//! and portal paths send) and its place on a US keyboard (a Linux input code,
//! which uinput sends and which keeps `code:`-style bindings working).
//! Pure tables and text, so all of it is tested without a session.

use crate::os::{Hotkey, MediaKey};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::ops::RangeInclusive;

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

    /// The character this key types, `None` for keys that do something else.
    fn character(self) -> Option<char> {
        let unicode = match self.sym {
            sym @ 0..0x100 => sym,
            sym => sym.checked_sub(0x0100_0000)?,
        };
        char::from_u32(unicode)
    }

    /// A character that does not fit 16 bits, which is all Chromium keeps of
    /// a key on Wayland.
    fn beyond_16_bits(self) -> bool {
        (0x0101_0000..=0x0110_ffff).contains(&self.sym)
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
/// A key that means nothing to anyone, on a number clients have no name for.
pub const NOTHING: Key = Key::at(0x00ff_ffff, 247);
const ENTER: Key = Key::at(0xff0d, 28);
const TAB: Key = Key::at(0xff09, 15);
const INSERT: Key = Key::at(0xff63, 110);

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

/// The symbols a US keyboard has at `code`: plain, then with Shift. Zero for
/// none.
fn us_levels(code: u16) -> [u32; 2] {
    let mut levels = [0; 2];
    for c in ' '..='~' {
        if let Some((_, shifted)) = us_place(c).filter(|&(at, _)| at == code) {
            levels[usize::from(shifted)] = u32::from(c);
        }
    }
    levels
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
        "Insert" => INSERT,
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

/// A stretch of strokes and how it reaches the client.
#[derive(Debug, PartialEq, Eq)]
pub enum Part<'a> {
    Keys(&'a [Stroke]),
    Paste(&'a [Stroke]),
}

/// Splits text for clients that cannot take every key. A line or cell with a
/// character beyond 16 bits is pasted whole, in one go, so an emoji and the
/// joiners between its pieces stay together; Enter and Tab stay real keys, as
/// a chat box sends on one and a form moves on with the other.
pub fn parts(strokes: &[Stroke]) -> Vec<Part<'_>> {
    let breaks = |stroke: &Stroke| stroke.key == ENTER || stroke.key == TAB;
    let mut out = Vec::new();
    let mut typed_to = 0;
    let mut at = 0;
    for line in strokes.split_inclusive(breaks) {
        let length = line.len() - usize::from(line.last().is_some_and(breaks));
        let text = line.get(..length).unwrap_or_default();
        if text.iter().any(|stroke| stroke.key.beyond_16_bits()) {
            if typed_to < at {
                out.push(Part::Keys(&strokes[typed_to..at]));
            }
            out.push(Part::Paste(text));
            typed_to = at + length;
        }
        at += line.len();
    }
    if typed_to < strokes.len() {
        out.push(Part::Keys(&strokes[typed_to..]));
    }
    out
}

/// The text a run of strokes types.
pub fn string(strokes: &[Stroke]) -> String {
    strokes.iter().filter_map(|s| s.key.character()).collect()
}

/// Shift+Insert, which pastes nearly everywhere, terminals included.
pub fn paste_stroke() -> Stroke {
    Stroke {
        mods: vec![SHIFT],
        key: INSERT,
    }
}

/// XKB key numbers for symbols without a place of their own: the number pad
/// and the extra keys of other keyboards, then the main block, which text
/// wants for itself. Chromium and Electron apps silently drop a key they have
/// no name for, and most numbers above the main keys are such keys, so only
/// numbers seen to arrive there are listed.
const SPARE_KEYS: [RangeInclusive<u32>; 12] = [
    79..=91,
    63..=63,
    106..=106,
    125..=125,
    129..=129,
    94..=94,
    97..=97,
    132..=132,
    10..=21,
    24..=35,
    38..=49,
    51..=61,
];

/// A private keyboard layout: every key a run of strokes needs. What a US
/// keyboard has sits on its real key and level, so clients see the same key
/// and Shift as from a real keyboard; the rest takes a spare key. Text in any
/// language works this way, whatever layout the user has active.
#[derive(Debug)]
pub struct Layout {
    /// Symbols by key number: plain, then with Shift. Zero for none.
    syms: BTreeMap<u32, [u32; 2]>,
    /// By symbol: its key number, and whether it needs Shift.
    keycodes: HashMap<u32, (u32, bool)>,
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

    /// Finds a place for a key: its real one when free, with the US symbols
    /// that belong there, else a spare key. `false` when the layout is full.
    fn place(&mut self, key: Key) -> bool {
        if self.keycodes.contains_key(&key.sym) {
            return true;
        }
        let real = key
            .code
            .map(|code| (code, u32::from(code) + 8))
            .filter(|(_, keycode)| !self.syms.contains_key(keycode));
        let (keycode, levels) = if let Some((code, keycode)) = real {
            let mut levels = us_levels(code);
            levels[usize::from(key.shifted)] = key.sym;
            (keycode, levels)
        } else {
            let spare = SPARE_KEYS
                .iter()
                .cloned()
                .flatten()
                .find(|k| !self.syms.contains_key(k));
            let Some(keycode) = spare else {
                return false;
            };
            (keycode, [key.sym, 0])
        };
        self.syms.insert(keycode, levels);
        for (level, sym) in levels.into_iter().enumerate() {
            if sym != 0 {
                self.keycodes.insert(sym, (keycode, level == 1));
            }
        }
        true
    }

    /// Adds everything a stroke needs; `false` when it did not all fit.
    pub fn add(&mut self, stroke: &Stroke) -> bool {
        stroke.mods.iter().all(|&m| self.place(m)) && self.place(stroke.key)
    }

    /// The XKB key number of a key that was added.
    pub fn keycode(&self, key: Key) -> Option<u32> {
        self.keycodes.get(&key.sym).map(|&(keycode, _)| keycode)
    }

    /// Whether a key that was added needs Shift held.
    pub fn needs_shift(&self, key: Key) -> bool {
        self.keycodes.get(&key.sym).is_some_and(|&(_, shift)| shift)
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
        for (keycode, levels) in &self.syms {
            let _ = match levels {
                [plain, 0] => writeln!(out, "key <K{keycode}> {{ [ {plain:#x} ] }};"),
                [plain, shifted] => {
                    writeln!(out, "key <K{keycode}> {{ [ {plain:#x}, {shifted:#x} ] }};")
                }
            };
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

    fn pasted(strokes: &[Stroke]) -> Vec<String> {
        parts(strokes)
            .into_iter()
            .filter_map(|part| match part {
                Part::Paste(run) => Some(string(run)),
                Part::Keys(_) => None,
            })
            .collect()
    }

    #[test]
    fn text_within_16_bits_stays_on_keys() {
        let strokes = text("tests!!! € я 日本語\nnext");
        assert_eq!(parts(&strokes), [Part::Keys(&strokes)]);
        assert_eq!(parts(&[]), []);
    }

    #[test]
    fn a_line_with_an_emoji_is_pasted_whole() {
        let strokes = text("hello \u{1f600}!");
        assert_eq!(parts(&strokes), [Part::Paste(&strokes)]);
        assert_eq!(string(&strokes), "hello \u{1f600}!");
    }

    #[test]
    fn enter_and_tab_around_an_emoji_stay_keys() {
        let strokes = text("one\n\u{1f600}\ttwo\n");
        let parts = parts(&strokes);
        assert_eq!(pasted(&strokes), ["\u{1f600}"]);
        assert_eq!(
            parts,
            [
                Part::Keys(&strokes[..4]),
                Part::Paste(&strokes[4..5]),
                Part::Keys(&strokes[5..]),
            ]
        );
    }

    #[test]
    fn joined_emoji_stay_in_one_paste() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(pasted(&text(family)), [family]);
    }

    #[test]
    fn hotkeys_and_media_keys_are_never_pasted() {
        let strokes = [paste_stroke(), Stroke::single(Key::media(MediaKey::Mute))];
        assert_eq!(parts(&strokes), [Part::Keys(&strokes)]);
        assert_eq!(string(&strokes), "");
    }

    #[test]
    fn layouts_use_real_keys_and_levels() {
        let mut layout = Layout::new();
        let a = Key::for_char('a').unwrap();
        let upper = Key::for_char('A').unwrap();
        let euro = Key::for_char('€').unwrap();
        assert!(layout.add(&Stroke::single(a)));
        assert!(layout.add(&Stroke::single(upper)));
        assert!(layout.add(&Stroke::single(euro)));
        assert_eq!(layout.keycode(a), Some(38));
        assert_eq!(layout.keycode(upper), Some(38));
        assert!(!layout.needs_shift(a));
        assert!(layout.needs_shift(upper));
        assert_eq!(layout.keycode(SHIFT), Some(50));
        assert!(layout.keycode(euro).is_some());
        assert!(!layout.needs_shift(euro));
        assert_eq!(layout.keycode(Key::for_char('z').unwrap()), None);
        let map = layout.keymap();
        assert!(map.contains("key <K38> { [ 0x61, 0x41 ] };"));
        assert!(map.contains("[ 0x10020ac ]"));
        assert!(map.contains("modifier_map Mod4 { <K133> };"));
    }

    /// Chromium drops keys it has no name for: "tests!!!hello" came out as
    /// "testshello" when `!` sat on a free key number near the top.
    #[test]
    fn symbols_on_a_us_keyboard_use_its_keys_and_shift() {
        let mut layout = Layout::new();
        for stroke in text("tests!!!dadadad@@@hello") {
            assert!(layout.add(&stroke));
        }
        let bang = Key::for_char('!').unwrap();
        let at = Key::for_char('@').unwrap();
        assert_eq!(layout.keycode(bang), Some(10));
        assert_eq!(layout.keycode(at), Some(11));
        assert!(layout.needs_shift(bang) && layout.needs_shift(at));
        let map = layout.keymap();
        assert!(map.contains("key <K10> { [ 0x31, 0x21 ] };"));
        assert!(map.contains("key <K11> { [ 0x32, 0x40 ] };"));
    }

    #[test]
    fn every_symbol_of_a_us_keyboard_has_its_real_key() {
        for c in ' '..='~' {
            let key = Key::for_char(c).unwrap();
            let mut layout = Layout::new();
            assert!(layout.add(&Stroke::single(key)));
            let real = key.code.map(|code| u32::from(code) + 8);
            assert_eq!(layout.keycode(key), real, "{c}");
            assert_eq!(layout.needs_shift(key), key.shifted, "{c}");
        }
    }

    #[test]
    fn spare_keys_are_ones_chromium_delivers() {
        // Numbers seen to give no key event in Electron 43 on Wayland.
        let dropped = [
            92, 103, 109, 120, 138, 147, 149, 153, 154, 155, 156, 157, 159, 161, 162, 165, 168,
            170, 178, 183, 184, 185, 186, 203, 204, 205, 206, 207, 208, 210, 211, 213, 219, 220,
            221, 222, 223, 224, 226, 227, 228, 229, 230, 231, 234, 244, 245, 246, 247, 248, 249,
            250, 251, 253, 254, 255,
        ];
        let spare: Vec<u32> = SPARE_KEYS.iter().cloned().flatten().collect();
        assert!(spare.iter().all(|k| !dropped.contains(k)));
        let mut unique = spare.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), spare.len());
    }

    #[test]
    fn a_spare_key_does_not_cost_a_letter_its_place() {
        let mut layout = Layout::new();
        let symbols = (0..SPARE_KEYS.iter().cloned().flatten().count() as u32 - 1)
            .filter_map(|n| Key::for_char(char::from_u32(0x4e00 + n)?));
        for key in symbols {
            assert!(layout.add(&Stroke::single(key)));
        }
        // The last spare is taken by "q", whose own key went to a symbol.
        let q = Key::for_char('q').unwrap();
        assert!(layout.add(&Stroke::single(q)));
        assert!(layout.keycode(q).is_some());
        assert!(!layout.needs_shift(q));
        assert!(!layout.add(&Stroke::single(Key::for_char('Q').unwrap())));
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
        assert_eq!(placed, SPARE_KEYS.iter().cloned().flatten().count());
    }
}

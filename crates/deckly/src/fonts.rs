//! Title fonts: Deckly's default font and the families installed on the
//! system, in the weight and slant a title asks for.
//!
//! Nothing here fails. A family that is not installed, a font file that does
//! not parse or one too large to load falls back to the default font, and a
//! face without its bold or italic is drawn bolder or slanted instead
//! (docs/clean-room/2026-11-04-title-fonts.md).

use ab_glyph::{FontArc, FontVec, VariableFont};
use resvg::usvg::fontdb;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Font files larger than this are not loaded for titles.
const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;
/// Faces kept loaded; more distinct fonts than this start the cache over.
const MAX_CACHED: usize = 32;
/// Families listed in the font menu.
const MAX_FAMILIES: usize = 2000;

/// Family in lowercase, bold, italic.
type FaceKey = (String, bool, bool);

/// A font ready to draw a title with.
#[derive(Clone)]
pub struct Face {
    pub font: FontArc,
    /// Bold was asked for and the face is lighter: draw it bolder.
    pub embolden: bool,
    /// Italic was asked for and the face is upright: slant it.
    pub slant: bool,
}

/// All fonts of the system, scanned on first use. Plugins' SVG images share
/// it.
pub fn system() -> Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// Installed families by name, sorted, for the font menu. `None` until a
/// background scan is done; `ready` runs once it is.
pub fn families(ready: impl FnOnce() + Send + 'static) -> Option<Arc<Vec<String>>> {
    static FAMILIES: OnceLock<Arc<Vec<String>>> = OnceLock::new();
    static STARTED: OnceLock<()> = OnceLock::new();
    if let Some(list) = FAMILIES.get() {
        return Some(list.clone());
    }
    if STARTED.set(()).is_ok() {
        let spawned = std::thread::Builder::new()
            .name("deckly-fonts".into())
            .spawn(move || {
                let _ = FAMILIES.set(Arc::new(family_names(&system())));
                ready();
            });
        if let Err(e) = spawned {
            eprintln!("deckly: cannot list fonts: {e}");
            let _ = FAMILIES.set(Arc::default());
        }
    }
    FAMILIES.get().cloned()
}

fn family_names(db: &fontdb::Database) -> Vec<String> {
    let mut names: Vec<String> = db
        .faces()
        .filter_map(|f| f.families.first())
        .map(|(name, _)| name.trim())
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .map(str::to_owned)
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names.truncate(MAX_FAMILIES);
    names
}

/// The face for a title: `family` in bold and italic as asked. An empty or
/// unknown family is the default font.
pub fn face(family: &str, bold: bool, italic: bool) -> Option<Face> {
    static CACHE: OnceLock<Mutex<HashMap<FaceKey, Option<Face>>>> = OnceLock::new();
    let key = (family.trim().to_lowercase(), bold, italic);
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(face) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return face.clone();
    }
    // Load without the lock: the first named family scans the system.
    let face = if key.0.is_empty() {
        default_face(bold, italic)
    } else {
        system_face(&system(), &key.0, bold, italic).or_else(|| face("", bold, italic))
    };
    let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
    if cache.len() >= MAX_CACHED {
        cache.clear();
    }
    cache.insert(key, face.clone());
    face
}

/// Whether `family` is installed, ignoring case. Scans the system fonts.
pub fn installed(family: &str) -> bool {
    find_family(&system(), family.trim()).is_some()
}

fn find_family<'a>(db: &'a fontdb::Database, family: &str) -> Option<&'a str> {
    db.faces()
        .flat_map(|f| f.families.iter())
        .map(|(name, _)| name.as_str())
        .find(|name| name.eq_ignore_ascii_case(family))
}

fn system_face(db: &fontdb::Database, family: &str, bold: bool, italic: bool) -> Option<Face> {
    let name = find_family(db, family)?;
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(name)],
        weight: if bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        },
        style: if italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        },
        ..fontdb::Query::default()
    };
    let id = db.query(&query)?;
    let info = db.face(id)?;
    let font = db.with_face_data(id, parse)??;
    Some(styled(
        font,
        bold,
        italic,
        bold && info.weight.0 < 600,
        italic && info.style == fontdb::Style::Normal,
    ))
}

fn parse(data: &[u8], index: u32) -> Option<FontVec> {
    if data.len() > MAX_FONT_BYTES {
        return None;
    }
    FontVec::try_from_vec_and_index(data.to_vec(), index).ok()
}

/// A variable font is set to the weight and slant asked for; the system
/// lists it once, at its default. Anything it cannot do is drawn.
fn styled(mut font: FontVec, bold: bool, italic: bool, embolden: bool, slant: bool) -> Face {
    let axes = font.variations();
    let axis = |tag: &[u8; 4]| axes.iter().find(|a| &a.tag == tag);
    let mut face = (embolden, slant);
    if let Some(weight) = axis(b"wght") {
        let wanted: f32 = if bold { 700.0 } else { 400.0 };
        // Not `clamp`: a broken font may name a minimum above its maximum.
        let value = wanted.max(weight.min_value).min(weight.max_value);
        font.set_variation(b"wght", value);
        face.0 = bold && value < 600.0;
    }
    if italic {
        if let Some(ital) = axis(b"ital").filter(|a| a.max_value >= 1.0) {
            font.set_variation(b"ital", ital.max_value.min(1.0));
            face.1 = false;
        } else if let Some(slnt) = axis(b"slnt").filter(|a| a.min_value <= -5.0) {
            font.set_variation(b"slnt", slnt.min_value.max(-12.0));
            face.1 = false;
        }
    }
    Face {
        font: FontArc::new(font),
        embolden: face.0,
        slant: face.1,
    }
}

/// Files of the default font, by bold and italic. The first that exists
/// wins; these ship with the system.
fn default_files(bold: bool, italic: bool) -> &'static [&'static str] {
    if cfg!(windows) {
        match (bold, italic) {
            (false, false) => &[
                r"C:\Windows\Fonts\segoeui.ttf",
                r"C:\Windows\Fonts\arial.ttf",
            ],
            (true, false) => &[
                r"C:\Windows\Fonts\segoeuib.ttf",
                r"C:\Windows\Fonts\arialbd.ttf",
            ],
            (false, true) => &[
                r"C:\Windows\Fonts\segoeuii.ttf",
                r"C:\Windows\Fonts\ariali.ttf",
            ],
            (true, true) => &[
                r"C:\Windows\Fonts\segoeuiz.ttf",
                r"C:\Windows\Fonts\arialbi.ttf",
            ],
        }
    } else if cfg!(target_os = "macos") {
        match (bold, italic) {
            (false, false) => &["/System/Library/Fonts/Supplemental/Arial.ttf"],
            (true, false) => &["/System/Library/Fonts/Supplemental/Arial Bold.ttf"],
            (false, true) => &["/System/Library/Fonts/Supplemental/Arial Italic.ttf"],
            (true, true) => &["/System/Library/Fonts/Supplemental/Arial Bold Italic.ttf"],
        }
    } else {
        match (bold, italic) {
            (false, false) => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/TTF/DejaVuSans.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            ],
            (true, false) => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
            ],
            (false, true) => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
                "/usr/share/fonts/TTF/DejaVuSans-Oblique.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans-Oblique.ttf",
            ],
            (true, true) => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-BoldOblique.ttf",
                "/usr/share/fonts/TTF/DejaVuSans-BoldOblique.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans-BoldOblique.ttf",
            ],
        }
    }
}

/// The default font in the closest variant found: the exact one, then the
/// same weight upright, then the other weight. egui's bundled font is the
/// last resort and is always there.
fn default_face(bold: bool, italic: bool) -> Option<Face> {
    let variants = [
        (bold, italic),
        (bold, false),
        (!bold, italic),
        (!bold, false),
    ];
    for (b, i) in variants {
        for path in default_files(b, i) {
            if let Ok(bytes) = std::fs::read(path)
                && let Some(font) = parse(&bytes, 0)
            {
                return Some(styled(font, bold, italic, bold && !b, italic && !i));
            }
        }
    }
    let defaults = eframe::egui::FontDefinitions::default();
    let data = defaults.font_data.get("Ubuntu-Light")?;
    Some(styled(parse(&data.font, 0)?, bold, italic, bold, italic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_of_the_default_font_loads() {
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(face("", bold, italic).is_some());
        }
    }

    #[test]
    fn unknown_families_fall_back_to_the_default_font() {
        let face = face("No Such Font Family 1234", true, true).unwrap();
        let default = super::face("", true, true).unwrap();
        assert_eq!(face.embolden, default.embolden);
        assert_eq!(face.slant, default.slant);
        assert!(!installed("No Such Font Family 1234"));
    }

    #[test]
    fn broken_and_oversized_font_data_is_refused() {
        assert!(parse(b"not a font", 0).is_none());
        assert!(parse(&vec![0; MAX_FONT_BYTES + 1], 0).is_none());
    }

    #[test]
    fn families_are_sorted_without_duplicates() {
        let mut db = fontdb::Database::new();
        let defaults = eframe::egui::FontDefinitions::default();
        for data in defaults.font_data.values() {
            db.load_font_data(data.font.to_vec());
            db.load_font_data(data.font.to_vec());
        }
        let names = family_names(&db);
        assert!(!names.is_empty());
        let mut sorted = names.clone();
        sorted.sort_by_key(|n| n.to_lowercase());
        sorted.dedup();
        assert_eq!(names, sorted);
    }

    #[test]
    fn missing_variants_are_drawn_bolder_or_slanted() {
        // egui's Ubuntu-Light is one light upright face.
        let mut db = fontdb::Database::new();
        let defaults = eframe::egui::FontDefinitions::default();
        let data = defaults.font_data.get("Ubuntu-Light").unwrap();
        db.load_font_data(data.font.to_vec());
        let family = family_names(&db).remove(0);
        let face = system_face(&db, &family.to_uppercase(), true, true).unwrap();
        assert!(face.embolden && face.slant);
        let face = system_face(&db, &family, false, false).unwrap();
        assert!(!face.embolden && !face.slant);
    }
}

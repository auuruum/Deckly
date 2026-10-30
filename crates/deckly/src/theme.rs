//! Colours, sizes, fonts and egui style.
//!
//! Colours and sizes follow measurements of the official app's main window
//! (docs/clean-room/2026-10-25-main-window.md); all drawing code is Deckly's
//! own.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke,
};

// Left column: canvas and property inspector.
pub const CANVAS: Color32 = Color32::from_rgb(0x2D, 0x2D, 0x2D);
pub const SPLITTER: Color32 = Color32::from_rgb(0x22, 0x22, 0x22);
pub const KEY_BORDER: Color32 = Color32::from_rgb(0x47, 0x47, 0x47);
pub const KEY_EMPTY: Color32 = Color32::from_rgb(0x22, 0x22, 0x22);
pub const KEY_SELECTED: Color32 = Color32::from_rgb(0xC8, 0xC8, 0xC8);
pub const KEY_DROP: Color32 = Color32::from_rgb(0x3E, 0x7A, 0xC0);
pub const PAGER: Color32 = Color32::from_rgb(0x37, 0x37, 0x37);
pub const PAGER_CHIP: Color32 = Color32::from_rgb(0xC8, 0xC8, 0xC8);
pub const PAGER_CHIP_TEXT: Color32 = Color32::from_rgb(0x2D, 0x2D, 0x2D);
pub const MEMBER_ROW: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);

// Right column: action list.
pub const LIST: Color32 = Color32::from_rgb(0x22, 0x22, 0x22);
pub const LIST_HEADER: Color32 = Color32::from_rgb(0x29, 0x29, 0x29);
pub const LIST_HOVER: Color32 = Color32::from_rgb(0x2E, 0x2E, 0x2E);
pub const SEARCH: Color32 = Color32::from_rgb(0x3B, 0x3B, 0x3B);
pub const TAB_SELECTED: Color32 = Color32::from_rgb(0x34, 0x34, 0x34);
pub const CHEVRON: Color32 = Color32::from_rgb(0x92, 0x92, 0x92);
pub const ITEM_ICON: Color32 = Color32::from_rgb(0xA6, 0xA6, 0xA6);

// Text.
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xE6);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x96, 0x96, 0x96);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x68, 0x68, 0x68);

// Controls and overlays.
pub const FIELD: Color32 = Color32::from_rgb(0x3B, 0x3B, 0x3B);
pub const POPUP: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
pub const ACCENT: Color32 = Color32::from_rgb(0x3E, 0x7A, 0xC0);
pub const WARNING: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x3C);

// Preferences window.
pub const PREF_STRIP: Color32 = Color32::from_rgb(0x19, 0x19, 0x19);
pub const PREF_BODY: Color32 = Color32::from_rgb(0x30, 0x30, 0x30);
pub const PREF_TAB_BORDER: Color32 = Color32::from_rgb(0x45, 0x45, 0x45);
pub const PREF_PANEL: Color32 = Color32::from_rgb(0x29, 0x29, 0x29);
pub const PREF_PANEL_BORDER: Color32 = Color32::from_rgb(0x22, 0x22, 0x22);
pub const PREF_FIELD: Color32 = Color32::from_rgb(0x3D, 0x3D, 0x3D);
pub const PREF_SEGMENT: Color32 = Color32::from_rgb(0x55, 0x55, 0x55);
pub const PREF_CHECK: Color32 = Color32::from_rgb(0x36, 0x7A, 0xF6);
pub const PREF_ROW_SELECTED: Color32 = Color32::from_rgb(0x00, 0x78, 0xFF);

// Sizes at 100% scale.
pub const LIST_WIDTH: f32 = 298.0;
pub const KEY_SIZE: f32 = 70.0;
pub const KEY_PITCH: f32 = 84.0;
pub const KEY_BORDER_WIDTH: f32 = 3.0;
pub const CATEGORY_ROW: f32 = 48.0;
pub const ACTION_ROW: f32 = 40.0;

const BOLD: &str = "bold";

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let (regular, bold) = system_ui_fonts();
    if let Some(bytes) = regular {
        fonts
            .font_data
            .insert("system-ui".into(), FontData::from_owned(bytes).into());
        if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
            family.insert(0, "system-ui".into());
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // The bold family always exists; without a bold system font it falls
    // back to the regular fonts.
    let mut bold_family = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(bytes) = bold {
        fonts
            .font_data
            .insert("system-ui-bold".into(), FontData::from_owned(bytes).into());
        bold_family.insert(0, "system-ui-bold".into());
    }
    fonts
        .families
        .insert(FontFamily::Name(BOLD.into()), bold_family);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.override_text_color = Some(TEXT);
        v.panel_fill = CANVAS;
        v.window_fill = POPUP;
        v.window_stroke = Stroke::new(1.0, KEY_BORDER);
        v.extreme_bg_color = FIELD;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.7);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        let radius = CornerRadius::same(4);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(2.0, SPLITTER);
        v.widgets.inactive.weak_bg_fill = PAGER;
        v.widgets.inactive.bg_fill = FIELD;
        v.widgets.inactive.bg_stroke = Stroke::NONE;
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x44, 0x44, 0x44);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, KEY_BORDER);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 25.0;
        style.spacing.scroll = egui::style::ScrollStyle::thin();
    });
}

fn system_ui_fonts() -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let (regular, bold): (&[&str], &[&str]) = if cfg!(windows) {
        (
            &[r"C:\Windows\Fonts\segoeui.ttf"],
            &[r"C:\Windows\Fonts\segoeuib.ttf"],
        )
    } else if cfg!(target_os = "macos") {
        (
            &["/System/Library/Fonts/Supplemental/Arial.ttf"],
            &["/System/Library/Fonts/Supplemental/Arial Bold.ttf"],
        )
    } else {
        (
            &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/TTF/DejaVuSans.ttf",
            ],
            &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            ],
        )
    };
    let read = |paths: &[&str]| paths.iter().find_map(|p| std::fs::read(p).ok());
    (read(regular), read(bold))
}

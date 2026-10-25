//! Colours, fonts and egui style. The dark layout follows the official
//! app's editor window; all drawing code is Deckly's own.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke};

pub const CANVAS: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1D);
pub const TOOLBAR: Color32 = Color32::from_rgb(0x26, 0x26, 0x2A);
pub const PANEL: Color32 = Color32::from_rgb(0x21, 0x21, 0x25);
pub const INSPECTOR: Color32 = Color32::from_rgb(0x24, 0x24, 0x28);
pub const DEVICE: Color32 = Color32::from_rgb(0x0E, 0x0E, 0x10);
pub const DEVICE_EDGE: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x31);
pub const KEY_EDGE: Color32 = Color32::from_rgb(0x30, 0x30, 0x35);
pub const FIELD: Color32 = Color32::from_rgb(0x16, 0x16, 0x19);
pub const FIELD_STROKE: Color32 = Color32::from_rgb(0x36, 0x36, 0x3C);
pub const ROW_HOVER: Color32 = Color32::from_rgb(0x2E, 0x2E, 0x33);
pub const ICON_TILE: Color32 = Color32::from_rgb(0x33, 0x33, 0x39);
pub const DIVIDER: Color32 = Color32::from_rgb(0x14, 0x14, 0x16);
pub const ACCENT: Color32 = Color32::from_rgb(0x3D, 0x8B, 0xFD);
pub const TEXT: Color32 = Color32::from_rgb(0xED, 0xED, 0xF0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9A, 0x9A, 0xA3);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5E, 0x5E, 0x66);
pub const WARNING: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x3C);

pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Some(bytes) = system_ui_font() {
        fonts
            .font_data
            .insert("system-ui".into(), FontData::from_owned(bytes).into());
        if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
            family.insert(0, "system-ui".into());
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.override_text_color = Some(TEXT);
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.extreme_bg_color = FIELD;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.6);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        let radius = CornerRadius::same(5);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
        }
        v.widgets.inactive.weak_bg_fill = ICON_TILE;
        v.widgets.inactive.bg_fill = FIELD;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, FIELD_STROKE);
        v.widgets.hovered.weak_bg_fill = ROW_HOVER;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, DIVIDER);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
    });
}

fn system_ui_font() -> Option<Vec<u8>> {
    let candidates: &[&str] = if cfg!(windows) {
        &[r"C:\Windows\Fonts\segoeui.ttf"]
    } else if cfg!(target_os = "macos") {
        &["/System/Library/Fonts/Supplemental/Arial.ttf"]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ]
    };
    candidates.iter().find_map(|p| std::fs::read(p).ok())
}

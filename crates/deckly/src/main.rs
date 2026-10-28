//! Deckly — an unofficial, independent app for Stream Deck hardware.
//! Not made, approved or supported by Elgato or Corsair.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod app;
mod cropper;
mod logo;
mod os;
mod plugins;
mod prefs;
mod render;
mod runtime;
mod store;
mod theme;

fn main() -> eframe::Result {
    // Deckly runs each plugin through a copy of itself in guard mode.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args
        .first()
        .is_some_and(|a| a == deckly_plugin_host::GUARD_ARG)
    {
        std::process::exit(deckly_plugin_host::run_guard(&args[1..]));
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Deckly")
            .with_app_id("deckly")
            .with_icon(logo::icon(256))
            .with_inner_size([852.0, 742.0])
            .with_min_inner_size([852.0, 742.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Deckly",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

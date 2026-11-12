//! Deckly — an unofficial, independent app for Stream Deck hardware.
//! Not made, approved or supported by Elgato or Corsair.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod animation;
mod app;
mod app_picker;
mod auto_switch;
mod background;
mod backup;
mod clipboard;
mod cropper;
mod ffmpeg;
mod fonts;
mod forms;
mod history;
mod image_editor;
mod launch;
mod lock_gate;
mod logo;
mod official;
mod os;
mod plugins;
mod prefs;
mod render;
mod runtime;
mod secrets;
mod sequence;
mod store;
mod theme;
mod transfer;
mod update;
mod video;
mod video_setup;
mod wallpaper;

fn main() -> eframe::Result {
    // Deckly runs each plugin through a copy of itself in guard mode.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args
        .first()
        .is_some_and(|a| a == deckly_plugin_host::GUARD_ARG)
    {
        std::process::exit(deckly_plugin_host::run_guard(&args[1..]));
    }
    // Started by an update: the old Deckly is still quitting.
    let wait = if std::env::var_os(update::AFTER_UPDATE_ENV).is_some() {
        std::time::Duration::from_secs(30)
    } else {
        std::time::Duration::ZERO
    };
    let instance = match store::config_dir().map(|dir| os::instance::claim(&dir, wait)) {
        Some(Ok(Some(instance))) => Some(instance),
        Some(Ok(None)) => return Ok(()),
        Some(Err(e)) => {
            eprintln!("deckly: cannot check for a running Deckly: {e}");
            None
        }
        None => None,
    };
    let background = background::Background::start(instance);
    let start_in_tray = runtime::lock(&background.state).settings.start_in_tray;
    let launch = launch::Launch::from_args(&args);
    let mut hidden = !launch::opens_window(launch, start_in_tray, || background.tray_ready());
    if hidden && background::TRAY_WITHOUT_WINDOW {
        // No window until the user asks for it, as after closing one.
        background.set_closing_to_tray(true);
        if !background.wait_for_window() {
            return Ok(());
        }
        hidden = false;
    }
    loop {
        let window = background.clone();
        let result = eframe::run_native(
            "Deckly",
            options(hidden),
            Box::new(move |cc| Ok(Box::new(app::App::new(cc, window, hidden)))),
        );
        hidden = false;
        if let Err(e) = result {
            background.shutdown();
            return Err(e);
        }
        if !background.wait_for_window() {
            return Ok(());
        }
    }
}

/// `hidden`: the window starts hidden (only on macOS, see
/// [`background::TRAY_WITHOUT_WINDOW`]).
fn options(hidden: bool) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Deckly")
            .with_app_id("deckly")
            .with_icon(logo::icon(256))
            .with_inner_size([852.0, 742.0])
            .with_min_inner_size([852.0, 742.0]),
        ..Default::default()
    };
    lean_wgpu(&mut options.wgpu_options.wgpu_setup);
    // A start in the tray at login leaves the focus where it is. Showing the
    // window later activates Deckly through `ViewportCommand::Focus`.
    #[cfg(target_os = "macos")]
    if hidden {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            builder.with_activate_ignoring_other_apps(false);
        }));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = hidden;
    options
}

/// The editor draws a few small textures, so wgpu's default memory blocks
/// (64-256 MB each) are mostly waste. On Windows, initializing Vulkan and GL
/// next to DX12 also loads every one of those drivers into the process.
/// `WGPU_BACKEND` still overrides the backend.
fn lean_wgpu(setup: &mut eframe::egui_wgpu::WgpuSetup) {
    use eframe::egui_wgpu::{WgpuSetup, wgpu};
    let WgpuSetup::CreateNew(setup) = setup else {
        return;
    };
    if cfg!(windows) && wgpu::Backends::from_env().is_none() {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
    }
    let device = std::sync::Arc::clone(&setup.device_descriptor);
    setup.device_descriptor = std::sync::Arc::new(move |adapter| wgpu::DeviceDescriptor {
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..device(adapter)
    });
}

#[cfg(test)]
mod tests {
    use eframe::egui_wgpu::{WgpuSetup, wgpu};

    #[test]
    fn windows_uses_only_dx12_unless_overridden() {
        let mut setup = eframe::egui_wgpu::WgpuConfiguration::default().wgpu_setup;
        super::lean_wgpu(&mut setup);
        let WgpuSetup::CreateNew(setup) = setup else {
            panic!("default setup creates a new device");
        };
        if cfg!(windows) && wgpu::Backends::from_env().is_none() {
            assert_eq!(setup.instance_descriptor.backends, wgpu::Backends::DX12);
        }
    }
}

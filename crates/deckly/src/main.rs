//! Deckly — an unofficial, independent app for Stream Deck hardware.
//! Not made, approved or supported by Elgato or Corsair.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod app;
mod background;
mod clipboard;
mod cropper;
mod history;
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
    let instance = match store::config_dir().map(|dir| os::instance::claim(&dir)) {
        Some(Ok(Some(instance))) => Some(instance),
        Some(Ok(None)) => return Ok(()),
        Some(Err(e)) => {
            eprintln!("deckly: cannot check for a running Deckly: {e}");
            None
        }
        None => None,
    };
    let background = background::Background::start(instance);
    loop {
        let window = background.clone();
        let result = eframe::run_native(
            "Deckly",
            options(),
            Box::new(|cc| Ok(Box::new(app::App::new(cc, window)))),
        );
        if let Err(e) = result {
            background.shutdown();
            return Err(e);
        }
        if !background.wait_for_window() {
            return Ok(());
        }
    }
}

fn options() -> eframe::NativeOptions {
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

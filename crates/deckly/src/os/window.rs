//! Keeps a window that starts hidden from showing for a moment.
//!
//! eframe shows its window once the first frame is drawn, even a window
//! meant to stay hidden; Deckly's hide follows in the same pass. That is
//! only an issue on macOS, the one system where a start in the tray creates
//! the window (its tray needs the event loop): there the window is fully
//! transparent until Deckly shows it. Windows and Linux create no window
//! for a start in the tray, so there is nothing to conceal.

pub use imp::{Concealed, conceal};

#[cfg(target_os = "macos")]
mod imp {
    use eframe::egui_wgpu::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
    use objc2::rc::Retained;
    use objc2_app_kit::{NSView, NSWindow};

    pub struct Concealed(Retained<NSWindow>);

    /// Call on the main thread, before the window's first frame.
    pub fn conceal(handle: &impl HasWindowHandle) -> Option<Concealed> {
        let RawWindowHandle::AppKit(handle) = handle.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: an AppKit handle points at the NSView that winit made for
        // this window; eframe keeps it alive while it hands out the handle,
        // and this runs on the main thread that owns it. `window` retains
        // what it returns, so nothing borrowed outlives this call.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let window = view.window()?;
        window.setAlphaValue(0.0);
        Some(Concealed(window))
    }

    impl Concealed {
        /// Call on the main thread, right before showing the window.
        pub fn reveal(self) {
            self.0.setAlphaValue(1.0);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub struct Concealed;

    pub fn conceal<T>(_handle: &T) -> Option<Concealed> {
        None
    }

    impl Concealed {
        pub fn reveal(self) {}
    }
}

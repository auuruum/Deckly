//! What runs without a window: devices, plugins and the tray.
//!
//! winit cannot hide a Wayland window, so on Linux closing the window really
//! closes it: `main` ends the window's event loop and runs it again when the
//! user wants the window back. Windows and macOS need the event loop for
//! the tray, so there the window is hidden instead. Either way this outlives
//! the window, and each window gets a new [`App`] over it.

use crate::os::instance::Instance;
use crate::os::tray::{self, Command};
use crate::runtime::{self, Msg, Shared};
use crate::{plugins, store};
use deckly_plugin_host::Host;
use eframe::egui;
use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const REOPENS_WINDOW: bool = cfg!(target_os = "linux");

/// Reaches the window, if one is open, from any thread.
#[derive(Clone)]
struct Wake {
    commands: Sender<Command>,
    window: Arc<Mutex<Option<egui::Context>>>,
}

impl Wake {
    fn window(&self) -> Option<egui::Context> {
        let window = self.window.lock();
        window.unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set_window(&self, ctx: Option<egui::Context>) {
        let window = self.window.lock();
        *window.unwrap_or_else(|p| p.into_inner()) = ctx;
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
        self.repaint();
    }

    fn repaint(&self) {
        if let Some(window) = self.window() {
            window.request_repaint();
        }
    }

    /// With no window open, `main` is the one to update the tray menu.
    fn changed(&self) {
        match self.window() {
            Some(window) => window.request_repaint(),
            None => {
                let _ = self.commands.send(Command::Refresh);
            }
        }
    }
}

pub struct Background {
    pub state: Shared,
    pub runtime: Sender<Msg>,
    /// `None` when the plugin host could not start.
    pub host: Option<Host>,
    /// The window is hidden, so plugins need not repaint it.
    pub hidden: Arc<AtomicBool>,
    wake: Wake,
    /// From the tray and from later starts of the program.
    commands: Receiver<Command>,
    /// Added with the first window: macOS needs the event loop running.
    /// Holds `None` when this desktop has no tray.
    tray: OnceCell<Option<tray_icon::TrayIcon>>,
    /// What the tray menu lists now (`None`: it needs building).
    tray_menu: RefCell<Option<Vec<tray::Device>>>,
    reopens_window: bool,
    /// The window is closing to come back later, not to quit.
    closing_to_tray: Cell<bool>,
}

impl Background {
    pub fn start(instance: Option<Instance>) -> Rc<Self> {
        let (profiles, errors) = store::load_profiles();
        let (settings, settings_error) = store::load_settings();
        let mut state = runtime::State::new(profiles, settings);
        if let Some(e) = settings_error {
            eprintln!("deckly: {e}");
            state.notify("Settings could not be loaded; using defaults");
        }
        if !errors.is_empty() {
            for e in &errors {
                eprintln!("deckly: {e}");
            }
            state.notify(format!(
                "{} profile file(s) could not be loaded",
                errors.len()
            ));
        }
        let (commands_tx, commands) = channel();
        let wake = Wake {
            commands: commands_tx,
            window: Arc::default(),
        };
        if let Some(instance) = instance {
            let wake = wake.clone();
            instance.listen(move || wake.send(Command::Show));
        }
        let state: Shared = Arc::new(Mutex::new(state));
        // What the runtime reports can change the tray menu, so it reaches a
        // hidden or closed window too. Plugin images do not, and can arrive
        // many times a second.
        let repaint: runtime::Repaint = {
            let wake = wake.clone();
            Arc::new(move || wake.changed())
        };
        let hidden = Arc::new(AtomicBool::new(false));
        let plugin_repaint: runtime::Repaint = {
            let wake = wake.clone();
            let hidden = hidden.clone();
            Arc::new(move || {
                if !hidden.load(Ordering::Relaxed) {
                    wake.repaint();
                }
            })
        };
        let (host, host_events) = start_host();
        if host.is_none() {
            runtime::lock(&state).notify("Plugins could not be started");
        }
        let runtime = runtime::start(state.clone(), repaint, host.clone());
        if let (Some(host), Some(events)) = (&host, host_events) {
            plugins::start(
                state.clone(),
                runtime.clone(),
                host.clone(),
                events,
                plugin_repaint,
            );
        }
        Rc::new(Self {
            state,
            runtime,
            host,
            hidden,
            wake,
            commands,
            tray: OnceCell::new(),
            tray_menu: RefCell::new(None),
            reopens_window: REOPENS_WINDOW,
            closing_to_tray: Cell::new(false),
        })
    }

    /// Connects a new window. Returns whether a tray can keep Deckly running
    /// once that window is closed.
    pub fn attach(&self, ctx: &egui::Context) -> bool {
        self.wake.set_window(Some(ctx.clone()));
        let tray = self.tray.get_or_init(|| {
            let wake = self.wake.clone();
            tray::start(move |command| wake.send(command))
                .inspect_err(|e| {
                    eprintln!("deckly: no tray icon: {e}");
                    runtime::lock(&self.state).notify(format!(
                        "No tray icon ({e}); closing the window quits Deckly"
                    ));
                })
                .ok()
        });
        tray.is_some()
    }

    pub fn detach(&self) {
        self.wake.set_window(None);
    }

    pub fn send(&self, command: Command) {
        self.wake.send(command);
    }

    pub fn next_command(&self) -> Option<Command> {
        self.commands.try_recv().ok()
    }

    pub fn reopens_window(&self) -> bool {
        self.reopens_window
    }

    pub fn set_closing_to_tray(&self, closing: bool) {
        self.closing_to_tray.set(closing);
    }

    pub fn closing_to_tray(&self) -> bool {
        self.closing_to_tray.get()
    }

    /// Rebuilds the tray menu when what it lists changed.
    pub fn refresh_tray(&self) {
        let Some(tray) = self.tray.get().and_then(Option::as_ref) else {
            return;
        };
        let devices = runtime::lock(&self.state).tray_devices();
        let mut shown = self.tray_menu.borrow_mut();
        if shown.as_ref() == Some(&devices) {
            return;
        }
        match tray::menu(&devices) {
            Ok(menu) => tray.set_menu(Some(Box::new(menu))),
            Err(e) => eprintln!("deckly: cannot update the tray menu: {e}"),
        }
        *shown = Some(devices);
    }

    pub fn switch_profile(&self, serial: &str, id: String) {
        let mut s = runtime::lock(&self.state);
        if s.profile(&id).is_some() {
            s.settings.assignments.insert(serial.to_owned(), id);
            s.save_settings();
        }
        drop(s);
        let _ = self.runtime.send(Msg::Refresh);
        // A clicked item flips its own check mark, even on the profile
        // already active.
        *self.tray_menu.borrow_mut() = None;
    }

    /// Called once the window's event loop ended. Follows the tray until the
    /// user wants a window again (`true`) or Deckly is to exit (`false`).
    pub fn wait_for_window(&self) -> bool {
        if !self.closing_to_tray.take() {
            return false;
        }
        loop {
            self.refresh_tray();
            match self.commands.recv() {
                Ok(Command::Show) => return true,
                Ok(Command::Preferences) => {
                    self.send(Command::Preferences);
                    return true;
                }
                Ok(Command::Profile { serial, id }) => self.switch_profile(&serial, id),
                Ok(Command::Refresh) => {}
                Ok(Command::Quit) | Err(_) => {
                    self.shutdown();
                    return false;
                }
            }
        }
    }

    /// Stops plugins and returns devices to their own idle screen.
    pub fn shutdown(&self) {
        if let Some(host) = &self.host {
            host.shutdown();
        }
        let _ = self.runtime.send(Msg::Shutdown);
        // Give the writer threads a moment to send the reset.
        std::thread::sleep(Duration::from_millis(150));
    }

    /// Nothing started: no devices, plugins or tray.
    #[cfg(test)]
    pub fn idle(reopens_window: bool) -> Rc<Self> {
        let (commands_tx, commands) = channel();
        Rc::new(Self {
            state: Arc::new(Mutex::new(runtime::State::new(
                Vec::new(),
                Default::default(),
            ))),
            runtime: channel().0,
            host: None,
            hidden: Arc::default(),
            wake: Wake {
                commands: commands_tx,
                window: Arc::default(),
            },
            commands,
            tray: OnceCell::new(),
            tray_menu: RefCell::new(None),
            reopens_window,
            closing_to_tray: Cell::new(false),
        })
    }
}

/// Starts the plugin host; plugins live in the config folder and run with
/// the `deckly-web` helper installed next to Deckly.
fn start_host() -> (
    Option<Host>,
    Option<Receiver<deckly_plugin_host::HostEvent>>,
) {
    let Some(dir) = store::config_dir() else {
        return (None, None);
    };
    let helper = std::env::current_exe().ok().and_then(|exe| {
        let name = if cfg!(windows) {
            "deckly-web.exe"
        } else {
            "deckly-web"
        };
        Some(exe.parent()?.join(name))
    });
    let config = deckly_plugin_host::Config {
        plugins_dir: dir.join("plugins"),
        logs_dir: dir.join("logs").join("plugins"),
        web_helper: helper,
        node: std::env::var_os("DECKLY_NODE").map(PathBuf::from),
        wine: std::env::var_os("DECKLY_WINE").map(PathBuf::from),
        guard: std::env::current_exe().ok(),
        runtimes_dir: dir.join("runtimes"),
        app: deckly_plugin_host::AppInfo {
            language: "en".into(),
            platform_version: crate::os::system_version(),
            font: if cfg!(windows) {
                "Segoe UI".into()
            } else {
                String::new()
            },
        },
    };
    let (tx, rx) = channel();
    match Host::start(config, tx) {
        Ok(host) => (Some(host), Some(rx)),
        Err(e) => {
            eprintln!("deckly: cannot start plugins: {e}");
            (None, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_without_a_close_to_the_tray_ends_deckly() {
        let background = Background::idle(true);
        background.send(Command::Show);
        assert!(!background.wait_for_window());
    }

    #[test]
    fn show_brings_a_new_window() {
        let background = Background::idle(true);
        background.set_closing_to_tray(true);
        background.send(Command::Refresh);
        background.send(Command::Show);
        assert!(background.wait_for_window());
        // The next close decides for itself.
        assert!(!background.closing_to_tray());
    }

    #[test]
    fn preferences_wait_for_the_new_window() {
        let background = Background::idle(true);
        background.set_closing_to_tray(true);
        background.send(Command::Preferences);
        assert!(background.wait_for_window());
        assert_eq!(background.next_command(), Some(Command::Preferences));
    }

    #[test]
    fn profile_changes_keep_waiting_and_quit_ends() {
        let background = Background::idle(true);
        background.set_closing_to_tray(true);
        background.send(Command::Profile {
            serial: "S1".into(),
            id: "no such profile".into(),
        });
        background.send(Command::Quit);
        assert!(!background.wait_for_window());
        assert!(
            runtime::lock(&background.state)
                .settings
                .assignments
                .is_empty()
        );
    }

    #[test]
    fn with_no_window_changes_reach_main() {
        let background = Background::idle(true);
        background.wake.changed();
        assert_eq!(background.next_command(), Some(Command::Refresh));
        // A plugin image has nothing to do without a window.
        background.wake.repaint();
        assert_eq!(background.next_command(), None);
    }
}

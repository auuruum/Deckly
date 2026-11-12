//! What runs without a window: devices, plugins and the tray.
//!
//! winit cannot hide a Wayland window, so on Linux closing the window really
//! closes it: `main` ends the window's event loop and runs it again when the
//! user wants the window back. Windows and macOS need the event loop for
//! the tray, so there the window is hidden instead. Either way this outlives
//! the window, and each window gets a new [`App`] over it.
//!
//! A start in the tray (see [`crate::launch`]) opens no window on Linux and
//! Windows: the tray works without one there, so `main` waits here until the
//! user asks for the window. macOS needs the event loop for its tray, so
//! there the first window starts hidden.

use crate::os::instance::Instance;
use crate::os::main_thread;
use crate::os::tray::{self, Command};
use crate::runtime::{self, Msg, Shared};
use crate::update::Updater;
use crate::{plugins, store};
use deckly_plugin_host::Host;
use eframe::egui;
use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REOPENS_WINDOW: bool = cfg!(target_os = "linux");
/// The tray runs while no window is open: through D-Bus on Linux, through
/// the main thread's messages on Windows ([`main_thread`]).
pub const TRAY_WITHOUT_WINDOW: bool = cfg!(any(windows, target_os = "linux"));
/// At login the tray may come up after Deckly. This long Deckly waits for it
/// before it opens the window instead, and this often it looks again.
const TRAY_WAIT: Duration = Duration::from_secs(10);
const TRAY_RETRY: Duration = Duration::from_secs(1);

/// What came of waiting for a tray.
#[derive(Debug, PartialEq, Eq)]
enum TrayWait {
    Ready,
    /// None came up in time.
    Missing,
    /// The user asked for the window meanwhile; the command is queued again.
    WindowWanted,
}

/// Reaches the window, if one is open, from any thread.
#[derive(Clone)]
struct Wake {
    commands: Sender<Command>,
    window: Arc<Mutex<Option<egui::Context>>>,
    /// With no window open, `main` waits on the main thread.
    main: main_thread::Waker,
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
        match self.window() {
            Some(window) => window.request_repaint(),
            None => self.main.wake(),
        }
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
                self.main.wake();
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
    /// Update checks and downloads; outlives each window.
    pub updater: Updater,
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
        // `main` calls this on the main thread, which is the one to wake.
        let wake = Wake {
            commands: commands_tx,
            window: Arc::default(),
            main: main_thread::Waker::for_this_thread(),
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
        let updater = Updater::new(store::config_dir(), {
            let wake = wake.clone();
            Arc::new(move || wake.changed())
        });
        {
            let state = state.clone();
            updater.start_daily(move || runtime::lock(&state).settings.check_updates);
        }
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
            updater,
        })
    }

    /// Connects a new window. Returns whether a tray can keep Deckly running
    /// once that window is closed.
    pub fn attach(&self, ctx: &egui::Context) -> bool {
        self.wake.set_window(Some(ctx.clone()));
        self.start_tray()
    }

    /// Adds the tray icon once; says whether there is one. Call on the main
    /// thread: with the event loop running on macOS, any time elsewhere.
    fn start_tray(&self) -> bool {
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

    /// Sets up the tray for a start without the window and says whether
    /// that worked. Where the tray needs the event loop (macOS), the window
    /// starts hidden and opens by itself if no tray comes up; see
    /// [`crate::app::App::new`].
    pub fn tray_ready(&self) -> bool {
        if !TRAY_WITHOUT_WINDOW {
            return true;
        }
        match self.wait_for_tray(tray::available, TRAY_WAIT, TRAY_RETRY) {
            TrayWait::Ready => self.start_tray(),
            TrayWait::Missing | TrayWait::WindowWanted => false,
        }
    }

    /// Waits up to `limit` for `available`, asking again every `retry`, and
    /// meanwhile hears later starts of the program, which want the window.
    fn wait_for_tray(
        &self,
        available: impl Fn() -> Result<(), String>,
        limit: Duration,
        retry: Duration,
    ) -> TrayWait {
        let deadline = Instant::now() + limit;
        loop {
            let error = match available() {
                Ok(()) => return TrayWait::Ready,
                Err(e) => e,
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                eprintln!(
                    "deckly: no tray after {}s ({error}); opening the window",
                    limit.as_secs()
                );
                return TrayWait::Missing;
            }
            match self.commands.recv_timeout(retry.min(left)) {
                Ok(Command::Refresh) | Err(RecvTimeoutError::Timeout) => {}
                Ok(command) => {
                    // The window handles it once it is open.
                    let _ = self.wake.commands.send(command);
                    return TrayWait::WindowWanted;
                }
                Err(RecvTimeoutError::Disconnected) => return TrayWait::Missing,
            }
        }
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
            s.assign(serial, id);
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
            match main_thread::recv(&self.commands) {
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
                main: main_thread::Waker::for_this_thread(),
            },
            commands,
            tray: OnceCell::new(),
            tray_menu: RefCell::new(None),
            reopens_window,
            closing_to_tray: Cell::new(false),
            updater: Updater::new(None, Arc::new(|| {})),
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
        Ok(host) => {
            // Deckly plugins get their secrets back in place of references.
            host.set_settings_filter(std::sync::Arc::new(|plugin, settings| {
                crate::secrets::reveal(crate::secrets::vault(), plugin, settings);
            }));
            crate::secrets::init();
            (Some(host), Some(rx))
        }
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
    fn a_tray_that_is_there_is_used_at_once() {
        let background = Background::idle(true);
        let wait = background.wait_for_tray(|| Ok(()), Duration::ZERO, Duration::ZERO);
        assert_eq!(wait, TrayWait::Ready);
    }

    #[test]
    fn a_tray_that_comes_up_late_is_waited_for() {
        let background = Background::idle(true);
        let asked = Cell::new(0);
        let available = || {
            asked.set(asked.get() + 1);
            if asked.get() < 3 {
                Err("not yet".to_owned())
            } else {
                Ok(())
            }
        };
        let wait =
            background.wait_for_tray(available, Duration::from_secs(5), Duration::from_millis(10));
        assert_eq!(wait, TrayWait::Ready);
        assert_eq!(asked.get(), 3);
    }

    #[test]
    fn without_a_tray_the_wait_ends() {
        let background = Background::idle(true);
        // Menu updates do not end it early.
        background.wake.changed();
        let start = Instant::now();
        let wait = background.wait_for_tray(
            || Err("this desktop has no tray".to_owned()),
            Duration::from_millis(100),
            Duration::from_millis(20),
        );
        assert_eq!(wait, TrayWait::Missing);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_later_start_during_the_wait_opens_the_window() {
        let background = Background::idle(true);
        background.send(Command::Preferences);
        let wait = background.wait_for_tray(
            || Err("not yet".to_owned()),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        assert_eq!(wait, TrayWait::WindowWanted);
        // The window opens Preferences.
        assert_eq!(background.next_command(), Some(Command::Preferences));
    }

    #[test]
    fn a_start_in_the_tray_waits_for_the_window_like_a_close() {
        let background = Background::idle(true);
        background.set_closing_to_tray(true);
        let wake = background.wake.clone();
        let later = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            wake.send(Command::Show);
        });
        assert!(background.wait_for_window());
        later.join().unwrap();
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

//! Host side of the Stream Deck SDK plugin protocol: finds installed
//! plugins, runs each one as its own process, talks to it over a local
//! WebSocket and restarts it when it dies.
//!
//! Plugins are third-party programs. Nothing they send is trusted, and a
//! plugin that crashes, hangs or floods the host only affects itself. See
//! docs/clean-room/2026-10-28-plugin-host.md for the protocol notes.
//!
//! Deckly is an independent app and is not made, approved or supported by
//! Elgato or Corsair.

mod guard;
pub mod install;
pub mod manifest;
mod node;
pub mod protocol;
mod server;

pub use guard::{GUARD_ARG, run_guard};
pub use manifest::{Entry, Manifest};
pub use protocol::{AppInfo, Command, DeviceInfo, Instance};

use server::{Server, ServerEvent};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// A plugin that has not registered this long after starting is restarted.
const REGISTER_DEADLINE: Duration = Duration::from_secs(30);
/// Crashes within this window count towards giving up.
const CRASH_WINDOW: Duration = Duration::from_secs(600);
const MAX_CRASHES: usize = 5;
/// Host notes stop being added to a log larger than this.
const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;

pub struct Config {
    pub plugins_dir: PathBuf,
    pub logs_dir: PathBuf,
    /// `deckly-web`, which runs HTML plugins and property inspectors.
    pub web_helper: Option<PathBuf>,
    /// One Node.js for every plugin, overriding the version each manifest
    /// asks for (a developer setting).
    pub node: Option<PathBuf>,
    /// Where Node.js builds downloaded for plugins are kept.
    pub runtimes_dir: PathBuf,
    /// A program that runs `<guard> --plugin-guard <plugin command>` (see
    /// [`run_guard`]); Deckly passes itself. Without one, plugins run
    /// directly and may outlive a host that crashed.
    pub guard: Option<PathBuf>,
    pub app: AppInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting,
    Running,
    /// Waiting to start again after it stopped.
    Restarting(String),
    /// Waiting for what it needs to run (the Node.js it asks for).
    Installing(String),
    /// Not running, with the reason.
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Plugin {
    /// The plugin id (`com.example.plugin`).
    pub id: String,
    pub dir: PathBuf,
    /// `None` when the manifest cannot be read; the status says why.
    pub manifest: Option<Arc<Manifest>>,
    pub status: Status,
}

impl Plugin {
    pub fn log_file(&self, logs_dir: &Path) -> PathBuf {
        logs_dir.join(format!("{}.log", self.id))
    }
}

#[derive(Debug)]
pub enum HostEvent {
    /// A plugin registered: tell it about the devices and its visible keys.
    Started(String),
    Command {
        plugin: String,
        command: Command,
    },
    /// From the open property inspector, which belongs to `instance`.
    InspectorCommand {
        plugin: String,
        instance: Instance,
        command: Command,
    },
    /// The plugin list or a plugin's status changed.
    Changed,
}

struct OpenInspector {
    plugin: String,
    instance: Instance,
    token: String,
}

#[derive(Default)]
struct Shared {
    plugins: Vec<Plugin>,
    /// Registration token of each running plugin.
    live: HashMap<String, String>,
    devices: Vec<DeviceInfo>,
    inspector: Option<OpenInspector>,
    /// The editor window property inspectors belong to (a Windows HWND).
    owner: Option<i64>,
}

enum Ctl {
    Server(ServerEvent),
    Rescan,
    Restart(String),
    Install(Box<install::Staged>, Sender<Result<String, String>>),
    Remove(String, Sender<Result<(), String>>),
    OpenInspector(Instance),
    CloseInspector,
    Shutdown(Sender<()>),
    PlaceInspector(Option<[i32; 4]>),
    NodeInstalled(String, Result<PathBuf, String>),
}

#[derive(Clone)]
pub struct Host {
    ctl: Sender<Ctl>,
    shared: Arc<Mutex<Shared>>,
    server: Server,
    plugins_dir: PathBuf,
    logs_dir: PathBuf,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|p| p.into_inner())
}

impl Host {
    /// Starts the WebSocket server and every installed plugin.
    pub fn start(config: Config, events: Sender<HostEvent>) -> std::io::Result<Self> {
        std::fs::create_dir_all(&config.plugins_dir)?;
        std::fs::create_dir_all(&config.logs_dir)?;
        install::clean(&config.plugins_dir);
        let (ctl, rx) = channel();
        let server_ctl = Mutex::new(ctl.clone());
        let server = Server::start(move |e| {
            let _ = server_ctl
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .send(Ctl::Server(e));
        })?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let host = Self {
            ctl,
            shared: shared.clone(),
            server: server.clone(),
            plugins_dir: config.plugins_dir.clone(),
            logs_dir: config.logs_dir.clone(),
        };
        let mut supervisor = Supervisor {
            ctl: host.ctl.clone(),
            inspector_place: None,
            node_waiting: HashMap::new(),
            node_fallback: HashMap::new(),
            config,
            server,
            shared,
            events,
            slots: HashMap::new(),
            inspector_child: None,
        };
        std::thread::Builder::new()
            .name("deckly-plugins".into())
            .spawn(move || supervisor.run(rx))?;
        let _ = host.ctl.send(Ctl::Rescan);
        Ok(host)
    }

    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    pub fn plugins(&self) -> Vec<Plugin> {
        lock(&self.shared).plugins.clone()
    }

    /// The plugin that owns an action.
    pub fn plugin_of(&self, action: &str) -> Option<String> {
        lock(&self.shared)
            .plugins
            .iter()
            .find(|p| {
                p.manifest
                    .as_ref()
                    .is_some_and(|m| m.action(action).is_some())
            })
            .map(|p| p.id.clone())
    }

    /// Sends an event to a running plugin; false when it is not running.
    pub fn send(&self, plugin: &str, text: String) -> bool {
        let token = lock(&self.shared).live.get(plugin).cloned();
        if debug() {
            append_log(&self.logs_dir, plugin, &format!("-> {text}"));
        }
        token.is_some_and(|t| self.server.send(&t, text))
    }

    /// Sends an event to every running plugin.
    pub fn broadcast(&self, text: &str) {
        let live: Vec<(String, String)> = lock(&self.shared)
            .live
            .iter()
            .map(|(id, t)| (id.clone(), t.clone()))
            .collect();
        for (id, t) in live {
            if debug() {
                append_log(&self.logs_dir, &id, &format!("-> {text}"));
            }
            self.server.send(&t, text.to_owned());
        }
    }

    /// The devices put into the registration info of plugins started later.
    pub fn set_devices(&self, devices: Vec<DeviceInfo>) {
        lock(&self.shared).devices = devices;
    }

    pub fn open_inspector(&self, instance: Instance) {
        let _ = self.ctl.send(Ctl::OpenInspector(instance));
    }

    pub fn close_inspector(&self) {
        let _ = self.ctl.send(Ctl::CloseInspector);
    }

    /// Makes property inspectors child windows of the editor (a Windows HWND),
    /// so they are clipped to its client area, move with it and hide with it.
    pub fn set_owner_window(&self, handle: i64) {
        lock(&self.shared).owner = Some(handle);
    }

    /// Where the open property inspector is drawn, as a client rectangle in
    /// physical pixels (x, y, width, height) inside the editor's inspector
    /// panel; `None` hides it. Only with an owner window.
    pub fn place_inspector(&self, place: Option<[i32; 4]>) {
        let _ = self.ctl.send(Ctl::PlaceInspector(place));
    }

    /// The key whose property inspector is open.
    pub fn inspector(&self) -> Option<Instance> {
        lock(&self.shared)
            .inspector
            .as_ref()
            .map(|i| i.instance.clone())
    }

    /// Sends an event to the open property inspector if it belongs to
    /// `context`.
    pub fn send_to_inspector(&self, context: &str, text: String) -> bool {
        let token = lock(&self.shared)
            .inspector
            .as_ref()
            .filter(|i| i.instance.context == context)
            .map(|i| i.token.clone());
        token.is_some_and(|t| self.server.send(&t, text))
    }

    pub fn restart(&self, plugin: &str) {
        let _ = self.ctl.send(Ctl::Restart(plugin.to_owned()));
    }

    /// Looks for added or removed plugin folders.
    pub fn rescan(&self) {
        let _ = self.ctl.send(Ctl::Rescan);
    }

    /// Installs a `.streamDeckPlugin` archive or a `.sdPlugin` folder,
    /// replacing an installed copy. Blocks until done; returns the plugin id.
    pub fn install(&self, path: &Path) -> Result<String, String> {
        let staged = if path.is_dir() {
            install::stage_folder(path, &self.plugins_dir)
        } else {
            install::stage_archive(path, &self.plugins_dir)
        }
        .map_err(|e| e.to_string())?;
        let (tx, rx) = channel();
        let _ = self.ctl.send(Ctl::Install(Box::new(staged), tx));
        rx.recv()
            .unwrap_or_else(|_| Err("the plugin host stopped".into()))
    }

    /// Stops and deletes an installed plugin. Blocks until done.
    pub fn remove(&self, plugin: &str) -> Result<(), String> {
        let (tx, rx) = channel();
        let _ = self.ctl.send(Ctl::Remove(plugin.to_owned(), tx));
        rx.recv()
            .unwrap_or_else(|_| Err("the plugin host stopped".into()))
    }

    /// Stops every plugin process. Blocks until they are gone.
    pub fn shutdown(&self) {
        let (tx, rx) = channel();
        if self.ctl.send(Ctl::Shutdown(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
    }
}

#[derive(Default)]
struct Slot {
    child: Option<Child>,
    token: Option<String>,
    started: Option<Instant>,
    registered: bool,
    crashes: Vec<Instant>,
    retry_at: Option<Instant>,
}

struct Supervisor {
    config: Config,
    server: Server,
    shared: Arc<Mutex<Shared>>,
    events: Sender<HostEvent>,
    slots: HashMap<String, Slot>,
    inspector_child: Option<Child>,
    /// Where the editor wants the property inspector drawn.
    inspector_place: Option<[i32; 4]>,
    /// Plugins waiting for a Node.js major being installed.
    node_waiting: HashMap<String, Vec<String>>,
    /// A Node.js of another major used when a major could not be installed.
    node_fallback: HashMap<String, PathBuf>,
    ctl: Sender<Ctl>,
}

impl Supervisor {
    fn run(&mut self, rx: Receiver<Ctl>) {
        loop {
            let msg = match self.next_deadline() {
                Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => break,
                },
            };
            match msg {
                None => self.timers(),
                Some(Ctl::Server(e)) => self.server_event(e),
                Some(Ctl::Rescan) => self.rescan(),
                Some(Ctl::Restart(id)) => {
                    self.stop(&id);
                    if let Some(slot) = self.slots.get_mut(&id) {
                        slot.crashes.clear();
                    }
                    self.launch(&id);
                }
                Some(Ctl::Install(staged, reply)) => {
                    let id = staged.manifest.uuid.clone();
                    self.stop(&id);
                    let result = install::commit(*staged, &self.config.plugins_dir)
                        .map_err(|e| e.to_string());
                    self.slots.remove(&id);
                    self.rescan();
                    let _ = reply.send(result);
                }
                Some(Ctl::Remove(id, reply)) => {
                    self.stop(&id);
                    self.slots.remove(&id);
                    let result =
                        install::remove(&self.config.plugins_dir, &id).map_err(|e| e.to_string());
                    self.rescan();
                    let _ = reply.send(result);
                }
                Some(Ctl::OpenInspector(instance)) => self.open_inspector(instance),
                Some(Ctl::CloseInspector) => self.close_inspector(),
                Some(Ctl::PlaceInspector(place)) => {
                    self.inspector_place = place;
                    self.send_place();
                }
                Some(Ctl::NodeInstalled(major, result)) => self.node_installed(&major, result),
                Some(Ctl::Shutdown(reply)) => {
                    self.close_inspector();
                    let ids: Vec<String> = self.slots.keys().cloned().collect();
                    for id in ids {
                        self.stop(&id);
                    }
                    let _ = reply.send(());
                    break;
                }
            }
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter_map(|s| match (s.retry_at, s.started) {
                (Some(at), _) => Some(at),
                (None, Some(started)) if !s.registered && s.child.is_some() => {
                    Some(started + REGISTER_DEADLINE)
                }
                _ => None,
            })
            .min()
    }

    fn timers(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .slots
            .iter()
            .filter(|(_, s)| s.retry_at.is_some_and(|at| at <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            self.launch(&id);
        }
        let silent: Vec<String> = self
            .slots
            .iter()
            .filter(|(_, s)| {
                !s.registered
                    && s.child.is_some()
                    && s.started.is_some_and(|t| now >= t + REGISTER_DEADLINE)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in silent {
            self.note(&id, "did not connect in time");
            self.crashed(&id);
        }
    }

    fn emit(&self, e: HostEvent) {
        let _ = self.events.send(e);
    }

    fn set_status(&self, id: &str, status: Status) {
        let mut s = lock(&self.shared);
        if let Some(p) = s.plugins.iter_mut().find(|p| p.id == id) {
            p.status = status;
        }
        drop(s);
        self.emit(HostEvent::Changed);
    }

    fn manifest(&self, id: &str) -> Option<(PathBuf, Arc<Manifest>)> {
        lock(&self.shared)
            .plugins
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| Some((p.dir.clone(), p.manifest.clone()?)))
    }

    /// Reads the plugins folder; starts new plugins, stops removed ones.
    fn rescan(&mut self) {
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.config.plugins_dir) {
            for entry in entries.flatten() {
                let dir = entry.path();
                let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(folder_id) = name.strip_suffix(".sdPlugin") else {
                    continue;
                };
                if !dir.is_dir() || !manifest::valid_uuid(folder_id) {
                    continue;
                }
                let (manifest, status) = match Manifest::load(&dir) {
                    Ok(m) if m.uuid == folder_id => (Some(Arc::new(m)), Status::Starting),
                    Ok(m) => (
                        None,
                        Status::Failed(format!("the folder should be named {}.sdPlugin", m.uuid)),
                    ),
                    Err(e) => (None, Status::Failed(e.to_string())),
                };
                found.push(Plugin {
                    id: folder_id.to_owned(),
                    dir,
                    manifest,
                    status,
                });
            }
        }
        found.sort_by_key(|p| {
            p.manifest
                .as_ref()
                .map_or_else(|| p.id.to_lowercase(), |m| m.name.to_lowercase())
        });
        let gone: Vec<String> = self
            .slots
            .keys()
            .filter(|id| !found.iter().any(|p| &p.id == *id))
            .cloned()
            .collect();
        for id in gone {
            self.stop(&id);
            self.slots.remove(&id);
        }
        let start: Vec<String> = found
            .iter()
            .filter(|p| p.manifest.is_some() && !self.slots.contains_key(&p.id))
            .map(|p| p.id.clone())
            .collect();
        {
            let mut s = lock(&self.shared);
            // Plugins that keep running keep their status.
            for p in &mut found {
                if let Some(old) = s.plugins.iter().find(|o| o.id == p.id)
                    && self.slots.contains_key(&p.id)
                {
                    p.status = old.status.clone();
                }
            }
            s.plugins = found;
        }
        for id in start {
            self.launch(&id);
        }
        self.emit(HostEvent::Changed);
    }

    fn launch(&mut self, id: &str) {
        let slot = self.slots.entry(id.to_owned()).or_default();
        slot.retry_at = None;
        let Some((dir, manifest)) = self.manifest(id) else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let info = {
            let s = lock(&self.shared);
            protocol::registration_info(&self.config.app, id, &manifest.version, &s.devices)
        };
        let plugin_args = [
            "-port".to_owned(),
            self.server.port().to_string(),
            "-pluginUUID".to_owned(),
            token.clone(),
            "-registerEvent".to_owned(),
            "registerPlugin".to_owned(),
            "-info".to_owned(),
            info.clone(),
        ];
        let entry = manifest.entry(&dir);
        let web = matches!(entry, Ok(Entry::Html(_)));
        let plugin_args = plugin_args.map(OsString::from);
        let node = match &entry {
            Ok(Entry::Node(_)) => match self.node_for(id, &manifest) {
                Some(node) => Some(node),
                // Started again once Node.js is installed.
                None => return,
            },
            _ => None,
        };
        let command = match entry {
            Ok(Entry::Node(script)) => node.ok_or_else(String::new).map(|node| {
                let mut args: Vec<OsString> = vec![
                    "--no-global-search-paths".into(),
                    "--enable-source-maps".into(),
                ];
                match manifest.node_debug.as_str() {
                    "enabled" => args.push("--inspect=127.0.0.1:0".into()),
                    "break" => args.push("--inspect-brk=127.0.0.1:0".into()),
                    _ => {}
                }
                args.push(script.into());
                args.extend(plugin_args);
                self.guarded(node.into(), args)
            }),
            Ok(Entry::Native(program)) => Ok(self.guarded(program.into(), plugin_args.to_vec())),
            Ok(Entry::Html(page)) => self.web_helper().map(|helper| {
                let mut c = std::process::Command::new(helper);
                c.arg("plugin")
                    .arg(page)
                    .arg(self.server.port().to_string())
                    .arg(&token)
                    .arg("registerPlugin")
                    .arg(&info);
                c
            }),
            Err(e) => Err(e),
        };
        let mut command = match command {
            Ok(c) => c,
            Err(e) => {
                self.set_status(id, Status::Failed(e));
                return;
            }
        };
        let log = self.config.logs_dir.join(format!("{id}.log"));
        // The guard and deckly-web stop when this pipe closes, so plugins
        // never outlive Deckly.
        let lifeline = web || self.config.guard.is_some();
        command.current_dir(&dir).stdin(if lifeline {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        // shortcut: the plugin's own output is not size-limited; the log is
        // truncated at each start. Rotate it if plugins turn out to flood it.
        if let Ok(file) = File::create(&log)
            && let Ok(copy) = file.try_clone()
        {
            command.stdout(file).stderr(copy);
        } else {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        hide_console(&mut command);
        self.server.allow(&token);
        match command.spawn() {
            Ok(child) => {
                let slot = self.slots.entry(id.to_owned()).or_default();
                slot.child = Some(child);
                slot.token = Some(token);
                slot.started = Some(Instant::now());
                slot.registered = false;
                self.set_status(id, Status::Starting);
            }
            Err(e) => {
                self.server.forbid(&token);
                self.note(id, &format!("cannot start: {e}"));
                self.set_status(id, Status::Failed(format!("cannot start: {e}")));
            }
        }
    }

    /// A plugin command, run through the guard when there is one.
    fn guarded(&self, program: OsString, args: Vec<OsString>) -> std::process::Command {
        match &self.config.guard {
            Some(guard) => {
                let mut c = std::process::Command::new(guard);
                c.arg(GUARD_ARG).arg(program).args(args);
                c
            }
            None => {
                let mut c = std::process::Command::new(program);
                c.args(args);
                c
            }
        }
    }

    /// The Node.js a plugin asks for in its manifest, or `None` while it is
    /// being installed; the plugin starts when the install ends.
    fn node_for(&mut self, id: &str, manifest: &Manifest) -> Option<PathBuf> {
        if let Some(node) = &self.config.node {
            return Some(node.clone());
        }
        let major = manifest
            .node_version
            .as_deref()
            .and_then(node::major)
            .unwrap_or_else(|| node::DEFAULT_MAJOR.to_owned());
        if let Some(found) = node::find(&self.config.runtimes_dir, &major)
            .or_else(|| self.node_fallback.get(&major).cloned())
        {
            return Some(found);
        }
        let first = !self.node_waiting.contains_key(&major);
        let waiting = self.node_waiting.entry(major.clone()).or_default();
        if !waiting.iter().any(|w| w == id) {
            waiting.push(id.to_owned());
        }
        if first {
            let ctl = self.ctl.clone();
            let runtimes = self.config.runtimes_dir.clone();
            let m = major.clone();
            let spawned = std::thread::Builder::new()
                .name("deckly-node-install".into())
                .spawn(move || {
                    let result = node::install(&runtimes, &m);
                    let _ = ctl.send(Ctl::NodeInstalled(m, result));
                });
            if let Err(e) = spawned {
                self.node_installed(&major, Err(e.to_string()));
                return None;
            }
        }
        self.note(id, &format!("installing Node.js {major} from nodejs.org"));
        self.set_status(id, Status::Installing(format!("Node.js {major}")));
        None
    }

    /// Starts the plugins that waited for a Node.js install. When it failed
    /// they run on the `node` on `PATH`, if any, with a note in their log.
    fn node_installed(&mut self, major: &str, result: Result<PathBuf, String>) {
        let ids = self.node_waiting.remove(major).unwrap_or_default();
        if let Err(e) = &result {
            let Some(other) = node::on_path() else {
                for id in &ids {
                    self.note(id, &format!("cannot install Node.js {major}: {e}"));
                    let why = format!("needs Node.js {major}, which could not be installed: {e}");
                    self.set_status(id, Status::Failed(why));
                }
                return;
            };
            for id in &ids {
                let note = format!(
                    "cannot install Node.js {major} ({e}); using {} instead",
                    other.display()
                );
                self.note(id, &note);
            }
            self.node_fallback.insert(major.to_owned(), other);
        }
        for id in ids {
            self.launch(&id);
        }
    }

    fn web_helper(&self) -> Result<PathBuf, String> {
        self.config
            .web_helper
            .clone()
            .filter(|p| p.is_file())
            .ok_or_else(|| "needs the deckly-web helper, which is not installed".to_owned())
    }

    /// Stops a plugin's process without restarting it.
    fn stop(&mut self, id: &str) {
        if lock(&self.shared)
            .inspector
            .as_ref()
            .is_some_and(|i| i.plugin == id)
        {
            self.close_inspector();
        }
        let Some(slot) = self.slots.get_mut(id) else {
            return;
        };
        if let Some(token) = slot.token.take() {
            self.server.forbid(&token);
        }
        if let Some(child) = slot.child.take() {
            end_child(child);
        }
        slot.registered = false;
        slot.started = None;
        slot.retry_at = None;
        lock(&self.shared).live.remove(id);
    }

    /// A plugin stopped on its own: restart it after a pause that grows
    /// with each crash, or give up after too many.
    fn crashed(&mut self, id: &str) {
        let exit = self
            .slots
            .get_mut(id)
            .and_then(|s| s.child.as_mut())
            .and_then(|c| c.try_wait().ok().flatten());
        self.stop(id);
        let Some(slot) = self.slots.get_mut(id) else {
            return;
        };
        let now = Instant::now();
        slot.crashes
            .retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        slot.crashes.push(now);
        let reason = match exit.and_then(|s| s.code()) {
            Some(code) => format!("stopped with exit code {code}"),
            None => "stopped".to_owned(),
        };
        let count = slot.crashes.len();
        if count >= MAX_CRASHES {
            self.note(id, &format!("{reason}; giving up after {count} crashes"));
            self.set_status(
                id,
                Status::Failed(format!("{reason} {count} times; see the log")),
            );
            return;
        }
        let delay = Duration::from_secs(1 << (count - 1).min(6));
        slot.retry_at = Some(now + delay);
        self.note(id, &format!("{reason}; restarting in {}s", delay.as_secs()));
        self.set_status(id, Status::Restarting(reason));
    }

    fn plugin_by_token(&self, token: &str) -> Option<String> {
        self.slots
            .iter()
            .find(|(_, s)| s.token.as_deref() == Some(token))
            .map(|(id, _)| id.clone())
    }

    fn server_event(&mut self, e: ServerEvent) {
        let inspector = {
            let s = lock(&self.shared);
            s.inspector
                .as_ref()
                .map(|i| (i.token.clone(), i.plugin.clone(), i.instance.clone()))
        };
        match e {
            ServerEvent::Registered { token, .. } => {
                if let Some((t, plugin, instance)) = &inspector
                    && *t == token
                {
                    self.send(
                        plugin,
                        instance.inspector_event("propertyInspectorDidAppear"),
                    );
                    return;
                }
                let Some(id) = self.plugin_by_token(&token) else {
                    return;
                };
                if let Some(slot) = self.slots.get_mut(&id) {
                    slot.registered = true;
                }
                lock(&self.shared).live.insert(id.clone(), token);
                self.set_status(&id, Status::Running);
                self.emit(HostEvent::Started(id));
            }
            ServerEvent::Message { token, text } => {
                if let Some((t, plugin, instance)) = inspector.clone()
                    && t == token
                {
                    if debug() {
                        self.note(&plugin, &format!("<- inspector {text}"));
                    }
                    match protocol::parse_command(&text) {
                        Ok(Command::SendToPlugin { payload, .. }) => {
                            let msg = protocol::forward(
                                "sendToPlugin",
                                &instance.action,
                                &instance.context,
                                &payload,
                            );
                            self.send(&plugin, msg);
                        }
                        Ok(command) => self.emit(HostEvent::InspectorCommand {
                            plugin,
                            instance,
                            command,
                        }),
                        Err(e) => self.note(&plugin, &format!("bad inspector message: {e}")),
                    }
                    return;
                }
                let Some(id) = self.plugin_by_token(&token) else {
                    return;
                };
                if debug() {
                    self.note(&id, &format!("<- {text}"));
                }
                match protocol::parse_command(&text) {
                    Ok(Command::LogMessage { message }) => self.note(&id, &message),
                    Ok(Command::SendToPropertyInspector { context, payload }) => {
                        if let Some((t, plugin, instance)) = inspector
                            && plugin == id
                            && instance.context == context
                        {
                            let msg = protocol::forward(
                                "sendToPropertyInspector",
                                &instance.action,
                                &context,
                                &payload,
                            );
                            self.server.send(&t, msg);
                        }
                    }
                    Ok(command) => self.emit(HostEvent::Command {
                        plugin: id,
                        command,
                    }),
                    Err(e) => self.note(&id, &format!("bad message: {e}")),
                }
            }
            ServerEvent::Closed { token } => {
                if inspector.as_ref().is_some_and(|(t, _, _)| *t == token) {
                    self.close_inspector();
                    return;
                }
                if let Some(id) = self.plugin_by_token(&token) {
                    self.crashed(&id);
                }
            }
        }
    }

    fn send(&self, plugin: &str, text: String) {
        let token = lock(&self.shared).live.get(plugin).cloned();
        if debug() {
            self.note(plugin, &format!("-> {text}"));
        }
        if let Some(t) = token {
            self.server.send(&t, text);
        }
    }

    fn open_inspector(&mut self, instance: Instance) {
        self.close_inspector();
        let found = lock(&self.shared).plugins.iter().find_map(|p| {
            let m = p.manifest.as_ref()?;
            let action = m.action(&instance.action)?;
            Some((p.id.clone(), p.dir.clone(), m.clone(), action.clone()))
        });
        let Some((id, dir, manifest, action)) = found else {
            return;
        };
        let path = if action.property_inspector.is_empty() {
            &manifest.property_inspector
        } else {
            &action.property_inspector
        };
        let Some(page) = manifest::safe_join(&dir, path).filter(|p| p.is_file()) else {
            return;
        };
        let helper = match self.web_helper() {
            Ok(h) => h,
            Err(e) => {
                self.note(&id, &format!("cannot open settings: {e}"));
                return;
            }
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let info = {
            let s = lock(&self.shared);
            protocol::registration_info(&self.config.app, &id, &manifest.version, &s.devices)
        };
        let title = format!("{} - {}", action.name, manifest.name);
        let mut command = std::process::Command::new(helper);
        command
            .arg("inspector")
            .arg(page)
            .arg(self.server.port().to_string())
            .arg(&token)
            .arg("registerPropertyInspector")
            .arg(&info)
            .arg(instance.action_info())
            .arg(title)
            .args(lock(&self.shared).owner.map(|o| o.to_string()))
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut command);
        self.server.allow(&token);
        match command.spawn() {
            Ok(child) => {
                self.inspector_child = Some(child);
                self.send_place();
                lock(&self.shared).inspector = Some(OpenInspector {
                    plugin: id,
                    instance,
                    token,
                });
            }
            Err(e) => {
                self.server.forbid(&token);
                self.note(&id, &format!("cannot open settings: {e}"));
            }
        }
    }

    fn close_inspector(&mut self) {
        let open = lock(&self.shared).inspector.take();
        if let Some(child) = self.inspector_child.take() {
            end_child(child);
        }
        if let Some(i) = open {
            self.server.forbid(&i.token);
            self.send(
                &i.plugin,
                i.instance.inspector_event("propertyInspectorDidDisappear"),
            );
        }
    }

    /// Tells the property inspector helper where to draw itself, through
    /// its standard input (one line per change).
    fn send_place(&mut self) {
        // Standalone inspectors must stay visible on platforms without an owner.
        if lock(&self.shared).owner.is_none() {
            return;
        }
        let line = match self.inspector_place {
            Some([x, y, w, h]) => format!("place {x} {y} {w} {h}\n"),
            None => "hide\n".to_owned(),
        };
        if let Some(stdin) = self.inspector_child.as_mut().and_then(|c| c.stdin.as_mut()) {
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.flush();
        }
    }

    fn note(&self, id: &str, text: &str) {
        append_log(&self.config.logs_dir, id, text);
    }
}

/// Adds a line of Deckly's own to a plugin's log.
fn append_log(logs_dir: &Path, id: &str, text: &str) {
    let path = logs_dir.join(format!("{id}.log"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        return;
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let line: String = text.chars().take(4096).collect();
        let _ = writeln!(f, "[deckly] {line}");
    }
}

/// DECKLY_PLUGIN_DEBUG=1 writes every message to and from plugins into
/// their logs.
fn debug() -> bool {
    static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEBUG.get_or_init(|| std::env::var_os("DECKLY_PLUGIN_DEBUG").is_some())
}

/// Stops a plugin process. A guard or deckly-web gets its pipe closed so it
/// can stop what it runs first; it is killed if it does not exit in time.
fn end_child(mut child: Child) {
    if child.stdin.take().is_some() {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Console programs would open a window each on Windows.
pub(crate) fn hide_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

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

pub mod install;
pub mod manifest;
pub mod protocol;
mod server;

pub use manifest::{Entry, Manifest};
pub use protocol::{AppInfo, Command, DeviceInfo, Instance};

use server::{Server, ServerEvent};
use std::collections::HashMap;
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
    /// Node.js; found on `PATH` when not given.
    pub node: Option<PathBuf>,
    pub app: AppInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting,
    Running,
    /// Waiting to start again after it stopped.
    Restarting(String),
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
        let command = match entry {
            Ok(Entry::Node(script)) => self.node().map(|node| {
                let mut c = std::process::Command::new(node);
                c.arg("--no-global-search-paths")
                    .arg("--enable-source-maps");
                match manifest.node_debug.as_str() {
                    "enabled" => {
                        c.arg("--inspect=127.0.0.1:0");
                    }
                    "break" => {
                        c.arg("--inspect-brk=127.0.0.1:0");
                    }
                    _ => {}
                }
                c.arg(script).args(&plugin_args);
                c
            }),
            Ok(Entry::Native(program)) => {
                let mut c = std::process::Command::new(program);
                c.args(&plugin_args);
                Ok(c)
            }
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
        // deckly-web exits when this pipe closes, so it never outlives Deckly.
        command
            .current_dir(&dir)
            .stdin(if web { Stdio::piped() } else { Stdio::null() });
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

    fn node(&self) -> Result<PathBuf, String> {
        self.config.node.clone().or_else(find_node).ok_or_else(|| {
            "needs Node.js, which was not found; install Node.js 20 or newer".to_owned()
        })
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
        if let Some(mut child) = slot.child.take() {
            let _ = child.kill();
            let _ = child.wait();
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
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut command);
        self.server.allow(&token);
        match command.spawn() {
            Ok(child) => {
                self.inspector_child = Some(child);
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
        if let Some(mut child) = self.inspector_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(i) = open {
            self.server.forbid(&i.token);
            self.send(
                &i.plugin,
                i.instance.inspector_event("propertyInspectorDidDisappear"),
            );
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

/// Finds `node` on `PATH`.
fn find_node() -> Option<PathBuf> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Console programs would open a window each on Windows.
fn hide_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

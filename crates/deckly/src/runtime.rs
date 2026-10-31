//! The device runtime: connects devices to profiles, keeps key images in
//! sync and runs actions. It lives on its own thread so keys keep working
//! while the window is hidden or busy.
//!
//! The editor and the runtime share one [`State`]. The editor changes it and
//! sends [`Msg::Refresh`] (or [`Msg::Settings`] after a preference changed);
//! the runtime re-renders only keys whose image changed.

use crate::actions::{self, Outcome};
use crate::os::tray;
use crate::plugins::{self, Overlay};
use crate::render::{self, Live, Renderer};
use crate::sequence::{self, Press, Step};
use crate::store;
use deckly_core::profile::MAX_FOLDER_DEPTH;
use deckly_core::{
    DeviceKind, DeviceModel, KeyConfig, Orientation, Page, Profile, Settings, TitleStyle,
};
use deckly_device::{ConnectedDevice, DeviceManager, Event, Handle};
use deckly_plugin_host::{Host, Instance, protocol};
use image::RgbaImage;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub type Shared = Arc<Mutex<State>>;

pub struct State {
    pub profiles: Vec<Profile>,
    pub settings: Settings,
    pub devices: Vec<ConnectedDevice>,
    /// Current page index per profile id.
    pub pages: HashMap<String, usize>,
    /// Open folders per profile id, outermost first.
    pub folders: HashMap<String, Vec<String>>,
    /// Keys held down right now, for the editor's live view.
    pub pressed: HashSet<(Handle, u8)>,
    pub notice: Option<(String, Instant)>,
    /// Installed plugins and their actions.
    pub catalog: plugins::Catalog,
    /// What plugins set on their keys, by context.
    pub plugin_keys: HashMap<String, plugins::PluginKey>,
}

impl State {
    pub fn new(profiles: Vec<Profile>, settings: Settings) -> Self {
        Self {
            profiles,
            settings,
            devices: Vec::new(),
            pages: HashMap::new(),
            folders: HashMap::new(),
            pressed: HashSet::new(),
            notice: None,
            catalog: plugins::Catalog::default(),
            plugin_keys: HashMap::new(),
        }
    }

    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn profile_mut(&mut self, id: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    pub fn device(&self, handle: Handle) -> Option<&ConnectedDevice> {
        self.devices.iter().find(|d| d.handle == handle)
    }

    /// The profile assigned to a device serial, if it still exists.
    pub fn assigned(&self, serial: &str) -> Option<&Profile> {
        self.profile(self.settings.assignments.get(serial)?)
    }

    pub fn page(&self, profile: &Profile) -> usize {
        let last = profile.pages.len().saturating_sub(1);
        self.pages.get(&profile.id).copied().unwrap_or(0).min(last)
    }

    pub fn set_page(&mut self, profile_id: &str, page: usize) {
        if let Some(p) = self.profile(profile_id) {
            let page = page.min(p.pages.len().saturating_sub(1));
            self.pages.insert(profile_id.to_owned(), page);
        }
    }

    /// The folders open on a profile, outermost first. A folder that no
    /// longer exists closes, with the folders inside it.
    pub fn folder_path(&self, profile: &Profile) -> Vec<String> {
        let mut path = self.folders.get(&profile.id).cloned().unwrap_or_default();
        if let Some(gone) = path.iter().position(|id| profile.folder(id).is_none()) {
            path.truncate(gone);
        }
        path
    }

    pub fn set_folder_path(&mut self, profile_id: &str, mut path: Vec<String>) {
        path.truncate(MAX_FOLDER_DEPTH);
        if path.is_empty() {
            self.folders.remove(profile_id);
        } else {
            self.folders.insert(profile_id.to_owned(), path);
        }
    }

    /// The keys a profile shows now: its innermost open folder, else its
    /// current page.
    pub fn shown<'a>(&self, profile: &'a Profile) -> Option<&'a Page> {
        match self.folder_path(profile).last() {
            Some(id) => profile.folder(id),
            None => profile.pages.get(self.page(profile)),
        }
    }

    /// What keys of a profile show besides their configuration.
    pub fn live(&self, profile: &Profile) -> Live {
        Live {
            page: self.page(profile) + 1,
            pressed: false,
        }
    }

    /// Returns the first profile for this device model, creating a default
    /// one when there is none. The new profile is saved immediately.
    pub fn ensure_profile(&mut self, kind: DeviceKind) -> String {
        if let Some(p) = self.profiles.iter().find(|p| p.device == kind) {
            return p.id.clone();
        }
        self.add_profile("Default Profile", kind)
    }

    /// Creates and saves an empty profile; returns its id.
    pub fn add_profile(&mut self, name: &str, kind: DeviceKind) -> String {
        let profile = Profile::new(name, kind);
        if let Err(e) = store::save_profile(&profile) {
            self.notify(format!("Cannot save profile: {e}"));
        }
        let id = profile.id.clone();
        self.profiles.push(profile);
        id
    }

    /// Deletes a profile and its file. Connected devices that showed it
    /// switch to another profile for their model.
    pub fn delete_profile(&mut self, id: &str) {
        let kind = self.profile(id).map(|p| p.device);
        self.profiles.retain(|p| p.id != id);
        self.pages.remove(id);
        self.folders.remove(id);
        if let Err(e) = store::delete_profile(id) {
            self.notify(format!("Cannot delete profile: {e}"));
        }
        // A device whose default was deleted takes the next profile for its
        // model as the new default.
        let replacement = kind
            .and_then(|k| self.profiles.iter().find(|p| p.device == k))
            .map(|p| p.id.clone());
        self.settings.forget_profile(id, replacement.as_deref());
        self.assign_orphans();
        self.save_settings();
    }

    /// Takes in imported or restored profiles, replacing those with the same
    /// id. They are saved already.
    pub fn adopt_profiles(&mut self, profiles: Vec<Profile>) {
        for profile in profiles {
            self.profiles.retain(|p| p.id != profile.id);
            self.pages.remove(&profile.id);
            self.folders.remove(&profile.id);
            self.profiles.push(profile);
        }
        self.profiles.sort_by_key(|p| p.name.to_lowercase());
    }

    /// Replaces the settings with restored ones.
    pub fn adopt_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.assign_orphans();
        self.save_settings();
    }

    /// Gives connected devices without a valid profile one for their model.
    fn assign_orphans(&mut self) {
        let orphans: Vec<(String, DeviceKind)> = self
            .devices
            .iter()
            .filter(|d| self.assigned(&d.serial).is_none())
            .map(|d| (d.serial.clone(), d.model.kind))
            .collect();
        for (serial, kind) in orphans {
            let next = self.ensure_profile(kind);
            self.settings.assignments.insert(serial, next);
        }
    }

    /// The name a device is shown with: its own name or the model name.
    pub fn device_name(&self, device: &ConnectedDevice) -> String {
        let name = self.settings.device(&device.serial).name;
        if name.is_empty() {
            device.model.name.to_owned()
        } else {
            name
        }
    }

    /// The connected devices as the tray menu lists them.
    pub fn tray_devices(&self) -> Vec<tray::Device> {
        self.devices
            .iter()
            .map(|d| tray::Device {
                serial: d.serial.clone(),
                name: self.device_name(d),
                profiles: self
                    .profiles
                    .iter()
                    .filter(|p| p.device == d.model.kind)
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect(),
                active: self.assigned(&d.serial).map(|p| p.id.clone()),
            })
            .collect()
    }

    pub fn save_settings(&mut self) {
        if let Err(e) = store::save_settings(&self.settings) {
            self.notify(format!("Cannot save settings: {e}"));
        }
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    fn key(&self, serial: &str, key: u8) -> Option<KeyConfig> {
        let profile = self.assigned(serial)?;
        self.shown(profile)?.keys.get(&key).cloned()
    }

    /// The context of a key on what a device shows now.
    pub(crate) fn context(&self, serial: &str, key: u8) -> Option<String> {
        let page = self.shown(self.assigned(serial)?)?;
        Some(plugins::context(&page.id, key))
    }
}

pub fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub enum Msg {
    Device(Event),
    /// Profiles or pages changed; resend images that differ.
    Refresh,
    /// Only what plugins show changed; custom images stay cached.
    Redraw,
    /// A plugin (re)started and needs to hear about devices and its keys.
    PluginStarted(String),
    /// Device preferences or the global title style changed.
    Settings,
    Outcome(Handle, Result<Outcome, String>),
    /// A sequence pressed or released a plugin action.
    PluginStep {
        handle: Handle,
        step: Step,
        pressed: bool,
    },
    /// A sequence ended or was cancelled.
    RunDone {
        context: String,
        cancelled: bool,
    },
    /// Store a standby picture (panel-sized) on the device with this serial.
    Standby {
        serial: String,
        image: RgbaImage,
    },
    /// Return devices to their own idle screen before the app exits.
    Shutdown,
}

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

/// What the runtime knows about one connected device.
struct Link {
    serial: String,
    model: &'static DeviceModel,
    /// Hash of the image last sent to each key, by key index as the user
    /// sees it.
    sent: Vec<Option<u64>>,
    /// Preferences last applied to the device.
    enabled: bool,
    orientation: Orientation,
    asleep: bool,
    /// The keys show the screensaver (only while asleep).
    screensaver: bool,
    /// Last key press, for Sleep After.
    active_at: Instant,
    /// The plugin key shown on each key, as its plugin was told.
    shown: Vec<Option<Shown>>,
    /// Plugin actions inside the keys shown, by context.
    members: HashMap<String, Shown>,
}

/// A plugin key as its plugin last heard of it.
#[derive(Clone)]
struct Shown {
    plugin: String,
    instance: Instance,
    title: String,
    style: TitleStyle,
}

impl Link {
    fn new(info: &ConnectedDevice) -> Self {
        Self {
            serial: info.serial.clone(),
            model: info.model,
            sent: vec![None; usize::from(info.model.key_count())],
            enabled: true,
            orientation: Orientation::Standard,
            asleep: false,
            screensaver: false,
            active_at: Instant::now(),
            shown: vec![None; usize::from(info.model.key_count())],
            members: HashMap::new(),
        }
    }

    /// Ends sleep. Key images are sent again if the screensaver covered them.
    fn wake(&mut self) {
        self.asleep = false;
        if self.screensaver {
            self.screensaver = false;
            self.forget_images();
        }
    }

    fn forget_images(&mut self) {
        self.sent.fill(None);
    }
}

type Links = HashMap<Handle, Link>;

/// A Key Logic key between its press and what it turns out to be.
struct Pending {
    press: Press,
    handle: Handle,
    key: KeyConfig,
}

/// What Multi Action and Key Logic keys are doing, by context.
#[derive(Default)]
struct Groups {
    /// Running sequences and what cancels each.
    runs: HashMap<String, Sender<()>>,
    /// Pressed to cancel a run; the release must not start another.
    cancelling: HashSet<String>,
    presses: HashMap<String, Pending>,
}

pub fn start(state: Shared, repaint: Repaint, host: Option<Host>) -> Sender<Msg> {
    let (tx, rx) = channel::<Msg>();
    let device_tx = tx.clone();
    let action_tx = tx.clone();
    let step_tx = tx.clone();
    let run = move || {
        // DECKLY_NO_DEVICES=1 runs the editor without opening any device,
        // e.g. while the official app owns it.
        let devices = if std::env::var_os("DECKLY_NO_DEVICES").is_some() {
            DeviceManager::disabled()
        } else {
            DeviceManager::start(Arc::new(move |e| {
                let _ = device_tx.send(Msg::Device(e));
            }))
        };
        let actions = start_action_worker(action_tx);
        let mut renderer = Renderer::new();
        let mut links: Links = HashMap::new();
        let mut groups = Groups::default();
        // DECKLY_DEBUG=1 prints device events and action results to stderr.
        let debug = std::env::var_os("DECKLY_DEBUG").is_some();
        loop {
            let tools = Tools {
                state: &state,
                actions: &actions,
                host: host.as_ref(),
                links: &links,
                results: &step_tx,
            };
            if press_timers(&tools, &mut groups) {
                refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                repaint();
            }
            // Wait for the next message, until a device should sleep, until
            // a plugin's Alert/OK mark ends, or until a Key Logic press is
            // decided.
            let mark_end = lock(&state).next_mark_end();
            let wake = next_sleep(&state, &links)
                .into_iter()
                .chain(mark_end)
                .chain(groups.presses.values().filter_map(|p| p.press.deadline()))
                .min();
            let msg = match wake {
                Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(msg) => msg,
                    Err(RecvTimeoutError::Timeout) => {
                        fall_asleep(&state, &devices, &mut links);
                        refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                        repaint();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(msg) => msg,
                    Err(_) => break,
                },
            };
            if debug {
                match &msg {
                    Msg::Device(e) => eprintln!("deckly: {e:?}"),
                    Msg::Outcome(h, r) => eprintln!("deckly: device {h} action result {r:?}"),
                    _ => {}
                }
            }
            match msg {
                Msg::Device(Event::Connected(info)) => {
                    connect(&state, &info);
                    if let Some(host) = &host {
                        let s = lock(&state);
                        let device = plugins::device_info(&s, &info);
                        let all = s
                            .devices
                            .iter()
                            .map(|d| plugins::device_info(&s, d))
                            .collect();
                        drop(s);
                        host.set_devices(all);
                        host.broadcast(&protocol::device_did_connect(&device));
                    }
                    links.insert(info.handle, Link::new(&info));
                    // Deckly runs Sleep After itself; a timer left in the
                    // device by another app would put it to sleep behind
                    // Deckly's back.
                    devices.set_sleep_timer(info.handle, 0);
                    apply_settings(&state, &devices, &mut links);
                    refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                    repaint();
                }
                Msg::Device(Event::Disconnected(handle)) => {
                    let mut s = lock(&state);
                    s.devices.retain(|d| d.handle != handle);
                    s.pressed.retain(|&(h, _)| h != handle);
                    let all = s
                        .devices
                        .iter()
                        .map(|d| plugins::device_info(&s, d))
                        .collect();
                    drop(s);
                    groups.presses.retain(|_, p| p.handle != handle);
                    let link = links.remove(&handle);
                    if let (Some(host), Some(link)) = (&host, link) {
                        for shown in link.shown.iter().flatten().chain(link.members.values()) {
                            host.send(&shown.plugin, shown.instance.event("willDisappear"));
                        }
                        host.set_devices(all);
                        let id = protocol::device_id(&link.serial);
                        host.broadcast(&protocol::device_did_disconnect(&id));
                    }
                    repaint();
                }
                Msg::Device(Event::Key {
                    handle,
                    key,
                    pressed,
                }) => {
                    match key_event(&state, &devices, &mut links, handle, key, pressed) {
                        Some(hit) if !hit.config.groups.is_empty() => {
                            let tools = Tools {
                                state: &state,
                                actions: &actions,
                                host: host.as_ref(),
                                links: &links,
                                results: &step_tx,
                            };
                            group_key(&tools, &mut groups, handle, hit, pressed);
                        }
                        Some(hit) if hit.plugin => {
                            plugin_key(&state, host.as_ref(), &links, handle, &hit, pressed);
                        }
                        Some(hit) if pressed => {
                            let _ = actions.send((handle, hit.config));
                        }
                        _ => {}
                    }
                    // Shows the held key smaller (or normal again), and the
                    // keys a screensaver covered.
                    refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                    repaint();
                }
                Msg::Standby { serial, image } => {
                    store_standby(&state, &devices, &links, &serial, &image);
                }
                Msg::Refresh => {
                    renderer.clear_image_cache();
                    refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                }
                Msg::Redraw => {
                    refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                }
                Msg::PluginStarted(id) => {
                    if let Some(host) = &host {
                        plugin_started(&state, host, &links, &id);
                    }
                }
                Msg::Settings => {
                    apply_settings(&state, &devices, &mut links);
                    refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                }
                Msg::Outcome(handle, Ok(Outcome::Sleep)) => {
                    let awake = links.get(&handle).is_some_and(|l| l.enabled && !l.asleep);
                    if awake {
                        put_to_sleep(&state, &devices, &mut links, vec![handle]);
                    }
                    repaint();
                }
                Msg::Outcome(handle, result) => {
                    apply_outcome(&state, &devices, handle, result);
                    refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                    repaint();
                }
                Msg::PluginStep {
                    handle,
                    step,
                    pressed,
                } => plugin_step(&state, host.as_ref(), &links, handle, &step, pressed),
                Msg::RunDone { context, cancelled } => {
                    groups.runs.remove(&context);
                    lock(&state).finish_run(&context, cancelled);
                    refresh(&state, &devices, &mut renderer, &mut links, host.as_ref());
                    repaint();
                }
                Msg::Shutdown => {
                    for (&handle, link) in &links {
                        if link.enabled {
                            devices.reset(handle);
                        }
                    }
                }
            }
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("deckly-runtime".into())
        .spawn(run)
    {
        eprintln!("deckly: cannot start runtime thread: {e}");
    }
    tx
}

/// Records a new device and picks its profile: the device's default
/// profile if it has one, else the profile it showed last.
fn connect(state: &Shared, info: &ConnectedDevice) {
    let mut s = lock(state);
    let preferred = s
        .settings
        .device(&info.serial)
        .default_profile
        .filter(|id| s.profile(id).is_some());
    let current = s.assigned(&info.serial).map(|p| p.id.clone());
    let pick = match (preferred, &current) {
        (Some(id), _) => Some(id),
        (None, None) => Some(s.ensure_profile(info.model.kind)),
        (None, Some(_)) => None,
    };
    if let Some(id) = pick
        && current.as_ref() != Some(&id)
    {
        s.settings.assignments.insert(info.serial.clone(), id);
        s.save_settings();
    }
    s.devices.retain(|d| d.serial != info.serial);
    s.devices.push(info.clone());
}

/// Brings each device in line with its preferences: enabled, orientation
/// and brightness. A disabled device goes back to its own idle screen.
/// Changing preferences also wakes a sleeping device.
fn apply_settings(state: &Shared, devices: &DeviceManager, links: &mut Links) {
    let s = lock(state);
    for (&handle, link) in links.iter_mut() {
        let prefs = s.settings.device(&link.serial);
        if !prefs.enabled {
            if link.enabled {
                devices.reset(handle);
            }
            link.enabled = false;
            link.forget_images();
            continue;
        }
        if !link.enabled || link.orientation != prefs.orientation {
            link.forget_images();
        }
        link.enabled = true;
        link.orientation = prefs.orientation;
        link.wake();
        link.active_at = Instant::now();
        devices.set_brightness(handle, prefs.brightness);
    }
}

/// A key change that reaches an action.
struct KeyHit {
    config: KeyConfig,
    /// Key index as the user sees it.
    index: u8,
    /// A plugin action, which hears releases too.
    plugin: bool,
    context: String,
}

/// Handles a key change; returns the key whose action should hear of it.
fn key_event(
    state: &Shared,
    devices: &DeviceManager,
    links: &mut Links,
    handle: Handle,
    physical: u8,
    pressed: bool,
) -> Option<KeyHit> {
    let link = links.get_mut(&handle)?;
    let key = link.orientation.map_key(physical, link.model.key_count());
    let mut s = lock(state);
    let hit = |s: &State, config: KeyConfig| KeyHit {
        plugin: s.catalog.action(&config.action).is_some(),
        context: s.context(&link.serial, key).unwrap_or_default(),
        config,
        index: key,
    };
    // A release always counts, so no key stays held after the device was
    // disabled with a key down. Plugins hear the release of a press they
    // heard, and Multi Actions run on it.
    if !pressed {
        if !s.pressed.remove(&(handle, key)) {
            return None;
        }
        return s
            .key(&link.serial, key)
            .map(|c| hit(&s, c))
            .filter(|h| h.plugin || !h.config.groups.is_empty());
    }
    if !link.enabled {
        return None;
    }
    link.active_at = Instant::now();
    // The first press only wakes a sleeping device.
    if link.asleep {
        link.wake();
        devices.set_brightness(handle, s.settings.device(&link.serial).brightness);
        return None;
    }
    s.pressed.insert((handle, key));
    s.key(&link.serial, key).map(|c| hit(&s, c))
}

/// Sends a plugin key's press or release to its plugin. An action with
/// several states moves to the next one when released, after the plugin
/// heard the release.
fn plugin_key(
    state: &Shared,
    host: Option<&Host>,
    links: &Links,
    handle: Handle,
    hit: &KeyHit,
    pressed: bool,
) {
    let (Some(host), Some(link)) = (host, links.get(&handle)) else {
        return;
    };
    let mut s = lock(state);
    let Some(context) = s.context(&link.serial, hit.index) else {
        return;
    };
    let Some((plugin, states, automatic)) = s
        .catalog
        .action(&hit.config.action)
        .map(|(p, a)| (p.id.clone(), a.states.len(), a.automatic_states))
    else {
        return;
    };
    let mut instance = s.instance(&context, &hit.config);
    instance.device = protocol::device_id(&link.serial);
    host.send(
        &plugin,
        instance.event(if pressed { "keyDown" } else { "keyUp" }),
    );
    if !pressed && automatic && states > 1 {
        let key = s.plugin_key(&context, states);
        key.state = (key.state + 1) % states;
    }
}

/// Presses or releases a plugin action on behalf of its Multi Action or Key
/// Logic key.
fn plugin_step(
    state: &Shared,
    host: Option<&Host>,
    links: &Links,
    handle: Handle,
    step: &Step,
    pressed: bool,
) {
    let (Some(host), Some(link)) = (host, links.get(&handle)) else {
        return;
    };
    let mut s = lock(state);
    let Some((plugin, states, automatic)) = s
        .catalog
        .action(&step.key.action)
        .map(|(p, a)| (p.id.clone(), a.states.len(), a.automatic_states))
    else {
        return;
    };
    let mut instance = s.instance(&step.context, &step.key);
    instance.device = protocol::device_id(&link.serial);
    host.send(
        &plugin,
        instance.event(if pressed { "keyDown" } else { "keyUp" }),
    );
    if !pressed && automatic && states > 1 && !instance.in_multi_action {
        let key = s.plugin_key(&step.context, states);
        key.state = (key.state + 1) % states;
    }
}

/// What a Multi Action or Key Logic key needs from the runtime.
struct Tools<'a> {
    state: &'a Shared,
    actions: &'a Sender<(Handle, KeyConfig)>,
    host: Option<&'a Host>,
    links: &'a Links,
    results: &'a Sender<Msg>,
}

/// A press of a Multi Action key cancels its run; a release starts one. Key
/// Logic keys work out which press this was.
fn group_key(tools: &Tools, groups: &mut Groups, handle: Handle, hit: KeyHit, pressed: bool) {
    let context = hit.context.clone();
    if hit.config.action == actions::KEY_LOGIC {
        key_logic(tools, groups, handle, hit, pressed);
    } else if pressed {
        if let Some(run) = groups.runs.get(&context) {
            let _ = run.send(());
            groups.cancelling.insert(context);
        }
    } else if !groups.cancelling.remove(&context) && !groups.runs.contains_key(&context) {
        start_run(tools, groups, handle, &hit);
    }
}

fn start_run(tools: &Tools, groups: &mut Groups, handle: Handle, hit: &KeyHit) {
    let Some((list, members)) = sequence::chosen(&hit.config) else {
        lock(tools.state).mark_key(&hit.context, plugins::Mark::Alert);
        return;
    };
    let s = lock(tools.state);
    let steps = members
        .into_iter()
        .filter_map(|i| {
            let key = hit.config.groups.get(list)?.get(i)?.clone();
            Some(Step {
                plugin: s.catalog.action(&key.action).is_some(),
                context: format!("{}.{list}.{i}", hit.context),
                key,
            })
        })
        .collect();
    drop(s);
    if let Some(run) = sequence::start(steps, handle, hit.context.clone(), tools.results.clone()) {
        groups.runs.insert(hit.context.clone(), run);
    }
}

fn key_logic(tools: &Tools, groups: &mut Groups, handle: Handle, hit: KeyHit, pressed: bool) {
    let now = Instant::now();
    let pending = groups
        .presses
        .entry(hit.context.clone())
        .or_insert_with(|| Pending {
            press: Press::default(),
            handle,
            key: hit.config,
        });
    let slot = if pressed {
        pending.press.down(now)
    } else {
        let held = pending.press.take_down();
        if let Some(slot) = held {
            release_slot(tools, handle, &pending.key, &hit.context, slot);
        }
        pending.press.up(now)
    };
    if let Some(slot) = slot {
        let key = pending.key.clone();
        trigger(tools, groups, handle, &key, &hit.context, slot);
    }
    if groups
        .presses
        .get(&hit.context)
        .is_some_and(|p| p.press.idle())
    {
        groups.presses.remove(&hit.context);
    }
}

fn member_step(state: &Shared, key: &KeyConfig, context: &str, slot: usize) -> Option<Step> {
    let member = key.groups.get(slot)?.first()?.clone();
    Some(Step {
        plugin: lock(state).catalog.action(&member.action).is_some(),
        context: plugins::member_context_of(context, slot, 0),
        key: member,
    })
}

/// Runs a Key Logic slot. A single press taps its action; the others hold it
/// until the key comes up. An empty slot shows an alert.
fn trigger(
    tools: &Tools,
    groups: &mut Groups,
    handle: Handle,
    key: &KeyConfig,
    context: &str,
    slot: usize,
) {
    let Some(mut step) = member_step(tools.state, key, context, slot) else {
        lock(tools.state).mark_key(context, plugins::Mark::Alert);
        return;
    };
    if slot == sequence::SINGLE {
        step.key.delays = Some(deckly_core::ActionDelays {
            press: sequence::TAP,
            next: deckly_core::ActionDelays::MIN,
        });
        if let Some(run) = sequence::start(
            vec![step],
            handle,
            context.to_owned(),
            tools.results.clone(),
        ) {
            groups.runs.insert(context.to_owned(), run);
        }
    } else if step.plugin {
        plugin_step(tools.state, tools.host, tools.links, handle, &step, true);
    } else {
        let _ = tools.actions.send((handle, step.key));
    }
}

fn release_slot(tools: &Tools, handle: Handle, key: &KeyConfig, context: &str, slot: usize) {
    if let Some(step) = member_step(tools.state, key, context, slot)
        && step.plugin
    {
        plugin_step(tools.state, tools.host, tools.links, handle, &step, false);
    }
}

/// Decides Key Logic presses whose time ran out; returns whether any was.
fn press_timers(tools: &Tools, groups: &mut Groups) -> bool {
    let now = Instant::now();
    let due: Vec<(String, Handle, KeyConfig, usize)> = groups
        .presses
        .iter_mut()
        .filter_map(|(context, p)| {
            let slot = p.press.tick(now)?;
            Some((context.clone(), p.handle, p.key.clone(), slot))
        })
        .collect();
    groups.presses.retain(|_, p| !p.press.idle());
    let fired = !due.is_empty();
    for (context, handle, key, slot) in due {
        trigger(tools, groups, handle, &key, &context, slot);
    }
    fired
}

/// Tells a plugin that just registered about the devices and its keys.
fn plugin_started(state: &Shared, host: &Host, links: &Links, plugin: &str) {
    let s = lock(state);
    for d in &s.devices {
        host.send(
            plugin,
            protocol::device_did_connect(&plugins::device_info(&s, d)),
        );
    }
    drop(s);
    let keys = links.values().flat_map(|l| l.shown.iter().flatten());
    let members = links.values().flat_map(|l| l.members.values());
    for shown in keys.chain(members) {
        if shown.plugin == plugin {
            host.send(plugin, shown.instance.event("willAppear"));
        }
    }
}

/// Tells plugins when actions inside shown keys appear and disappear; they
/// expect that before a Multi Action presses them.
fn update_members(host: Option<&Host>, link: &mut Link, new: HashMap<String, Shown>) {
    if let Some(host) = host {
        for (context, old) in &link.members {
            if new.get(context).is_none_or(|n| n.plugin != old.plugin) {
                host.send(&old.plugin, old.instance.event("willDisappear"));
            }
        }
        for (context, n) in &new {
            if link
                .members
                .get(context)
                .is_none_or(|o| o.plugin != n.plugin)
            {
                host.send(&n.plugin, n.instance.event("willAppear"));
            }
        }
    }
    link.members = new;
}

/// Tells plugins when their keys appear, disappear or get a new title.
fn update_shown(host: Option<&Host>, slot: &mut Option<Shown>, new: Option<Shown>) {
    if let Some(host) = host {
        match (slot.as_ref(), new.as_ref()) {
            (Some(old), Some(n))
                if old.instance.context == n.instance.context
                    && old.instance.action == n.instance.action
                    && old.instance.device == n.instance.device =>
            {
                if old.title != n.title || old.style != n.style {
                    let align = match n.style.align {
                        deckly_core::TitleAlign::Top => "top",
                        deckly_core::TitleAlign::Middle => "middle",
                        deckly_core::TitleAlign::Bottom => "bottom",
                    };
                    let msg =
                        n.instance
                            .title_parameters(&n.title, n.style.show, n.style.size, align);
                    host.send(&n.plugin, msg);
                }
            }
            _ => {
                if let Some(old) = slot.as_ref() {
                    host.send(&old.plugin, old.instance.event("willDisappear"));
                }
                if let Some(n) = new.as_ref() {
                    host.send(&n.plugin, n.instance.event("willAppear"));
                }
            }
        }
    }
    *slot = new;
}

/// When the next awake device should fall asleep, if any should.
fn next_sleep(state: &Shared, links: &Links) -> Option<Instant> {
    let s = lock(state);
    links
        .values()
        .filter(|l| l.enabled && !l.asleep)
        .filter_map(|l| {
            let idle = sleep_after(&s.settings, &l.serial)?;
            Some(l.active_at + idle)
        })
        .min()
}

/// Puts devices idle for longer than Sleep After to sleep.
fn fall_asleep(state: &Shared, devices: &DeviceManager, links: &mut Links) {
    let idle: Vec<Handle> = {
        let s = lock(state);
        links
            .iter()
            .filter(|(_, link)| {
                sleep_after(&s.settings, &link.serial).is_some_and(|idle| {
                    link.enabled && !link.asleep && link.active_at.elapsed() >= idle
                })
            })
            .map(|(&handle, _)| handle)
            .collect()
    };
    put_to_sleep(state, devices, links, idle);
}

/// Puts devices to sleep: their screensaver is shown if they have one,
/// else the backlight goes off.
fn put_to_sleep(state: &Shared, devices: &DeviceManager, links: &mut Links, handles: Vec<Handle>) {
    let sleepers: Vec<(Handle, Option<std::path::PathBuf>)> = {
        let s = lock(state);
        handles
            .into_iter()
            .filter_map(|h| {
                let serial = &links.get(&h)?.serial;
                Some((h, s.settings.device(serial).screensaver))
            })
            .collect()
    };
    // Pictures are decoded without holding the lock.
    for (handle, screensaver) in sleepers {
        let Some(link) = links.get_mut(&handle) else {
            continue;
        };
        link.asleep = true;
        let keys = screensaver.map(|path| render::screensaver_keys(&path, link.model));
        match keys {
            Some(Ok(keys)) => {
                let count = link.model.key_count();
                for (index, img) in keys.into_iter().enumerate() {
                    // index < key_count, which is a u8.
                    let physical = link.orientation.map_key(index as u8, count);
                    let image = render::encode_key(img, link.model, link.orientation);
                    devices.set_image(handle, physical, image);
                }
                link.forget_images();
                link.screensaver = true;
            }
            other => {
                if let Some(Err(e)) = other {
                    lock(state).notify(format!("Cannot show screensaver: {e}"));
                }
                devices.set_brightness(handle, 0);
            }
        }
    }
}

/// Encodes a standby picture for the device and stores it there.
fn store_standby(
    state: &Shared,
    devices: &DeviceManager,
    links: &Links,
    serial: &str,
    image: &RgbaImage,
) {
    let Some((&handle, link)) = links.iter().find(|(_, l)| l.serial == serial) else {
        lock(state).notify("Standby image: the device is not connected");
        return;
    };
    match render::standby_jpeg(image, link.model) {
        Ok(jpeg) => {
            devices.set_standby_image(handle, jpeg);
            let mut s = lock(state);
            let name = s
                .devices
                .iter()
                .find(|d| d.handle == handle)
                .map(|d| s.device_name(d))
                .unwrap_or_else(|| link.model.name.to_owned());
            s.notify(format!("Standby image sent to {name}"));
        }
        Err(e) => lock(state).notify(format!("Standby image: {e}")),
    }
}

fn sleep_after(settings: &Settings, serial: &str) -> Option<Duration> {
    let minutes = settings.device(serial).sleep_minutes;
    (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
}

/// Runs actions off the runtime thread: opening a program can take a while
/// and must not delay key images or other key presses.
fn start_action_worker(results: Sender<Msg>) -> Sender<(Handle, KeyConfig)> {
    let (tx, rx) = channel::<(Handle, KeyConfig)>();
    let run = move || {
        while let Ok((handle, key)) = rx.recv() {
            let outcome = actions::run(&key);
            if results.send(Msg::Outcome(handle, outcome)).is_err() {
                return;
            }
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("deckly-actions".into())
        .spawn(run)
    {
        eprintln!("deckly: cannot start action thread: {e}");
    }
    tx
}

fn apply_outcome(
    state: &Shared,
    devices: &DeviceManager,
    handle: Handle,
    result: Result<Outcome, String>,
) {
    let mut s = lock(state);
    let Some(serial) = s.device(handle).map(|d| d.serial.clone()) else {
        return;
    };
    let Some((profile_id, page, pages, mut path)) = s
        .assigned(&serial)
        .map(|p| (p.id.clone(), s.page(p), p.pages.len(), s.folder_path(p)))
    else {
        return;
    };
    // Page actions leave any open folder for the page they go to.
    let go_to_page = |s: &mut State, n: usize| {
        s.set_folder_path(&profile_id, Vec::new());
        s.set_page(&profile_id, n);
    };
    match result {
        // Sleep is handled by the runtime loop, which owns the device links.
        Ok(Outcome::Done | Outcome::Sleep) => {}
        Ok(Outcome::NextPage) => go_to_page(&mut s, (page + 1) % pages.max(1)),
        Ok(Outcome::PreviousPage) => go_to_page(
            &mut s,
            page.checked_sub(1).unwrap_or(pages.saturating_sub(1)),
        ),
        Ok(Outcome::GoToPage(n)) => go_to_page(&mut s, n),
        Ok(Outcome::OpenFolder(id)) => {
            let exists = s
                .profile(&profile_id)
                .is_some_and(|p| p.folder(&id).is_some());
            if exists && path.len() < MAX_FOLDER_DEPTH {
                path.push(id);
                s.set_folder_path(&profile_id, path);
            } else {
                s.notify("Folder: this folder cannot be opened");
            }
        }
        Ok(Outcome::ParentFolder) => {
            path.pop();
            s.set_folder_path(&profile_id, path);
        }
        Ok(Outcome::SwitchProfile(id)) => {
            if s.profile(&id).is_some() {
                s.settings.assignments.insert(serial, id);
                s.save_settings();
            } else {
                s.notify("Switch Profile: that profile no longer exists");
            }
        }
        Ok(Outcome::Brightness(change)) => {
            let prefs = s.settings.device_mut(&serial);
            prefs.brightness = change.apply(prefs.brightness);
            let brightness = prefs.brightness;
            devices.set_brightness(handle, brightness);
            s.save_settings();
        }
        Err(e) => s.notify(e),
    }
}

/// A key's configuration and what it shows besides, as sent to a device.
struct KeyView {
    key: Option<KeyConfig>,
    live: Live,
    overlay: Option<Overlay>,
    shown: Option<Shown>,
}

/// The plugin actions inside a key.
fn member_views(
    s: &State,
    page: &str,
    index: u8,
    key: &KeyConfig,
    device: &str,
) -> Vec<(String, Shown)> {
    let mut views = Vec::new();
    for (l, list) in key.groups.iter().enumerate() {
        for (i, member) in list.iter().enumerate() {
            let Some((plugin, _)) = s.catalog.action(&member.action) else {
                continue;
            };
            let context = plugins::member_context(page, index, l, i);
            let mut instance = s.instance(&context, member);
            instance.device = device.to_owned();
            views.push((
                context,
                Shown {
                    plugin: plugin.id.clone(),
                    instance,
                    title: member.title.clone(),
                    style: TitleStyle::default(),
                },
            ));
        }
    }
    views
}

fn refresh(
    state: &Shared,
    devices: &DeviceManager,
    renderer: &mut Renderer,
    links: &mut Links,
    host: Option<&Host>,
) {
    // Collect the work under the lock, render without it.
    let mut work: Vec<(Handle, Vec<KeyView>, HashMap<String, Shown>)> = Vec::new();
    {
        let s = lock(state);
        renderer.title_defaults = s.settings.title_style;
        renderer.key_press = s.settings.key_press;
        for d in &s.devices {
            // A screensaver keeps its keys until the device wakes.
            if !links
                .get(&d.handle)
                .is_some_and(|l| l.enabled && !l.screensaver)
            {
                continue;
            }
            let profile = s.assigned(&d.serial);
            let page = profile
                .and_then(|p| s.shown(p))
                .cloned()
                .unwrap_or_default();
            let live = profile.map(|p| s.live(p)).unwrap_or_default();
            let device_id = protocol::device_id(&d.serial);
            let members = page
                .keys
                .iter()
                .flat_map(|(&k, key)| member_views(&s, &page.id, k, key, &device_id))
                .collect();
            let keys = (0..d.model.key_count())
                .map(|k| {
                    let pressed = s.pressed.contains(&(d.handle, k));
                    let key = page.keys.get(&k).cloned();
                    let context = plugins::context(&page.id, k);
                    let overlay = key.as_ref().and_then(|c| s.overlay(&context, c));
                    let shown = key.as_ref().filter(|_| overlay.is_some()).and_then(|c| {
                        let (plugin, _) = s.catalog.action(&c.action)?;
                        let mut instance = s.instance(&context, c);
                        instance.device = device_id.clone();
                        Some(Shown {
                            plugin: plugin.id.clone(),
                            instance,
                            title: c.title.clone(),
                            style: renderer.title_style(c),
                        })
                    });
                    KeyView {
                        key,
                        live: Live { pressed, ..live },
                        overlay,
                        shown,
                    }
                })
                .collect();
            work.push((d.handle, keys, members));
        }
    }
    for (handle, keys, members) in work {
        let Some(link) = links.get_mut(&handle) else {
            continue;
        };
        update_members(host, link, members);
        let count = link.model.key_count();
        for (index, view) in keys.into_iter().enumerate() {
            if let Some(slot) = link.shown.get_mut(index) {
                update_shown(host, slot, view.shown);
            }
            let (key, live, overlay) = (view.key, view.live, view.overlay);
            let hash = renderer.key_hash(key.as_ref(), live, overlay.as_ref());
            let Some(slot) = link.sent.get_mut(index) else {
                continue;
            };
            if *slot == Some(hash) {
                continue;
            }
            *slot = Some(hash);
            // index < key_count, which is a u8.
            let physical = link.orientation.map_key(index as u8, count);
            let image = renderer.device_image(
                key.as_ref(),
                live,
                overlay.as_ref(),
                link.model,
                link.orientation,
            );
            devices.set_image(handle, physical, image);
        }
    }
}

/// How long a notice stays visible in the editor.
pub const NOTICE_TIME: Duration = Duration::from_secs(6);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tray_lists_the_profiles_of_each_device() {
        let first = Profile::new("First", DeviceKind::Mk2);
        let second = Profile::new("Second", DeviceKind::Mk2);
        let other_model = Profile::new("Mini", DeviceKind::Mini);
        let (first_id, second_id) = (first.id.clone(), second.id.clone());
        let model = first.model();
        let mut state = State::new(vec![first, second, other_model], Settings::default());
        assert!(state.tray_devices().is_empty());
        state.devices.push(ConnectedDevice {
            handle: 1,
            model,
            serial: "S1".into(),
            firmware: None,
        });
        state
            .settings
            .assignments
            .insert("S1".into(), second_id.clone());
        let devices = state.tray_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].serial, "S1");
        assert_eq!(devices[0].name, model.name);
        assert_eq!(
            devices[0].profiles,
            vec![
                (first_id, "First".to_owned()),
                (second_id.clone(), "Second".to_owned())
            ]
        );
        assert_eq!(devices[0].active, Some(second_id));
    }

    struct Bench {
        state: Shared,
        actions: Sender<(Handle, KeyConfig)>,
        actions_rx: std::sync::mpsc::Receiver<(Handle, KeyConfig)>,
        results: Sender<Msg>,
        results_rx: std::sync::mpsc::Receiver<Msg>,
        links: Links,
        groups: Groups,
    }

    impl Bench {
        fn new() -> Self {
            let (actions, actions_rx) = channel();
            let (results, results_rx) = channel();
            Self {
                state: Arc::new(Mutex::new(State::new(Vec::new(), Settings::default()))),
                actions,
                actions_rx,
                results,
                results_rx,
                links: Links::new(),
                groups: Groups::default(),
            }
        }

        fn key(&mut self, config: &KeyConfig, pressed: bool) {
            let tools = Tools {
                state: &self.state,
                actions: &self.actions,
                host: None,
                links: &self.links,
                results: &self.results,
            };
            let hit = KeyHit {
                config: config.clone(),
                index: 0,
                plugin: false,
                context: "p.0".into(),
            };
            group_key(&tools, &mut self.groups, 1, hit, pressed);
        }
    }

    #[test]
    fn a_release_starts_a_run_and_a_press_cancels_it() {
        let mut bench = Bench::new();
        let mut key = KeyConfig::new(actions::MULTI_ACTION, json!({}));
        key.groups[0].push(KeyConfig::new(actions::DELAY, json!({"delay": 60_000})));
        bench.key(&key, true);
        assert!(bench.groups.runs.is_empty());
        bench.key(&key, false);
        assert!(bench.groups.runs.contains_key("p.0"));
        bench.key(&key, true);
        // The release of the cancelling press starts nothing.
        bench.key(&key, false);
        assert!(bench.groups.cancelling.is_empty());
        assert!(matches!(
            bench.results_rx.recv(),
            Ok(Msg::RunDone {
                cancelled: true,
                ..
            })
        ));
    }

    #[test]
    fn an_empty_multi_action_shows_an_alert() {
        let mut bench = Bench::new();
        let key = KeyConfig::new(actions::MULTI_ACTION, json!({}));
        bench.key(&key, true);
        bench.key(&key, false);
        assert!(bench.groups.runs.is_empty());
        let s = lock(&bench.state);
        assert_eq!(
            s.overlay("p.0", &key).and_then(|o| o.mark),
            Some(plugins::Mark::Alert)
        );
    }

    #[test]
    fn key_logic_runs_the_action_of_a_double_press() {
        let mut bench = Bench::new();
        let mut key = KeyConfig::new(actions::KEY_LOGIC, json!({}));
        key.groups[sequence::DOUBLE].push(KeyConfig::new(actions::NEXT_PAGE, json!({})));
        for pressed in [true, false, true] {
            bench.key(&key, pressed);
        }
        let (_, member) = bench.actions_rx.try_recv().unwrap();
        assert_eq!(member.action, actions::NEXT_PAGE);
        bench.key(&key, false);
        assert!(bench.groups.presses.is_empty());
        assert!(bench.groups.runs.is_empty());
    }

    #[test]
    fn open_folders_are_shown_and_close_when_deleted() {
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.pages.push(deckly_core::Page::new());
        profile.pages[1]
            .keys
            .insert(2, KeyConfig::new(actions::CREATE_FOLDER, json!({})));
        profile.normalize_folders();
        let folder = profile.pages[1].keys[&2]
            .folder()
            .unwrap_or_default()
            .to_owned();
        let id = profile.id.clone();
        let mut s = State::new(vec![profile], Settings::default());
        s.set_page(&id, 1);
        s.set_folder_path(&id, vec![folder.clone()]);
        let p = s.profile(&id).unwrap();
        assert_eq!(s.shown(p).map(|f| f.id.as_str()), Some(folder.as_str()));
        // The page indicator still counts the page the folder is on.
        assert_eq!(s.live(p).page, 2);
        // Deleting the folder key closes the folder.
        let p = s.profile_mut(&id).unwrap();
        p.pages[1].keys.clear();
        p.normalize_folders();
        let p = s.profile(&id).unwrap();
        assert!(s.folder_path(p).is_empty());
        assert_eq!(
            s.shown(p).map(|f| f.id.as_str()),
            Some(p.pages[1].id.as_str())
        );
    }
}

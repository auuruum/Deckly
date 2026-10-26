//! The device runtime: connects devices to profiles, keeps key images in
//! sync and runs actions. It lives on its own thread so keys keep working
//! while the window is hidden or busy.
//!
//! The editor and the runtime share one [`State`]. The editor changes it and
//! sends [`Msg::Refresh`] (or [`Msg::Settings`] after a preference changed);
//! the runtime re-renders only keys whose image changed.

use crate::actions::{self, Outcome};
use crate::render::Renderer;
use crate::store;
use deckly_core::{DeviceKind, DeviceModel, KeyConfig, Orientation, Profile, Settings};
use deckly_device::{ConnectedDevice, DeviceManager, Event, Handle};
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
    /// Keys held down right now, for the editor's live view.
    pub pressed: HashSet<(Handle, u8)>,
    pub notice: Option<(String, Instant)>,
}

impl State {
    pub fn new(profiles: Vec<Profile>, settings: Settings) -> Self {
        Self {
            profiles,
            settings,
            devices: Vec::new(),
            pages: HashMap::new(),
            pressed: HashSet::new(),
            notice: None,
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
        self.profiles.retain(|p| p.id != id);
        self.pages.remove(id);
        if let Err(e) = store::delete_profile(id) {
            self.notify(format!("Cannot delete profile: {e}"));
        }
        self.settings.forget_profile(id);
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
        self.save_settings();
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
        profile
            .pages
            .get(self.page(profile))?
            .keys
            .get(&key)
            .cloned()
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
    /// Device preferences or the global title style changed.
    Settings,
    Outcome(Handle, Result<Outcome, String>),
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
    /// Last key press, for Sleep After.
    active_at: Instant,
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
            active_at: Instant::now(),
        }
    }

    fn forget_images(&mut self) {
        self.sent.fill(None);
    }
}

type Links = HashMap<Handle, Link>;

pub fn start(state: Shared, repaint: Repaint) -> Sender<Msg> {
    let (tx, rx) = channel::<Msg>();
    let device_tx = tx.clone();
    let action_tx = tx.clone();
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
        // DECKLY_DEBUG=1 prints device events and action results to stderr.
        let debug = std::env::var_os("DECKLY_DEBUG").is_some();
        loop {
            // Wait for the next message, or until a device should sleep.
            let msg = match next_sleep(&state, &links) {
                Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(msg) => msg,
                    Err(RecvTimeoutError::Timeout) => {
                        fall_asleep(&state, &devices, &mut links);
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
                    links.insert(info.handle, Link::new(&info));
                    apply_settings(&state, &devices, &mut links);
                    refresh(&state, &devices, &mut renderer, &mut links);
                    repaint();
                }
                Msg::Device(Event::Disconnected(handle)) => {
                    let mut s = lock(&state);
                    s.devices.retain(|d| d.handle != handle);
                    s.pressed.retain(|&(h, _)| h != handle);
                    links.remove(&handle);
                    drop(s);
                    repaint();
                }
                Msg::Device(Event::Key {
                    handle,
                    key,
                    pressed,
                }) => {
                    let run = key_event(&state, &devices, &mut links, handle, key, pressed);
                    if let Some(config) = run {
                        let _ = actions.send((handle, config));
                    }
                    repaint();
                }
                Msg::Refresh => {
                    renderer.clear_image_cache();
                    refresh(&state, &devices, &mut renderer, &mut links);
                }
                Msg::Settings => {
                    apply_settings(&state, &devices, &mut links);
                    refresh(&state, &devices, &mut renderer, &mut links);
                }
                Msg::Outcome(handle, result) => {
                    apply_outcome(&state, &devices, handle, result);
                    refresh(&state, &devices, &mut renderer, &mut links);
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
        link.asleep = false;
        link.active_at = Instant::now();
        devices.set_brightness(handle, prefs.brightness);
    }
}

/// Handles a key change; returns the key whose action should run.
fn key_event(
    state: &Shared,
    devices: &DeviceManager,
    links: &mut Links,
    handle: Handle,
    physical: u8,
    pressed: bool,
) -> Option<KeyConfig> {
    let link = links.get_mut(&handle)?;
    if !link.enabled {
        return None;
    }
    let key = link.orientation.map_key(physical, link.model.key_count());
    let mut s = lock(state);
    if !pressed {
        s.pressed.remove(&(handle, key));
        return None;
    }
    link.active_at = Instant::now();
    // The first press only wakes a sleeping device.
    if link.asleep {
        link.asleep = false;
        devices.set_brightness(handle, s.settings.device(&link.serial).brightness);
        return None;
    }
    s.pressed.insert((handle, key));
    s.key(&link.serial, key)
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

/// Turns off the backlight of devices idle for longer than Sleep After.
fn fall_asleep(state: &Shared, devices: &DeviceManager, links: &mut Links) {
    let s = lock(state);
    for (&handle, link) in links.iter_mut() {
        let Some(idle) = sleep_after(&s.settings, &link.serial) else {
            continue;
        };
        if link.enabled && !link.asleep && link.active_at.elapsed() >= idle {
            link.asleep = true;
            devices.set_brightness(handle, 0);
        }
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
    let Some((profile_id, page, pages)) = s
        .assigned(&serial)
        .map(|p| (p.id.clone(), s.page(p), p.pages.len()))
    else {
        return;
    };
    match result {
        Ok(Outcome::Done) => {}
        Ok(Outcome::NextPage) => s.set_page(&profile_id, (page + 1) % pages.max(1)),
        Ok(Outcome::PreviousPage) => s.set_page(
            &profile_id,
            page.checked_sub(1).unwrap_or(pages.saturating_sub(1)),
        ),
        Ok(Outcome::GoToPage(n)) => s.set_page(&profile_id, n),
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

fn refresh(state: &Shared, devices: &DeviceManager, renderer: &mut Renderer, links: &mut Links) {
    // Collect the work under the lock, render without it.
    let mut work: Vec<(Handle, Vec<Option<KeyConfig>>)> = Vec::new();
    {
        let s = lock(state);
        renderer.title_defaults = s.settings.title_style;
        for d in &s.devices {
            if !links.get(&d.handle).is_some_and(|l| l.enabled) {
                continue;
            }
            let page = s
                .assigned(&d.serial)
                .and_then(|p| p.pages.get(s.page(p)))
                .cloned()
                .unwrap_or_default();
            let keys = (0..d.model.key_count())
                .map(|k| page.keys.get(&k).cloned())
                .collect();
            work.push((d.handle, keys));
        }
    }
    for (handle, keys) in work {
        let Some(link) = links.get_mut(&handle) else {
            continue;
        };
        let count = link.model.key_count();
        for (index, key) in keys.iter().enumerate() {
            let hash = renderer.key_hash(key.as_ref());
            let Some(slot) = link.sent.get_mut(index) else {
                continue;
            };
            if *slot == Some(hash) {
                continue;
            }
            *slot = Some(hash);
            // index < key_count, which is a u8.
            let physical = link.orientation.map_key(index as u8, count);
            let image = renderer.device_image(key.as_ref(), link.model, link.orientation);
            devices.set_image(handle, physical, image);
        }
    }
}

/// How long a notice stays visible in the editor.
pub const NOTICE_TIME: Duration = Duration::from_secs(6);

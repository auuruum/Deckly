//! The device runtime: connects devices to profiles, keeps key images in
//! sync and runs actions. It lives on its own thread so keys keep working
//! while the window is hidden or busy.
//!
//! The editor and the runtime share one [`State`]. The editor changes it and
//! sends [`Msg::Refresh`] (or [`Msg::Settings`] after a preference changed);
//! the runtime re-renders only keys whose image changed.

use crate::actions::{self, Outcome};
use crate::render::{self, Live, Renderer};
use crate::store;
use deckly_core::profile::MAX_FOLDER_DEPTH;
use deckly_core::{DeviceKind, DeviceModel, KeyConfig, Orientation, Page, Profile, Settings};
use deckly_device::{ConnectedDevice, DeviceManager, Event, Handle};
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
        self.shown(profile)?.keys.get(&key).cloned()
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
                    // Deckly runs Sleep After itself; a timer left in the
                    // device by another app would put it to sleep behind
                    // Deckly's back.
                    devices.set_sleep_timer(info.handle, 0);
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
                    let covered = links.get(&handle).is_some_and(|l| l.screensaver);
                    let run = key_event(&state, &devices, &mut links, handle, key, pressed);
                    if let Some(config) = run {
                        let _ = actions.send((handle, config));
                    }
                    if covered {
                        refresh(&state, &devices, &mut renderer, &mut links);
                    }
                    repaint();
                }
                Msg::Standby { serial, image } => {
                    store_standby(&state, &devices, &links, &serial, &image);
                }
                Msg::Refresh => {
                    renderer.clear_image_cache();
                    refresh(&state, &devices, &mut renderer, &mut links);
                }
                Msg::Settings => {
                    apply_settings(&state, &devices, &mut links);
                    refresh(&state, &devices, &mut renderer, &mut links);
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
        link.wake();
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
        link.wake();
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

fn refresh(state: &Shared, devices: &DeviceManager, renderer: &mut Renderer, links: &mut Links) {
    // Collect the work under the lock, render without it.
    let mut work: Vec<(Handle, Vec<Option<KeyConfig>>, Live)> = Vec::new();
    {
        let s = lock(state);
        renderer.title_defaults = s.settings.title_style;
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
            let keys = (0..d.model.key_count())
                .map(|k| page.keys.get(&k).cloned())
                .collect();
            work.push((d.handle, keys, live));
        }
    }
    for (handle, keys, live) in work {
        let Some(link) = links.get_mut(&handle) else {
            continue;
        };
        let count = link.model.key_count();
        for (index, key) in keys.iter().enumerate() {
            let hash = renderer.key_hash(key.as_ref(), live);
            let Some(slot) = link.sent.get_mut(index) else {
                continue;
            };
            if *slot == Some(hash) {
                continue;
            }
            *slot = Some(hash);
            // index < key_count, which is a u8.
            let physical = link.orientation.map_key(index as u8, count);
            let image = renderer.device_image(key.as_ref(), live, link.model, link.orientation);
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

//! The device runtime: connects devices to profiles, keeps key images in
//! sync and runs actions. It lives on its own thread so keys keep working
//! while the window is hidden or busy.
//!
//! The editor and the runtime share one [`State`]. The editor changes it and
//! sends [`Msg::Refresh`]; the runtime re-renders only keys whose image
//! changed.

use crate::actions::{self, Outcome};
use crate::render::{self, Renderer};
use crate::store::{self, Settings};
use deckly_core::{DeviceKind, KeyConfig, Profile};
use deckly_device::{ConnectedDevice, DeviceManager, Event, Handle};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, channel};
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
        let profile = Profile::new("Default Profile", kind);
        if let Err(e) = store::save_profile(&profile) {
            self.notify(format!("Cannot save profile: {e}"));
        }
        let id = profile.id.clone();
        self.profiles.push(profile);
        id
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
    /// The brightness setting changed.
    Brightness,
    Outcome(Handle, Result<Outcome, String>),
    /// Return devices to their own idle screen before the app exits.
    Shutdown,
}

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

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
        let mut sent: HashMap<Handle, Vec<Option<u64>>> = HashMap::new();
        // DECKLY_DEBUG=1 prints device events and action results to stderr.
        let debug = std::env::var_os("DECKLY_DEBUG").is_some();
        while let Ok(msg) = rx.recv() {
            if debug {
                match &msg {
                    Msg::Device(e) => eprintln!("deckly: {e:?}"),
                    Msg::Outcome(h, r) => eprintln!("deckly: device {h} action result {r:?}"),
                    _ => {}
                }
            }
            match msg {
                Msg::Device(Event::Connected(info)) => {
                    {
                        let mut s = lock(&state);
                        if s.assigned(&info.serial).is_none() {
                            let id = s.ensure_profile(info.model.kind);
                            s.settings.assignments.insert(info.serial.clone(), id);
                            save_settings(&mut s);
                        }
                        devices.set_brightness(info.handle, s.settings.brightness);
                        s.devices.retain(|d| d.serial != info.serial);
                        s.devices.push(info.clone());
                    }
                    sent.insert(info.handle, vec![None; usize::from(info.model.key_count())]);
                    refresh(&state, &devices, &mut renderer, &mut sent);
                    repaint();
                }
                Msg::Device(Event::Disconnected(handle)) => {
                    let mut s = lock(&state);
                    s.devices.retain(|d| d.handle != handle);
                    s.pressed.retain(|&(h, _)| h != handle);
                    sent.remove(&handle);
                    drop(s);
                    repaint();
                }
                Msg::Device(Event::Key {
                    handle,
                    key,
                    pressed,
                }) => {
                    let config = {
                        let mut s = lock(&state);
                        if pressed {
                            s.pressed.insert((handle, key));
                        } else {
                            s.pressed.remove(&(handle, key));
                        }
                        let serial = s.device(handle).map(|d| d.serial.clone());
                        serial.and_then(|serial| s.key(&serial, key))
                    };
                    if pressed && let Some(config) = config {
                        let _ = actions.send((handle, config));
                    }
                    repaint();
                }
                Msg::Refresh => {
                    renderer.clear_image_cache();
                    refresh(&state, &devices, &mut renderer, &mut sent);
                }
                Msg::Brightness => {
                    let s = lock(&state);
                    for d in &s.devices {
                        devices.set_brightness(d.handle, s.settings.brightness);
                    }
                }
                Msg::Outcome(handle, result) => {
                    apply_outcome(&state, &devices, handle, result);
                    refresh(&state, &devices, &mut renderer, &mut sent);
                    repaint();
                }
                Msg::Shutdown => {
                    for d in &lock(&state).devices {
                        devices.reset(d.handle);
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
                save_settings(&mut s);
            } else {
                s.notify("Switch Profile: that profile no longer exists");
            }
        }
        Ok(Outcome::Brightness(change)) => {
            s.settings.brightness = change.apply(s.settings.brightness);
            devices.set_brightness(handle, s.settings.brightness);
            save_settings(&mut s);
        }
        Err(e) => s.notify(e),
    }
}

fn save_settings(s: &mut State) {
    if let Err(e) = store::save_settings(&s.settings) {
        s.notify(format!("Cannot save settings: {e}"));
    }
}

fn refresh(
    state: &Shared,
    devices: &DeviceManager,
    renderer: &mut Renderer,
    sent: &mut HashMap<Handle, Vec<Option<u64>>>,
) {
    // Collect the work under the lock, render without it.
    let mut work: Vec<(Handle, u32, Vec<Option<KeyConfig>>)> = Vec::new();
    {
        let s = lock(state);
        for d in &s.devices {
            let page = s
                .assigned(&d.serial)
                .and_then(|p| p.pages.get(s.page(p)))
                .cloned()
                .unwrap_or_default();
            let keys = (0..d.model.key_count())
                .map(|k| page.keys.get(&k).cloned())
                .collect();
            work.push((d.handle, d.model.key_image_size, keys));
        }
    }
    for (handle, size, keys) in work {
        let Some(last) = sent.get_mut(&handle) else {
            continue;
        };
        for (index, key) in keys.iter().enumerate() {
            let hash = render::key_hash(key.as_ref());
            let Some(slot) = last.get_mut(index) else {
                continue;
            };
            if *slot == Some(hash) {
                continue;
            }
            *slot = Some(hash);
            // index < key_count, which is a u8.
            devices.set_image(
                handle,
                index as u8,
                renderer.device_jpeg(key.as_ref(), size),
            );
        }
    }
}

/// How long a notice stays visible in the editor.
pub const NOTICE_TIME: Duration = Duration::from_secs(6);

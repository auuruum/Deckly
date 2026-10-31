//! Plugins inside the app: the catalog of installed plugin actions, what
//! plugins draw on their keys, and the thread that carries out what plugins
//! and their property inspectors ask for.
//!
//! The protocol and the plugin processes live in `deckly-plugin-host`; this
//! module ties them to profiles, keys and rendering. A key placed with a
//! plugin action is identified to the plugin by a context made of its page id
//! and key index.

use crate::actions;
use crate::os;
use crate::render;
use crate::runtime::{self, Msg, Repaint, Shared, State};
use crate::store;
use deckly_core::{DeviceKind, KeyConfig, Page};
use deckly_device::ConnectedDevice;
use deckly_plugin_host::manifest::{image_file, safe_join};
use deckly_plugin_host::{Command, Host, HostEvent, Instance, Status, protocol};
use image::RgbaImage;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Plugin images are decoded at the largest key size used today.
const IMAGE_SIZE: u32 = 144;
const MAX_IMAGE_DATA: usize = 4 * 1024 * 1024;
const MAX_GLOBAL_SETTINGS_BYTES: usize = 1024 * 1024;
const MARK_TIME: Duration = Duration::from_millis(1500);

/// A decoded image with an id that changes with its pixels.
pub struct KeyImage {
    pub id: u64,
    pub img: RgbaImage,
}

pub type Image = Arc<KeyImage>;

fn key_image(img: RgbaImage) -> Image {
    let mut h = DefaultHasher::new();
    img.as_raw().hash(&mut h);
    Arc::new(KeyImage {
        id: h.finish(),
        img,
    })
}

/// The short feedback a plugin can show on a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    Alert,
    Ok,
}

/// What a plugin key shows besides its configuration.
#[derive(Clone, Default)]
pub struct Overlay {
    pub image: Option<Image>,
    /// Shown when the user gave the key no title of their own.
    pub title: Option<String>,
    pub mark: Option<Mark>,
}

impl Hash for Overlay {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.image.as_ref().map(|i| i.id).hash(h);
        self.title.hash(h);
        self.mark.hash(h);
    }
}

#[derive(Clone)]
pub struct StateDef {
    pub image: Option<Image>,
    pub title: String,
}

#[derive(Clone)]
pub struct CatalogAction {
    pub uuid: String,
    pub name: String,
    pub tooltip: String,
    pub icon: Option<Image>,
    pub states: Vec<StateDef>,
    pub automatic_states: bool,
    pub has_inspector: bool,
    pub visible: bool,
    pub in_multi_actions: bool,
    pub in_key_logic: bool,
}

#[derive(Clone)]
pub struct CatalogPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub category: String,
    pub category_icon: Option<Image>,
    pub icon: Option<Image>,
    pub actions: Vec<CatalogAction>,
    pub status: Status,
    pub dir: PathBuf,
    /// The plugin refuses to work outside the official app.
    pub official_only: bool,
    /// Identifies the manifest the images were decoded from.
    source: usize,
}

#[derive(Default)]
pub struct Catalog {
    pub plugins: Vec<Arc<CatalogPlugin>>,
}

impl Catalog {
    pub fn action(&self, uuid: &str) -> Option<(&CatalogPlugin, &CatalogAction)> {
        self.plugins.iter().find_map(|p| {
            let a = p.actions.iter().find(|a| a.uuid == uuid)?;
            Some((&**p, a))
        })
    }
}

/// What plugins changed on one placed key.
#[derive(Default)]
pub struct PluginKey {
    pub state: usize,
    pub titles: Vec<Option<String>>,
    pub images: Vec<Option<Image>>,
    pub mark: Option<(Mark, Instant)>,
}

/// The id a plugin knows a placed key by.
pub fn context(page: &str, key: u8) -> String {
    format!("{page}.{key}")
}

/// The id a plugin knows an action inside a group key by.
pub fn member_context(page: &str, key: u8, list: usize, member: usize) -> String {
    member_context_of(&context(page, key), list, member)
}

/// The id of an action inside the group key with this context.
pub fn member_context_of(key_context: &str, list: usize, member: usize) -> String {
    format!("{key_context}.{list}.{member}")
}

/// Where a context points: a page, a key, and for an action inside a group
/// key its list and place in that list.
type Place<'a> = (&'a str, u8, Option<(usize, usize)>);

fn parse_context(context: &str) -> Option<Place<'_>> {
    let mut parts = context.rsplitn(4, '.');
    if let (Some(member), Some(list), Some(key), Some(page)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
        && let (Ok(member), Ok(list), Ok(key)) = (member.parse(), list.parse(), key.parse())
    {
        return Some((page, key, Some((list, member))));
    }
    let (page, key) = context.rsplit_once('.')?;
    Some((page, key.parse().ok()?, None))
}

/// The action a context points to on a key: the key or one of its members.
fn at_place(key: &KeyConfig, member: Option<(usize, usize)>) -> Option<&KeyConfig> {
    match member {
        Some((list, i)) => key.groups.get(list)?.get(i),
        None => Some(key),
    }
}

/// The SDK's device type for a model (docs/clean-room/2026-10-28-plugin-host.md).
pub fn sdk_device_type(kind: DeviceKind) -> u8 {
    use DeviceKind as K;
    match kind {
        K::Mini | K::MiniV2 | K::MiniDiscord | K::Module6 => 1,
        K::Xl | K::XlV2 | K::Module32 => 2,
        K::Plus => 7,
        K::Neo => 9,
        K::PlusXl => 13,
        K::OriginalV2 | K::Mk2 | K::Mk2Scissor | K::Module15 => 0,
    }
}

pub fn device_info(state: &State, d: &ConnectedDevice) -> protocol::DeviceInfo {
    protocol::DeviceInfo {
        id: protocol::device_id(&d.serial),
        name: state.device_name(d),
        columns: d.model.columns,
        rows: d.model.rows,
        kind: sdk_device_type(d.model.kind),
    }
}

impl State {
    /// What a plugin key shows from its plugin; `None` for other keys.
    pub fn overlay(&self, context: &str, key: &KeyConfig) -> Option<Overlay> {
        let live = self.plugin_keys.get(context);
        let mark = live
            .and_then(|l| l.mark)
            .filter(|(_, until)| *until > Instant::now())
            .map(|(m, _)| m);
        // Multi Action keys show how their run ended.
        if !key.groups.is_empty() {
            return mark.map(|mark| Overlay {
                mark: Some(mark),
                ..Overlay::default()
            });
        }
        let (_, action) = self.catalog.action(&key.action)?;
        let state = live
            .map_or(0, |l| l.state)
            .min(action.states.len().saturating_sub(1));
        let def = action.states.get(state);
        let image = live
            .and_then(|l| l.images.get(state).cloned().flatten())
            .or_else(|| def.and_then(|d| d.image.clone()))
            .or_else(|| action.icon.clone());
        let title = live
            .and_then(|l| l.titles.get(state).cloned().flatten())
            .or_else(|| def.map(|d| d.title.clone()).filter(|t| !t.is_empty()));
        Some(Overlay { image, title, mark })
    }

    /// Shows a short mark on a key.
    pub fn mark_key(&mut self, context: &str, mark: Mark) {
        self.plugin_key(context, 1).mark = Some((mark, Instant::now() + MARK_TIME));
    }

    /// Ends a Multi Action run on the key at `context`: a finished run shows
    /// OK and moves a switch to its other side, and a cancelled one does
    /// neither.
    pub fn finish_run(&mut self, context: &str, cancelled: bool) {
        let Some(key) = self
            .key_at(context)
            .filter(|k| k.action != actions::KEY_LOGIC)
        else {
            return;
        };
        if cancelled {
            return;
        }
        self.mark_key(context, Mark::Ok);
        if key.action != actions::MULTI_ACTION_SWITCH {
            return;
        }
        let Some((page_id, index, None)) = parse_context(context) else {
            return;
        };
        let Some((profile_id, page)) = self.page_mut(page_id) else {
            return;
        };
        if let Some(key) = page.keys.get_mut(&index) {
            let side = actions::switch_side(key);
            actions::set_switch_side(key, 1 - side);
        }
        if let Some(Err(e)) = self.profile(&profile_id).map(store::save_profile) {
            self.notify(format!("Cannot save profile: {e}"));
        }
    }

    /// When the next Alert/OK mark should disappear.
    pub fn next_mark_end(&self) -> Option<Instant> {
        let now = Instant::now();
        self.plugin_keys
            .values()
            .filter_map(|k| k.mark.map(|(_, until)| until))
            .filter(|until| *until > now)
            .min()
    }

    /// The page (or folder) with this id and the profile it is in.
    fn page_mut(&mut self, page_id: &str) -> Option<(String, &mut Page)> {
        self.profiles.iter_mut().find_map(|p| {
            let id = p.id.clone();
            p.pages
                .iter_mut()
                .chain(p.folders.iter_mut())
                .find(|page| page.id == page_id)
                .map(|page| (id, page))
        })
    }

    fn key_at(&self, context: &str) -> Option<KeyConfig> {
        let (page_id, key, member) = parse_context(context)?;
        self.profiles.iter().find_map(|p| {
            let page = p
                .pages
                .iter()
                .chain(&p.folders)
                .find(|page| page.id == page_id)?;
            at_place(page.keys.get(&key)?, member).cloned()
        })
    }

    /// The key at `context` when it holds one of `plugin`'s actions; a
    /// plugin may only touch its own keys.
    fn own_key(&self, plugin: &str, context: &str) -> Option<KeyConfig> {
        let key = self.key_at(context)?;
        let (owner, _) = self.catalog.action(&key.action)?;
        (owner.id == plugin).then_some(key)
    }

    /// Describes a placed key to its plugin.
    pub fn instance(&self, context: &str, key: &KeyConfig) -> Instance {
        let (page_id, index, member) = parse_context(context).unwrap_or(("", 0, None));
        // The device showing the key, else the first of its model.
        let profile = self
            .profiles
            .iter()
            .find(|p| p.pages.iter().chain(&p.folders).any(|pg| pg.id == page_id));
        let columns = profile.map_or(5, |p| p.model().columns).max(1);
        let device = profile
            .and_then(|p| {
                self.devices
                    .iter()
                    .find(|d| self.assigned(&d.serial).is_some_and(|a| a.id == p.id))
            })
            .map(|d| protocol::device_id(&d.serial))
            .unwrap_or_default();
        let states = self
            .catalog
            .action(&key.action)
            .map_or(0, |(_, a)| a.states.len());
        // Key Logic actions stay on their key; Multi Action ones have no place.
        let in_multi_action = member.is_some()
            && profile
                .and_then(|p| p.pages.iter().chain(&p.folders).find(|pg| pg.id == page_id))
                .and_then(|pg| pg.keys.get(&index))
                .is_none_or(|k| k.action != actions::KEY_LOGIC);
        let desired = usize::from(key.desired_state.unwrap_or(0)).min(states.saturating_sub(1));
        let state = if in_multi_action {
            desired
        } else {
            self.plugin_keys.get(context).map_or(0, |k| k.state)
        };
        Instance {
            action: key.action.clone(),
            context: context.to_owned(),
            device,
            settings: key.settings.clone(),
            column: index % columns,
            row: index / columns,
            state: (states > 1).then_some(state),
            in_multi_action,
            desired_state: (in_multi_action && states > 1).then_some(desired),
        }
    }

    pub fn plugin_key(&mut self, context: &str, states: usize) -> &mut PluginKey {
        let k = self.plugin_keys.entry(context.to_owned()).or_default();
        let n = states.max(1);
        k.titles.resize(n, None);
        k.images.resize(n, None);
        k
    }

    fn states_of(&self, key: &KeyConfig) -> usize {
        self.catalog
            .action(&key.action)
            .map_or(1, |(_, a)| a.states.len().max(1))
    }
}

/// Starts the thread that carries out plugin requests.
pub fn start(
    state: Shared,
    runtime: Sender<Msg>,
    host: Host,
    events: Receiver<HostEvent>,
    repaint: Repaint,
) {
    let run = move || {
        let mut worker = Worker {
            state,
            runtime,
            host,
            repaint,
        };
        worker.rebuild_catalog();
        while let Ok(event) = events.recv() {
            worker.handle(event);
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("deckly-plugin-events".into())
        .spawn(run)
    {
        eprintln!("deckly: cannot start plugin thread: {e}");
    }
}

struct Worker {
    state: Shared,
    runtime: Sender<Msg>,
    host: Host,
    repaint: Repaint,
}

impl Worker {
    fn redraw(&self) {
        let _ = self.runtime.send(Msg::Redraw);
        (self.repaint)();
    }

    fn handle(&mut self, event: HostEvent) {
        match event {
            HostEvent::Changed => self.rebuild_catalog(),
            HostEvent::Started(id) => {
                let _ = self.runtime.send(Msg::PluginStarted(id));
            }
            HostEvent::Command { plugin, command } => self.command(&plugin, command),
            HostEvent::InspectorCommand {
                plugin,
                instance,
                command,
            } => self.inspector_command(&plugin, &instance, command),
        }
    }

    /// Re-reads the plugin list. Images are decoded again only for plugins
    /// whose manifest changed.
    fn rebuild_catalog(&mut self) {
        let old: HashMap<String, Arc<CatalogPlugin>> = runtime::lock(&self.state)
            .catalog
            .plugins
            .iter()
            .map(|p| (p.id.clone(), p.clone()))
            .collect();
        let mut plugins = Vec::new();
        for p in self.host.plugins() {
            let Some(m) = &p.manifest else {
                plugins.push(Arc::new(CatalogPlugin {
                    id: p.id.clone(),
                    name: p.id.clone(),
                    version: String::new(),
                    author: String::new(),
                    category: p.id.clone(),
                    category_icon: None,
                    icon: None,
                    actions: Vec::new(),
                    status: p.status.clone(),
                    dir: p.dir.clone(),
                    official_only: false,
                    source: 0,
                }));
                continue;
            };
            if m.offers_nothing() {
                continue;
            }
            let source = Arc::as_ptr(m) as usize;
            if let Some(prev) = old.get(&p.id).filter(|o| o.source == source) {
                let mut next = (**prev).clone();
                next.status = p.status.clone();
                plugins.push(Arc::new(next));
                continue;
            }
            let image = |rel: &str| load_file_image(&p.dir, rel).map(key_image);
            let actions = m
                .actions
                .iter()
                .filter(|a| a.on_keys())
                .map(|a| CatalogAction {
                    uuid: a.uuid.clone(),
                    name: a.name.clone(),
                    tooltip: a.tooltip.clone(),
                    icon: image(&a.icon),
                    states: a
                        .states
                        .iter()
                        .map(|s| StateDef {
                            image: image(&s.image),
                            title: if s.show_title {
                                s.title.clone()
                            } else {
                                String::new()
                            },
                        })
                        .collect(),
                    automatic_states: a.automatic_states,
                    has_inspector: !a.property_inspector.is_empty()
                        || !m.property_inspector.is_empty(),
                    visible: a.visible,
                    in_multi_actions: a.in_multi_actions,
                    in_key_logic: a.in_key_logic,
                })
                .collect();
            plugins.push(Arc::new(CatalogPlugin {
                id: p.id.clone(),
                name: m.name.clone(),
                version: m.version.clone(),
                author: m.author.clone(),
                category: m.category_name().to_owned(),
                category_icon: image(&m.category_icon),
                icon: image(&m.icon),
                actions,
                status: p.status.clone(),
                dir: p.dir.clone(),
                official_only: deckly_plugin_host::manifest::needs_official_app(&p.dir),
                source,
            }));
        }
        runtime::lock(&self.state).catalog = Catalog { plugins };
        self.redraw();
    }

    fn command(&mut self, plugin: &str, command: Command) {
        match command {
            Command::SetTitle {
                context,
                title,
                state,
            } => {
                let mut s = runtime::lock(&self.state);
                let Some(key) = s.own_key(plugin, &context) else {
                    return;
                };
                let n = s.states_of(&key);
                let k = s.plugin_key(&context, n);
                let title = title.filter(|t| !t.is_empty());
                set_for_states(&mut k.titles, state, title);
                drop(s);
                self.redraw();
            }
            Command::SetImage {
                context,
                image,
                state,
            } => {
                let Some(dir) = self.plugin_dir(plugin) else {
                    return;
                };
                if runtime::lock(&self.state)
                    .own_key(plugin, &context)
                    .is_none()
                {
                    return;
                }
                // Decoded without holding the lock.
                let image = match image {
                    Some(data) => match decode_plugin_image(&data, &dir) {
                        Ok(img) => Some(key_image(img)),
                        Err(e) => {
                            self.log(plugin, &format!("setImage: {e}"));
                            return;
                        }
                    },
                    None => None,
                };
                let mut s = runtime::lock(&self.state);
                let Some(key) = s.own_key(plugin, &context) else {
                    return;
                };
                let n = s.states_of(&key);
                let k = s.plugin_key(&context, n);
                set_for_states(&mut k.images, state, image);
                drop(s);
                self.redraw();
            }
            Command::SetState { context, state } => {
                let mut s = runtime::lock(&self.state);
                let Some(key) = s.own_key(plugin, &context) else {
                    return;
                };
                let n = s.states_of(&key);
                s.plugin_key(&context, n).state = state.min(n - 1);
                drop(s);
                self.redraw();
            }
            Command::ShowAlert { context } => self.mark(plugin, &context, Mark::Alert),
            Command::ShowOk { context } => self.mark(plugin, &context, Mark::Ok),
            Command::SetSettings { context, settings } => {
                if let Some(instance) = self.store_settings(plugin, &context, settings) {
                    self.host
                        .send_to_inspector(&context, instance.event("didReceiveSettings"));
                }
            }
            Command::GetSettings { context } => {
                let s = runtime::lock(&self.state);
                if let Some(key) = s.own_key(plugin, &context) {
                    let msg = s.instance(&context, &key).event("didReceiveSettings");
                    drop(s);
                    self.host.send(plugin, msg);
                }
            }
            Command::SetGlobalSettings { settings } => {
                self.save_global(plugin, &settings);
                if self
                    .host
                    .inspector()
                    .is_some_and(|i| self.owns_action(plugin, &i.action))
                {
                    let msg = protocol::did_receive_global_settings(&settings);
                    if let Some(i) = self.host.inspector() {
                        self.host.send_to_inspector(&i.context, msg);
                    }
                }
            }
            Command::GetGlobalSettings => {
                let msg = protocol::did_receive_global_settings(&load_global(plugin));
                self.host.send(plugin, msg);
            }
            Command::OpenUrl { url } => self.open_url(plugin, &url),
            Command::SwitchToProfile {
                device,
                profile,
                page,
            } => {
                if let Err(e) = self.switch_to_profile(plugin, &device, profile, page) {
                    self.log(plugin, &format!("switchToProfile: {e}"));
                }
            }
            Command::LogMessage { .. }
            | Command::SendToPropertyInspector { .. }
            | Command::SendToPlugin { .. } => {}
            Command::Other(event) => self.log(plugin, &format!("{event} is not supported yet")),
        }
    }

    /// Shows one of the plugin's own profiles on a device, bringing it in on
    /// first use; no name goes back to the profile shown before.
    fn switch_to_profile(
        &self,
        plugin: &str,
        device: &str,
        name: Option<String>,
        page: Option<usize>,
    ) -> Result<(), String> {
        let (serial, kind) = runtime::lock(&self.state)
            .devices
            .iter()
            .find(|d| protocol::device_id(&d.serial) == device)
            .map(|d| (d.serial.clone(), d.model.kind))
            .ok_or("unknown device")?;
        let id = match name {
            Some(name) => {
                let dir = self.plugin_dir(plugin).ok_or("unknown plugin")?;
                // Only profiles the manifest lists, so a plugin cannot point
                // Deckly at other files.
                let listed = self.host.plugins().iter().any(|p| {
                    p.id == plugin
                        && p.manifest
                            .as_ref()
                            .is_some_and(|m| m.profiles.contains(&name))
                });
                if !listed {
                    return Err(format!("{name} is not a profile of this plugin"));
                }
                let file = deckly_plugin_host::manifest::find_file(
                    &dir,
                    &format!("{name}.streamDeckProfile"),
                )
                .ok_or(format!("the file of profile {name} is missing"))?;
                crate::transfer::import_bundled(&file, &self.state)?
            }
            None => runtime::lock(&self.state)
                .previous
                .get(&serial)
                .cloned()
                .ok_or("there is no previous profile")?,
        };
        let mut s = runtime::lock(&self.state);
        let profile = s.profile(&id).ok_or("that profile no longer exists")?;
        if profile.device != kind {
            return Err("that profile is for another device model".into());
        }
        s.assign(&serial, id.clone());
        s.set_folder_path(&id, Vec::new());
        if let Some(page) = page {
            s.set_page(&id, page);
        }
        drop(s);
        let _ = self.runtime.send(Msg::Refresh);
        (self.repaint)();
        Ok(())
    }

    fn inspector_command(&mut self, plugin: &str, instance: &Instance, command: Command) {
        let context = &instance.context;
        match command {
            Command::SetSettings { settings, .. } => {
                if let Some(updated) = self.store_settings(plugin, context, settings) {
                    self.host.send(plugin, updated.event("didReceiveSettings"));
                }
            }
            Command::GetSettings { .. } => {
                let s = runtime::lock(&self.state);
                if let Some(key) = s.own_key(plugin, context) {
                    let msg = s.instance(context, &key).event("didReceiveSettings");
                    drop(s);
                    self.host.send_to_inspector(context, msg);
                }
            }
            Command::SetGlobalSettings { settings } => {
                self.save_global(plugin, &settings);
                self.host
                    .send(plugin, protocol::did_receive_global_settings(&settings));
            }
            Command::GetGlobalSettings => {
                let msg = protocol::did_receive_global_settings(&load_global(plugin));
                self.host.send_to_inspector(context, msg);
            }
            Command::OpenUrl { url } => self.open_url(plugin, &url),
            _ => {}
        }
    }

    fn owns_action(&self, plugin: &str, action: &str) -> bool {
        runtime::lock(&self.state)
            .catalog
            .action(action)
            .is_some_and(|(p, _)| p.id == plugin)
    }

    fn plugin_dir(&self, plugin: &str) -> Option<PathBuf> {
        runtime::lock(&self.state)
            .catalog
            .plugins
            .iter()
            .find(|p| p.id == plugin)
            .map(|p| p.dir.clone())
    }

    fn mark(&self, plugin: &str, context: &str, mark: Mark) {
        let mut s = runtime::lock(&self.state);
        let Some(key) = s.own_key(plugin, context) else {
            return;
        };
        let n = s.states_of(&key);
        s.plugin_key(context, n).mark = Some((mark, Instant::now() + MARK_TIME));
        drop(s);
        self.redraw();
    }

    /// Saves new settings into the key's profile; returns the updated key
    /// as its plugin sees it.
    fn store_settings(&self, plugin: &str, context: &str, settings: Value) -> Option<Instance> {
        let mut s = runtime::lock(&self.state);
        s.own_key(plugin, context)?;
        let (page_id, index, member) = parse_context(context)?;
        let (profile_id, page) = s.page_mut(page_id)?;
        let key = page.keys.get_mut(&index)?;
        let key = match member {
            Some((list, i)) => key.groups.get_mut(list)?.get_mut(i)?,
            None => key,
        };
        key.settings = settings;
        let key = key.clone();
        if let Some(Err(e)) = s.profile(&profile_id).map(store::save_profile) {
            s.notify(format!("Cannot save profile: {e}"));
        }
        Some(s.instance(context, &key))
    }

    fn save_global(&self, plugin: &str, settings: &Value) {
        let Some(path) = global_settings_file(plugin) else {
            return;
        };
        let text = settings.to_string();
        if text.len() > MAX_GLOBAL_SETTINGS_BYTES {
            self.log(plugin, "global settings are too large; not saved");
            return;
        }
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, text));
        if let Err(e) = result {
            self.log(plugin, &format!("cannot save global settings: {e}"));
        }
    }

    fn open_url(&self, plugin: &str, url: &str) {
        let url = url.trim();
        // Plugins may open web pages only, not files or programs.
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.log(plugin, &format!("openUrl refused: {url}"));
            return;
        }
        if let Err(e) = os::open_url(url) {
            self.log(plugin, &format!("openUrl: {e}"));
        }
    }

    fn log(&self, plugin: &str, text: &str) {
        use std::io::Write;
        let path = self.host.logs_dir().join(format!("{plugin}.log"));
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "[deckly] {text}");
        }
    }
}

/// Sets one state's value, or every state's when no state is given.
fn set_for_states<T: Clone>(slots: &mut [Option<T>], state: Option<usize>, value: Option<T>) {
    match state {
        Some(i) => {
            if let Some(slot) = slots.get_mut(i) {
                *slot = value;
            }
        }
        None => slots.fill(value),
    }
}

fn global_settings_file(plugin: &str) -> Option<PathBuf> {
    deckly_plugin_host::manifest::valid_uuid(plugin)
        .then(|| store::config_dir().map(|d| d.join("plugin-data").join(format!("{plugin}.json"))))
        .flatten()
}

fn load_global(plugin: &str) -> Value {
    global_settings_file(plugin)
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= MAX_GLOBAL_SETTINGS_BYTES as u64))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Loads a manifest image (PNG or SVG).
fn load_file_image(dir: &Path, relative: &str) -> Option<RgbaImage> {
    let path = image_file(dir, relative)?;
    let result = if path.extension().is_some_and(|e| e == "svg") {
        std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| render_svg(&bytes))
    } else {
        render::decode_file(&path)
    };
    match result {
        Ok(img) => Some(img),
        Err(e) => {
            eprintln!("deckly: plugin image {}: {e}", path.display());
            None
        }
    }
}

/// Decodes what a plugin sends with `setImage`: a data URL (PNG, JPEG, SVG;
/// base64 or URL-encoded) or a path inside the plugin folder.
pub fn decode_plugin_image(data: &str, dir: &Path) -> Result<RgbaImage, String> {
    if data.len() > MAX_IMAGE_DATA {
        return Err("image is too large".into());
    }
    let Some(rest) = data.strip_prefix("data:") else {
        let path = safe_join(dir, data).ok_or("image path outside the plugin")?;
        return load_file_image(dir, data)
            .or_else(|| render::decode_file(&path).ok())
            .ok_or_else(|| "cannot read the image file".into());
    };
    let (meta, payload) = rest.split_once(',').ok_or("malformed data URL")?;
    let bytes = if meta.ends_with(";base64") {
        base64(payload).ok_or("bad base64 data")?
    } else {
        percent_decode(payload)
    };
    if meta.starts_with("image/svg") {
        render_svg(&bytes)
    } else {
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        if img.width() > 2048 || img.height() > 2048 {
            return Err("image is too large".into());
        }
        Ok(img.into_rgba8())
    }
}

fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

fn render_svg(bytes: &[u8]) -> Result<RgbaImage, String> {
    use resvg::{tiny_skia, usvg};
    let options = usvg::Options {
        fontdb: fonts(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).map_err(|e| e.to_string())?;
    let size = IMAGE_SIZE;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("cannot allocate image")?;
    let s = tree.size();
    let scale = (size as f32 / s.width()).min(size as f32 / s.height());
    let (dx, dy) = (
        (size as f32 - s.width() * scale) / 2.0,
        (size as f32 - s.height() * scale) / 2.0,
    );
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut img = RgbaImage::new(size, size);
    for (dst, src) in img.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(img)
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&h), Some(&l)) if hex(h).is_some() && hex(l).is_some() => {
                out.push((hex(h).unwrap_or(0) * 16 + hex(l).unwrap_or(0)) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_round_trip() {
        let c = context("5f0c-11", 14);
        assert_eq!(parse_context(&c), Some(("5f0c-11", 14, None)));
        assert_eq!(parse_context("nodot"), None);
        assert_eq!(parse_context("a.999"), None);
        let c = member_context("5f0c-11", 14, 1, 3);
        assert_eq!(parse_context(&c), Some(("5f0c-11", 14, Some((1, 3)))));
        assert_eq!(parse_context("p.1.2"), Some(("p.1", 2, None)));
        assert_eq!(parse_context("p.1.2.x"), None);
    }

    fn state_with(key: KeyConfig) -> (State, String) {
        let mut profile = deckly_core::Profile::new("Test", DeviceKind::Mk2);
        let page = profile.pages[0].id.clone();
        profile.pages[0].keys.insert(3, key);
        (State::new(vec![profile], Default::default()), page)
    }

    #[test]
    fn contexts_reach_the_actions_inside_a_key() {
        let mut key = KeyConfig::new(actions::MULTI_ACTION, json!({}));
        key.groups[0].push(KeyConfig::new("com.example.a", json!({"n": 1})));
        let (s, page) = state_with(key);
        let member = member_context(&page, 3, 0, 0);
        assert_eq!(
            s.key_at(&member).map(|k| k.action),
            Some("com.example.a".to_owned())
        );
        assert!(s.key_at(&member_context(&page, 3, 0, 1)).is_none());
        assert!(s.key_at(&member_context(&page, 3, 7, 0)).is_none());
        let instance = s.instance(&member, &s.key_at(&member).unwrap());
        assert!(instance.in_multi_action);
        let holder = context(&page, 3);
        assert!(
            !s.instance(&holder, &s.key_at(&holder).unwrap())
                .in_multi_action
        );
    }

    #[test]
    fn key_logic_actions_keep_their_place_on_the_key() {
        let mut key = KeyConfig::new(actions::KEY_LOGIC, json!({}));
        key.groups[1].push(KeyConfig::new("com.example.a", json!({})));
        let (s, page) = state_with(key);
        let member = member_context(&page, 3, 1, 0);
        let instance = s.instance(&member, &s.key_at(&member).unwrap());
        assert!(!instance.in_multi_action);
        assert_eq!((instance.column, instance.row), (3, 0));
    }

    #[test]
    fn a_finished_run_turns_a_switch_and_a_cancelled_one_does_not() {
        let (mut s, page) = state_with(KeyConfig::new(actions::MULTI_ACTION_SWITCH, json!({})));
        let key = context(&page, 3);
        let side = |s: &State| actions::switch_side(&s.key_at(&key).unwrap());
        s.finish_run(&key, true);
        assert_eq!(side(&s), 0);
        assert!(s.plugin_keys.is_empty());
        s.finish_run(&key, false);
        assert_eq!(side(&s), 1);
        assert_eq!(
            s.overlay(&key, &s.key_at(&key).unwrap())
                .and_then(|o| o.mark),
            Some(Mark::Ok)
        );
        s.finish_run(&key, false);
        assert_eq!(side(&s), 0);
        // A key removed during its run is no reason to stop.
        s.finish_run("gone.1", false);
    }

    #[test]
    fn decodes_data_urls() {
        let dir = Path::new(".");
        let svg = "data:image/svg+xml;charset=utf8,%3Csvg xmlns='http://www.w3.org/2000/svg' \
                   width='72' height='72'%3E%3Crect width='72' height='72' fill='%23ff0000'/%3E%3C/svg%3E";
        let img = decode_plugin_image(svg, dir).unwrap();
        assert_eq!(img.dimensions(), (IMAGE_SIZE, IMAGE_SIZE));
        assert_eq!(img.get_pixel(70, 70).0, [255, 0, 0, 255]);
        // A 1x1 green PNG in base64.
        let mut png = Vec::new();
        RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = encode_base64(&png);
        let img = decode_plugin_image(&format!("data:image/png;base64,{b64}"), dir).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [0, 255, 0, 255]);
        assert!(decode_plugin_image("data:image/png;base64,!!!", dir).is_err());
        assert!(decode_plugin_image("data:nocomma", dir).is_err());
        assert!(decode_plugin_image("../../etc/passwd", dir).is_err());
        assert!(decode_plugin_image("data:image/svg+xml,<svg", dir).is_err());
    }

    fn encode_base64(bytes: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVs\nbG8").unwrap(), b"hello");
        assert_eq!(percent_decode("a%20b%zz"), b"a b%zz");
    }

    #[test]
    fn set_for_states_targets_one_or_all() {
        let mut v = vec![None, None];
        set_for_states(&mut v, Some(1), Some(3));
        assert_eq!(v, [None, Some(3)]);
        set_for_states(&mut v, None, Some(5));
        assert_eq!(v, [Some(5), Some(5)]);
        set_for_states(&mut v, Some(9), Some(1));
        assert_eq!(v, [Some(5), Some(5)]);
    }
}

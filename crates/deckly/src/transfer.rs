//! Imports Deckly profile exports and the official Stream Deck app's profiles
//! from a folder or backup, with its plugins. Sources are untrusted; only files listed from the source are
//! ever opened, and sizes and counts are bounded.

use crate::backup;
use crate::official::{self, Converted, Preferred, RawProfile, Report};
use crate::runtime::{self, Msg, Shared};
use crate::store;
use deckly_core::Profile;
use deckly_core::profile::MAX_FILE_BYTES;
use deckly_plugin_host::Host;
use deckly_plugin_host::manifest::{Manifest, ManifestError, needs_official_app};
use eframe::egui;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

const MAX_ENTRIES: usize = 200_000;
const MAX_PROFILES: usize = 1000;
const MAX_FOLDER_DEPTH: usize = 8;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// What a background job is doing; the Preferences window shows it.
#[derive(Default)]
pub enum Job {
    #[default]
    Idle,
    Busy(String),
    /// The official app's setup was found on first start.
    Offer(Box<Scan>),
    /// The import is read and waits for a go-ahead.
    Review(Box<Scan>),
    Done(Summary),
    /// An import ended.
    Imported(Outcome),
}

pub type SharedJob = Arc<Mutex<Job>>;

/// The result of a job, in lines for people.
#[derive(Clone, Default)]
pub struct Summary {
    pub title: String,
    pub lines: Vec<String>,
}

/// What an import did, for its result window.
#[derive(Default)]
pub struct Outcome {
    /// Big numbers: the count and what it counts.
    pub totals: Vec<(String, String)>,
    /// Worst first.
    pub sections: Vec<Section>,
}

/// One thing about an import that the user should know.
pub struct Section {
    pub tone: Tone,
    pub title: String,
    pub text: String,
    /// Names, each with the number of keys it concerns, or 0 for none.
    pub names: Vec<(String, usize)>,
    pub lines: Vec<String>,
}

impl Section {
    fn new(tone: Tone, title: String, text: &str) -> Self {
        Self {
            tone,
            title,
            text: text.to_owned(),
            names: Vec::new(),
            lines: Vec::new(),
        }
    }
}

/// Ordered by how much it needs the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    /// Something failed.
    Problem,
    /// Something is left for the user to do.
    Todo,
    /// Good to know.
    Note,
}

/// Runs a job off the UI thread, showing `text` until it is done.
pub fn start(
    job: &SharedJob,
    ctx: &egui::Context,
    text: &str,
    work: impl FnOnce(&Progress) -> Job + Send + 'static,
) {
    let progress = Progress {
        job: job.clone(),
        ctx: ctx.clone(),
    };
    progress.show(text);
    std::thread::spawn(move || {
        let done = work(&progress);
        progress.set(done);
    });
}

pub struct Progress {
    job: SharedJob,
    ctx: egui::Context,
}

impl Progress {
    pub fn show(&self, text: &str) {
        self.set(Job::Busy(text.to_owned()));
    }

    fn set(&self, job: Job) {
        if let Ok(mut slot) = self.job.lock() {
            *slot = job;
        }
        self.ctx.request_repaint();
    }
}

/// The official app's data folder on this computer.
pub fn official_dir() -> Option<PathBuf> {
    let env = |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(windows) {
        env("APPDATA")?.join("Elgato").join("StreamDeck")
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Application Support/com.elgato.StreamDeck")
    } else {
        return None;
    };
    dir.join("ProfilesV3").is_dir().then_some(dir)
}

/// On a first start, reads the official app's setup; finding any, offers it
/// through `job`. Finding none shows nothing.
pub fn offer(job: &SharedJob, ctx: &egui::Context, state: &Shared) {
    if !runtime::lock(state).first_start() {
        return;
    }
    let (job, ctx, state) = (job.clone(), ctx.clone(), state.clone());
    std::thread::spawn(move || {
        let Some(dir) = official_dir() else {
            return;
        };
        let Ok(scan) = scan(&dir.join("ProfilesV3"), Some(&dir), &state) else {
            return;
        };
        if scan.profiles.is_empty() {
            return;
        }
        let mut slot = job.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(*slot, Job::Idle) {
            *slot = Job::Offer(Box::new(scan));
            ctx.request_repaint();
        }
    });
}

/// The files of a set of profiles: a folder or a zip archive.
enum Tree {
    Folder(PathBuf),
    Zip(zip::ZipArchive<File>),
}

impl Tree {
    /// Opens a source and lists its files, with `/` between folders.
    fn open(path: &Path) -> Result<(Self, Vec<String>), String> {
        let mut names = Vec::new();
        if path.is_dir() {
            walk(path, "", MAX_FOLDER_DEPTH, &mut names)?;
            return Ok((Tree::Folder(path.to_owned()), names));
        }
        let file = File::open(path).map_err(|e| e.to_string())?;
        let zip = zip::ZipArchive::new(file).map_err(|_| "this is not a backup file".to_owned())?;
        if zip.len() > MAX_ENTRIES {
            return Err("the file holds too many entries".into());
        }
        names.extend(
            zip.file_names()
                .filter_map(Result::ok)
                .filter(|n| safe_name(n))
                .map(|n| n.into_owned()),
        );
        Ok((Tree::Zip(zip), names))
    }

    fn read(&mut self, name: &str, limit: u64) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        match self {
            Tree::Folder(dir) => {
                let file = File::open(dir.join(name)).map_err(|e| e.to_string())?;
                file.take(limit + 1).read_to_end(&mut bytes)
            }
            Tree::Zip(zip) => zip
                .by_name(name)
                .map_err(|e| e.to_string())?
                .take(limit + 1)
                .read_to_end(&mut bytes),
        }
        .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > limit {
            return Err(format!("{name} is too large"));
        }
        Ok(bytes)
    }
}

/// The files in a folder and the folders below it, `depth` levels down.
pub fn list_files(dir: &Path, depth: usize) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    walk(dir, "", depth, &mut names)?;
    Ok(names)
}

fn walk(dir: &Path, prefix: &str, depth: usize, names: &mut Vec<String>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // Links could lead out of the folder; `file_type` does not follow them.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = format!("{prefix}{name}");
        if kind.is_dir() {
            if depth > 0 {
                walk(&entry.path(), &format!("{path}/"), depth - 1, names)?;
            }
        } else if kind.is_file() {
            if names.len() >= MAX_ENTRIES {
                return Err("the folder holds too many files".into());
            }
            names.push(path);
        }
    }
    Ok(())
}

/// A name inside a zip file that stays inside wherever it is unpacked.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && !name.starts_with('/')
        && name.split('/').all(|part| part != "." && part != "..")
}

/// A file name of Deckly's own for a name from a source.
pub fn clean_file_name(name: &str) -> Option<String> {
    let last = name.rsplit('/').next()?;
    let clean: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(120)
        .collect();
    (!clean.trim_start_matches('.').is_empty()).then_some(clean)
}

/// A profile set read from a source, not yet written anywhere.
pub struct Scan {
    source: PathBuf,
    pub profiles: Vec<Converted>,
    /// File in the source -> where it goes.
    images: Vec<(String, PathBuf)>,
    /// Profiles that could not be read, with the reason.
    pub skipped: Vec<String>,
    pub plugins: Vec<FoundPlugin>,
    /// How many profiles replace ones imported earlier.
    pub replaces: usize,
    /// Profile ids in the order the official app on this computer lists
    /// them. Backups do not hold it.
    order: Vec<String>,
    /// The profile that app keeps for each device. Backups do not hold it.
    preferred: Vec<Preferred>,
}

pub struct FoundPlugin {
    pub name: String,
    pub dir: PathBuf,
    pub kind: PluginKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    /// Can be copied.
    Ready,
    /// The same version is installed already.
    Installed,
    /// Its manifest is encrypted by the official marketplace.
    Encrypted,
    Unreadable,
}

/// Reads the profiles of a backup file, a single profile file or the
/// official data folder (`dir`), and lists the plugins of that folder.
pub fn scan(path: &Path, official: Option<&Path>, state: &Shared) -> Result<Scan, String> {
    let (mut tree, names) = Tree::open(path)?;
    let known: HashSet<&str> = names.iter().map(String::as_str).collect();
    if known.contains(backup::PROFILE_FILE) {
        return scan_native(path, &mut tree, &known, state);
    }
    let (raws, skipped) = read_profiles(&mut tree, &names)?;
    let root = store::config_dir().ok_or("no configuration folder")?;
    let devices = crate::os::official_devices_setting().unwrap_or_default();
    let mut scan = Scan {
        source: path.to_owned(),
        profiles: Vec::new(),
        images: Vec::new(),
        skipped,
        plugins: Vec::new(),
        replaces: 0,
        order: official::profile_order(&devices),
        preferred: official::preferred_profiles(&devices),
    };
    for (raw, dirs) in raws {
        let name = official::name_of(&raw.manifest).to_owned();
        let images = &mut scan.images;
        let id = raw.id.clone();
        let mut place = |page: &str, picture: &str| {
            let from = format!("{}/{picture}", dirs.get(page)?);
            if !known.contains(from.as_str()) {
                return None;
            }
            let to = root
                .join("images")
                .join(&id)
                .join(page)
                .join(clean_file_name(picture)?);
            images.push((from, to.clone()));
            Some(to)
        };
        match official::convert(&raw, &mut place) {
            Ok(converted) => scan.profiles.push(converted),
            Err(e) => scan.skipped.push(format!("{name}: {e}")),
        }
    }
    if scan.profiles.is_empty() && scan.skipped.is_empty() {
        return Err("no profiles were found".into());
    }
    let s = runtime::lock(state);
    scan.replaces = scan
        .profiles
        .iter()
        .filter(|c| s.profile(&c.profile.id).is_some())
        .count();
    drop(s);
    if let Some(dir) = official {
        let usage = plugin_names(&scan.profiles);
        scan.plugins = find_plugins(&dir.join("Plugins"), &usage, state);
    }
    Ok(scan)
}

fn scan_native(
    path: &Path,
    tree: &mut Tree,
    known: &HashSet<&str>,
    state: &Shared,
) -> Result<Scan, String> {
    let bytes = tree.read(backup::PROFILE_FILE, MAX_FILE_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let mut profile = Profile::from_json(text).map_err(|e| e.to_string())?;
    if profile.id.len() > 128
        || profile.id.is_empty()
        || !profile
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("invalid profile id".into());
    }
    let root = store::config_dir().ok_or("no configuration folder")?;
    let prefix = format!("images/{}/", profile.id);
    let mut images = Vec::new();
    profile.for_each_picture(|picture| {
        let Some(name) = picture.as_ref().and_then(|p| p.to_str()) else {
            *picture = None;
            return;
        };
        if !safe_name(name) || !name.starts_with(&prefix) || !known.contains(name) {
            *picture = None;
            return;
        }
        let to = root.join(name);
        images.push((name.to_owned(), to.clone()));
        *picture = Some(to);
    });
    let replaces = usize::from(runtime::lock(state).profile(&profile.id).is_some());
    let mut report = Report::default();
    profile.for_each_key(|_| report.keys += 1);
    Ok(Scan {
        source: path.to_owned(),
        order: vec![profile.id.clone()],
        profiles: vec![Converted { profile, report }],
        images,
        skipped: Vec::new(),
        plugins: Vec::new(),
        replaces,
        preferred: Vec::new(),
    })
}

/// Keep ids (and cross-profile links), but make names distinct within a model.
fn name_imports(imported: &mut [Profile], existing: &[Profile]) {
    let replacing: HashSet<&str> = imported.iter().map(|p| p.id.as_str()).collect();
    let mut names: Vec<Profile> = existing
        .iter()
        .filter(|p| !replacing.contains(p.id.as_str()))
        .filter(|p| {
            !p.is_placeholder()
                || !imported
                    .iter()
                    .any(|q| q.device == p.device && !q.is_placeholder())
        })
        .cloned()
        .collect();
    for profile in imported {
        profile.name = profile.available_name(&names);
        names.push(profile.clone());
    }
}

/// A profile and the folder each of its pages is in.
type Found = (RawProfile, HashMap<String, String>);

/// Finds `<id>.sdProfile` folders anywhere in the source. Returns each
/// profile with the folder of each page, and the profiles left out.
fn read_profiles(tree: &mut Tree, names: &[String]) -> Result<(Vec<Found>, Vec<String>), String> {
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    let mut manifests = Vec::new();
    for name in names {
        let parts: Vec<&str> = name.split('/').collect();
        let Some(at) = parts.iter().position(|p| p.ends_with(".sdProfile")) else {
            continue;
        };
        let Some(id) = parts[at]
            .strip_suffix(".sdProfile")
            .and_then(official::folder_id)
        else {
            continue;
        };
        match parts[at + 1..] {
            ["manifest.json"] => manifests.push((id, None, name)),
            ["Profiles", page, "manifest.json"] => {
                if let Some(page_id) = official::folder_id(page) {
                    let dir = parts[..at + 3].join("/");
                    manifests.push((id, Some((page_id, dir)), name));
                }
            }
            _ => {}
        }
    }
    let mut budget = MAX_MANIFEST_BYTES;
    let mut skipped = Vec::new();
    for (id, page, name) in manifests {
        if found.len() >= MAX_PROFILES {
            return Err("the source holds too many profiles".into());
        }
        let bytes = tree.read(name, MAX_FILE_BYTES.min(budget))?;
        budget -= bytes.len() as u64;
        let value = match official::parse(&bytes) {
            Ok(value) => value,
            Err(e) => {
                skipped.push(format!("{name}: {e}"));
                continue;
            }
        };
        let entry = found.entry(id.clone()).or_insert_with(|| {
            let raw = RawProfile {
                id,
                manifest: serde_json::Value::Null,
                pages: BTreeMap::new(),
            };
            (raw, HashMap::new())
        });
        match page {
            None => entry.0.manifest = value,
            Some((page_id, dir)) => {
                entry.1.insert(page_id.clone(), dir);
                entry.0.pages.insert(page_id, value);
            }
        }
    }
    let (with_manifest, without): (Vec<_>, Vec<_>) = found
        .into_values()
        .partition(|(raw, _)| raw.manifest.is_object());
    skipped.extend(
        without
            .iter()
            .map(|(raw, _)| format!("{}: no manifest", raw.id)),
    );
    Ok((with_manifest, skipped))
}

/// The names profiles give plugins; encrypted plugins cannot tell their own.
fn plugin_names(profiles: &[Converted]) -> HashMap<&str, &str> {
    profiles
        .iter()
        .flat_map(|c| &c.report.plugins)
        .filter(|(_, (name, _))| !name.is_empty())
        .map(|(id, (name, _))| (id.as_str(), name.as_str()))
        .collect()
}

fn find_plugins(dir: &Path, usage: &HashMap<&str, &str>, state: &Shared) -> Vec<FoundPlugin> {
    let installed: HashMap<String, String> = runtime::lock(state)
        .catalog
        .plugins
        .iter()
        .map(|p| (p.id.clone(), p.version.clone()))
        .collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<FoundPlugin> = entries
        .flatten()
        // Plugins being developed are often links to their project; follow them.
        .filter(|e| e.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(".sdPlugin")?.to_owned();
            let dir = entry.path();
            let (name, kind) = match Manifest::load(&dir) {
                Ok(m) if m.offers_nothing() => return None,
                Ok(m) if installed.get(&m.uuid) == Some(&m.version) => {
                    (m.name, PluginKind::Installed)
                }
                Ok(m) => (m.name, PluginKind::Ready),
                Err(e) => {
                    let name = usage
                        .get(id.as_str())
                        .map_or(id.clone(), |n| (*n).to_owned());
                    match e {
                        ManifestError::Encrypted => (name, PluginKind::Encrypted),
                        _ => (name, PluginKind::Unreadable),
                    }
                }
            };
            Some(FoundPlugin { name, dir, kind })
        })
        .collect();
    found.sort_by_key(|p| p.name.to_lowercase());
    found
}

impl Scan {
    pub fn count(&self, kind: PluginKind) -> usize {
        self.plugins.iter().filter(|p| p.kind == kind).count()
    }

    /// Big numbers for what this would bring in, as an import's result has.
    pub fn totals(&self) -> Vec<(String, String)> {
        let profiles = self.profiles.len();
        let keys = self.profiles.iter().map(|c| c.report.keys).sum();
        let plugins = self.count(PluginKind::Ready);
        let mut totals = vec![
            total(profiles, profiles, "profile"),
            total(keys, keys, "key"),
        ];
        if plugins > 0 {
            totals.push(total(plugins, plugins, "plugin"));
        }
        totals
    }
}

/// Writes the profiles of a scan and copies its plugins.
pub fn commit(
    scan: Scan,
    profiles: bool,
    plugins: bool,
    state: &Shared,
    host: Option<&Host>,
    runtime: &Sender<Msg>,
    progress: &Progress,
) -> Outcome {
    let mut outcome = Outcome::default();
    let mut report = Report::default();
    if profiles {
        let mut renamed = Vec::new();
        let done = match write_profiles(&scan, &mut report, state, Some(progress)) {
            Ok(n) => {
                let mut s = runtime::lock(state);
                for converted in &scan.profiles {
                    if let Some(profile) = s.profile(&converted.profile.id)
                        && profile.name != converted.profile.name
                    {
                        renamed.push(format!(
                            "\"{}\" is now \"{}\".",
                            converted.profile.name, profile.name
                        ));
                    }
                }
                s.assign_preferred(&scan.preferred);
                s.place_imported(&scan.order);
                s.drop_placeholders();
                s.save_settings();
                n
            }
            Err(e) => {
                let title = "Profiles not imported".to_owned();
                outcome
                    .sections
                    .push(Section::new(Tone::Problem, title, &e));
                0
            }
        };
        let of = scan.profiles.len() + scan.skipped.len();
        outcome.totals.push(total(done, of, "profile"));
        outcome.totals.push(total(report.keys, report.keys, "key"));
        if !scan.skipped.is_empty() {
            let title = count(scan.skipped.len(), "profile") + " not imported";
            outcome.sections.push(Section {
                lines: scan.skipped.clone(),
                ..Section::new(Tone::Todo, title, "")
            });
        }
        if !renamed.is_empty() {
            let title = count(renamed.len(), "profile") + " renamed";
            outcome.sections.push(Section {
                lines: renamed,
                ..Section::new(Tone::Note, title, "Their names were taken.")
            });
        }
        let _ = runtime.send(Msg::Refresh);
    }

    let mut copied = Vec::new();
    let mut failed = Vec::new();
    let ready: Vec<&FoundPlugin> = scan
        .plugins
        .iter()
        .filter(|p| p.kind == PluginKind::Ready)
        .collect();
    if plugins {
        for (i, plugin) in ready.iter().enumerate() {
            progress.show(&format!(
                "Copying plugin {} of {}: {}",
                i + 1,
                ready.len(),
                plugin.name
            ));
            let result = host
                .ok_or_else(|| "plugins are not available".to_owned())
                .and_then(|h| h.install(&plugin.dir));
            match result {
                Ok(id) => copied.push((*plugin, id)),
                Err(e) => failed.push(format!("{}: {e}", plugin.name)),
            }
        }
        let of = scan.plugins.len() - scan.count(PluginKind::Installed);
        if of > 0 {
            outcome.totals.push(total(copied.len(), of, "plugin"));
        }
        if !failed.is_empty() {
            let title = count(failed.len(), "plugin") + " could not be copied";
            outcome.sections.push(Section {
                lines: failed,
                ..Section::new(Tone::Problem, title, "")
            });
        }
    }
    let left: Vec<&FoundPlugin> = scan
        .plugins
        .iter()
        .filter(|p| plugins && matches!(p.kind, PluginKind::Encrypted | PluginKind::Unreadable))
        .collect();
    let s = runtime::lock(state);
    // The catalog may not list a plugin copied a moment ago yet.
    let has = |id: &str| {
        copied
            .iter()
            .any(|(_, c)| id == c || id.starts_with(&format!("{c}.")))
            || s.catalog.action(id).is_some()
            || s.catalog.plugins.iter().any(|p| p.id == id)
    };
    let missing = to_install(&left, &report, has);
    drop(s);
    if !missing.is_empty() {
        let mut text = String::new();
        if left.iter().any(|p| p.kind == PluginKind::Encrypted) {
            text.push_str(
                "Marketplace copies are encrypted, so get these from their authors' own \
                 downloads. ",
            );
        }
        text.push_str("Their keys are in place and start working once the plugin is installed.");
        let title = count(missing.len(), "plugin") + " to install";
        outcome.sections.push(Section {
            names: missing,
            ..Section::new(Tone::Todo, title, &text)
        });
    }
    let only_official: Vec<(String, usize)> = copied
        .iter()
        .filter(|(p, _)| needs_official_app(&p.dir))
        .map(|(p, _)| (p.name.clone(), 0))
        .collect();
    if !only_official.is_empty() {
        let title = count(only_official.len(), "plugin") + " only for the official app";
        outcome.sections.push(Section {
            names: only_official,
            ..Section::new(
                Tone::Todo,
                title,
                "They are copied, but Deckly cannot run them.",
            )
        });
    }
    if !copied.is_empty() {
        outcome.sections.push(Section::new(
            Tone::Note,
            "Sign in to your plugins again".to_owned(),
            "The official app keeps plugin logins and connections in a form Deckly cannot read. \
             Set them up again in each plugin.",
        ));
    }
    outcome.sections.extend(report_sections(&report));
    outcome.sections.sort_by_key(|section| section.tone);
    outcome
}

/// Brings in the profile a plugin ships and returns its id. A profile that
/// is in Deckly already is left alone, so the user's edits survive.
pub fn import_bundled(file: &Path, state: &Shared) -> Result<String, String> {
    let mut scan = scan(file, None, state)?;
    let id = scan
        .profiles
        .first()
        .map(|c| c.profile.id.clone())
        .ok_or("the file holds no profile")?;
    {
        let s = runtime::lock(state);
        scan.profiles.retain(|c| s.profile(&c.profile.id).is_none());
    }
    if !scan.profiles.is_empty() {
        write_profiles(&scan, &mut Report::default(), state, None)?;
    }
    Ok(id)
}

fn write_profiles(
    scan: &Scan,
    report: &mut Report,
    state: &Shared,
    progress: Option<&Progress>,
) -> Result<usize, String> {
    let show = |text: &str| progress.iter().for_each(|p| p.show(text));
    show("Copying key pictures");
    let (mut tree, _) = Tree::open(&scan.source)?;
    for (from, to) in &scan.images {
        // A missing picture leaves the key with its action's icon.
        let Ok(bytes) = tree.read(from, MAX_IMAGE_BYTES) else {
            report.missing_images += 1;
            continue;
        };
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(to, bytes).map_err(|e| e.to_string())?;
    }
    show("Saving profiles");
    let mut saved: Vec<Profile> = scan.profiles.iter().map(|c| c.profile.clone()).collect();
    let mut state = runtime::lock(state);
    name_imports(&mut saved, &state.profiles);
    for converted in &scan.profiles {
        merge_report(report, &converted.report);
    }
    for profile in &saved {
        store::save_profile(profile)?;
    }
    fold_plugin_actions(report);
    let n = saved.len();
    state.adopt_profiles(saved);
    Ok(n)
}

fn merge_report(into: &mut Report, from: &Report) {
    into.keys += from.keys;
    into.dials += from.dials;
    into.state_variants += from.state_variants;
    into.hotkeys_changed += from.hotkeys_changed;
    into.missing_images += from.missing_images;
    for (id, n) in &from.unsupported {
        *into.unsupported.entry(id.clone()).or_default() += n;
    }
    for (id, (name, n)) in &from.plugins {
        let entry = into.plugins.entry(id.clone()).or_default();
        if entry.0.is_empty() {
            entry.0.clone_from(name);
        }
        entry.1 += n;
    }
}

/// Actions inside Multi Actions may not name their plugin; they count for
/// the plugin whose id begins their own.
fn fold_plugin_actions(report: &mut Report) {
    let (named, unnamed): (Vec<String>, Vec<String>) = report
        .plugins
        .keys()
        .cloned()
        .partition(|id| report.plugins.get(id).is_some_and(|(n, _)| !n.is_empty()));
    for id in unnamed {
        let owner = named.iter().find(|o| id.starts_with(&format!("{o}.")));
        if let Some(owner) = owner
            && let Some((_, keys)) = report.plugins.remove(&id)
            && let Some(entry) = report.plugins.get_mut(owner)
        {
            entry.1 += keys;
        }
    }
}

/// The plugins that keys wait for and the ones that could not be brought
/// along, each once, with the keys that need them, most keys first.
fn to_install(
    left: &[&FoundPlugin],
    report: &Report,
    has: impl Fn(&str) -> bool,
) -> Vec<(String, usize)> {
    let mut plugins: BTreeMap<&str, (&str, usize)> = report
        .plugins
        .iter()
        .filter(|(id, _)| !has(id))
        .map(|(id, (name, n))| {
            (
                id.as_str(),
                (if name.is_empty() { id } else { name }.as_str(), *n),
            )
        })
        .collect();
    for plugin in left {
        let id = plugin
            .dir
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".sdPlugin"))
            .unwrap_or(&plugin.name);
        plugins.entry(id).or_insert((&plugin.name, 0));
    }
    let mut list: Vec<(String, usize)> = plugins
        .into_values()
        .map(|(name, n)| (name.to_owned(), n))
        .collect();
    list.sort_by_key(|(name, n)| (std::cmp::Reverse(*n), name.to_lowercase()));
    list
}

fn report_sections(report: &Report) -> Vec<Section> {
    let mut sections = Vec::new();
    if !report.unsupported.is_empty() {
        let title = count(report.unsupported.len(), "official action") + " not in Deckly yet";
        sections.push(Section {
            names: report
                .unsupported
                .iter()
                .map(|(id, n)| (id.rsplit('.').next().unwrap_or(id).to_owned(), *n))
                .collect(),
            ..Section::new(
                Tone::Todo,
                title,
                "They stay on their keys and do nothing for now.",
            )
        });
    }
    let notes = [
        (report.dials, "dial action", "not imported."),
        (
            report.state_variants,
            "key",
            "had different pictures or titles per state and kept one of them.",
        ),
        (
            report.hotkeys_changed,
            "hotkey",
            "changed; check them in the key settings.",
        ),
        (
            report.missing_images,
            "key picture",
            "missing from the source.",
        ),
    ];
    let lines: Vec<String> = notes
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, what, note)| format!("{} {note}", count(n, what)))
        .collect();
    if !lines.is_empty() {
        sections.push(Section {
            lines,
            ..Section::new(Tone::Note, "Small differences".to_owned(), "")
        });
    }
    sections
}

fn total(done: usize, of: usize, what: &str) -> (String, String) {
    let what = if done == of {
        noun(done, what)
    } else {
        format!("of {}", count(of, what))
    };
    (done.to_string(), what)
}

pub fn count(n: usize, what: &str) -> String {
    format!("{n} {}", noun(n, what))
}

fn noun(n: usize, what: &str) -> String {
    if n == 1 {
        what.to_owned()
    } else {
        format!("{what}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn imports_resolve_names_within_a_batch_and_keep_ids_and_placeholders() {
        use deckly_core::{DeviceKind, Page};
        let existing = Profile::new("Scenes", DeviceKind::Mk2);
        let mut replacement = existing.clone();
        let first = Profile::new("Scenes", DeviceKind::Mk2);
        let second = Profile::new("Scenes", DeviceKind::Mk2);
        let first_id = first.id.clone();
        let mut profiles = vec![replacement.clone(), first, second];
        name_imports(&mut profiles, std::slice::from_ref(&existing));
        assert_eq!(
            profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Scenes", "Scenes copy", "Scenes copy 1"]
        );
        assert_eq!(profiles[1].id, first_id);
        replacement.name = "Renamed".into();
        name_imports(std::slice::from_mut(&mut replacement), &[existing]);
        assert_eq!(replacement.name, "Renamed");
        let placeholder = Profile::new("Default Profile", DeviceKind::Mk2);
        let mut real = Profile::new("Default Profile", DeviceKind::Mk2);
        real.pages.push(Page::new());
        name_imports(std::slice::from_mut(&mut real), &[placeholder]);
        assert_eq!(real.name, "Default Profile");
    }

    #[test]
    fn a_native_export_round_trips_pages_folders_settings_and_pictures() {
        use deckly_core::{DeviceKind, KeyConfig, Settings};
        let root =
            std::env::temp_dir().join(format!("deckly-profile-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("page.svg");
        std::fs::write(&image, b"<svg/>").unwrap();
        let mut profile = Profile::new("Scenes", DeviceKind::Mk2);
        profile.pages[0].icon = Some(image.clone());
        profile
            .applications
            .push(deckly_core::AppRule::new("Game", "game.exe"));
        profile.pages[0].keys.insert(
            0,
            KeyConfig::new(deckly_core::profile::FOLDER_ACTION, serde_json::json!({})),
        );
        profile.normalize_folders();
        let mut nested = KeyConfig::new("com.example.action", serde_json::json!({"foo": "bar"}));
        nested.image = Some(image.clone());
        profile.folders[0].keys.insert(1, nested);
        let file = root.join("Scenes.decklyProfile");
        backup::export_profile(&file, profile.clone()).unwrap();
        std::fs::remove_file(&image).unwrap();
        let state = Arc::new(Mutex::new(crate::runtime::State::new(
            vec![],
            Settings::default(),
        )));
        let read = scan(&file, None, &state).unwrap();
        assert_eq!(read.profiles.len(), 1);
        assert_eq!(read.replaces, 0);
        assert!(read.plugins.is_empty());
        let mut imported = read.profiles.into_iter().next().unwrap().profile;
        let (mut tree, names) = Tree::open(&file).unwrap();
        assert!(
            names
                .iter()
                .all(|n| n == backup::PROFILE_FILE || n.starts_with("images/"))
        );
        for (from, _) in &read.images {
            assert_eq!(tree.read(from, MAX_IMAGE_BYTES).unwrap(), b"<svg/>");
        }
        assert_eq!(read.images.len(), 2);
        imported.for_each_picture(|p| {
            if p.is_some() {
                *p = Some(image.clone());
            }
        });
        assert_eq!(imported, profile);
        let existing_bytes = std::fs::read(&file).unwrap();
        assert!(backup::export_profile(&file, profile).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), existing_bytes);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_exported_profile_keeps_no_full_path_to_an_icon() {
        use deckly_core::{AppRule, DeviceKind, Settings};
        let root =
            std::env::temp_dir().join(format!("deckly-profile-icons-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut profile = Profile::new("Game", DeviceKind::Mk2);
        let program = std::env::current_exe().unwrap();
        profile.applications = vec![
            AppRule::new("Game", "game.exe").with_icon(&program.to_string_lossy()),
            AppRule::new("Music", "music").with_icon(r"shell:AppsFolder\music"),
        ];
        let file = root.join("Game.decklyProfile");
        backup::export_profile(&file, profile).unwrap();
        let state = Arc::new(Mutex::new(crate::runtime::State::new(
            vec![],
            Settings::default(),
        )));
        let read = scan(&file, None, &state).unwrap();
        let rules = &read.profiles[0].profile.applications;
        assert_eq!(rules[0].icon, "");
        assert_eq!(rules[1].icon, r"shell:AppsFolder\music");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_profiles_cannot_import_images_outside_their_folder() {
        use deckly_core::{DeviceKind, KeyConfig, Settings};
        let root =
            std::env::temp_dir().join(format!("deckly-profile-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("hostile.decklyProfile");
        let mut profile = Profile::new("Test", DeviceKind::Mk2);
        profile.id = "abc".into();
        for (index, path) in [
            "images/abc/ok.svg",
            "images/abc/../../evil.svg",
            "images/other/icon.svg",
            "/secret.svg",
            "images/abc/missing.svg",
        ]
        .iter()
        .enumerate()
        {
            let mut key = KeyConfig::new("com.example.a", serde_json::json!({}));
            key.image = Some(path.into());
            profile.pages[0].keys.insert(index as u8, key);
        }
        let write = |profile: &Profile| {
            let mut zip = zip::ZipWriter::new(File::create(&file).unwrap());
            zip.start_file(backup::PROFILE_FILE, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(profile.to_json().unwrap().as_bytes())
                .unwrap();
            for path in [
                "images/abc/ok.svg",
                "images/abc/../../evil.svg",
                "images/other/icon.svg",
                "/secret.svg",
            ] {
                zip.start_file(path, SimpleFileOptions::default()).unwrap();
                zip.write_all(b"svg").unwrap();
            }
            zip.finish().unwrap();
        };
        let state = Arc::new(Mutex::new(crate::runtime::State::new(
            vec![],
            Settings::default(),
        )));
        write(&profile);
        let read = scan(&file, None, &state).unwrap();
        assert_eq!(read.images.len(), 1);
        assert_eq!(read.images[0].0, "images/abc/ok.svg");
        let keys = &read.profiles[0].profile.pages[0].keys;
        assert!(keys[&0].image.is_some());
        assert!((1..5).all(|i| keys[&i].image.is_none()));
        profile.id = "../escape".into();
        write(&profile);
        assert!(scan(&file, None, &state).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn unsafe_names_are_refused() {
        assert!(safe_name("Profiles/a.sdProfile/manifest.json"));
        for name in ["../a", "a/../b", "/etc/passwd", "C:\\x", "a\\b", "c:/x", ""] {
            assert!(!safe_name(name), "{name}");
        }
    }

    #[test]
    fn file_names_are_cleaned() {
        assert_eq!(
            clean_file_name("Images/ABC123Z.png").as_deref(),
            Some("ABC123Z.png")
        );
        assert_eq!(clean_file_name("Images/..").as_deref(), None);
        assert_eq!(clean_file_name("a/b c$.webp").as_deref(), Some("bc.webp"));
    }

    const PROFILE: &str = "11111111-1111-4111-8111-111111111111";
    const PAGE: &str = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA";

    fn backup(path: &Path) {
        let prefix = format!("Profiles/{}.sdProfile", PROFILE.to_uppercase());
        let manifest = format!(
            r#"{{"Device":{{"Model":"20GBA9901","UUID":""}},"Name":"Mine",
                "Pages":{{"Current":"{0}","Default":"{0}","Pages":["{0}"]}}}}"#,
            PAGE.to_lowercase()
        );
        let page = r#"{"Controllers":[{"Type":"Keypad","Actions":{"0,0":{
            "UUID":"com.example.a",
            "States":[{"Image":"Images/ok.png"}]},
            "1,0":{"UUID":"com.example.b","States":[{"Image":"../../secret.png"}]}}}]}"#;
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        for (name, data) in [
            (format!("{prefix}/manifest.json"), manifest.as_str()),
            (format!("{prefix}/Profiles/{PAGE}/manifest.json"), page),
            (format!("{prefix}/Profiles/{PAGE}/Images/ok.png"), "png"),
            ("secret.png".to_owned(), "secret"),
            (
                "Profiles/not-a-uuid.sdProfile/manifest.json".to_owned(),
                "{}",
            ),
            ("Resources/manifest.json".to_owned(), "{}"),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_backup_file_is_read_with_its_pages_and_only_its_own_pictures() {
        let dir = std::env::temp_dir().join(format!("deckly-transfer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("b.streamDeckProfilesBackup");
        backup(&file);
        let (mut tree, names) = Tree::open(&file).unwrap();
        let (raws, skipped) = read_profiles(&mut tree, &names).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(raws.len(), 1);
        let (raw, dirs) = &raws[0];
        assert_eq!(raw.id, PROFILE);
        let page = PAGE.to_lowercase();
        assert!(raw.pages.contains_key(&page));
        assert_eq!(
            tree.read(&format!("{}/Images/ok.png", dirs[&page]), 8)
                .unwrap(),
            b"png"
        );
        assert!(
            tree.read(&format!("{}/Images/ok.png", dirs[&page]), 1)
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn actions_without_a_plugin_name_count_for_their_plugin() {
        let mut report = Report::default();
        report
            .plugins
            .insert("com.example.p".into(), ("P".into(), 2));
        report
            .plugins
            .insert("com.example.p.action".into(), (String::new(), 3));
        report
            .plugins
            .insert("com.example.pp.action".into(), (String::new(), 1));
        fold_plugin_actions(&mut report);
        assert_eq!(report.plugins["com.example.p"], ("P".into(), 5));
        assert_eq!(report.plugins["com.example.pp.action"].1, 1);
        assert_eq!(report.plugins.len(), 2);
    }

    fn found(id: &str, name: &str, kind: PluginKind) -> FoundPlugin {
        FoundPlugin {
            name: name.into(),
            dir: PathBuf::from(format!("{id}.sdPlugin")),
            kind,
        }
    }

    #[test]
    fn plugins_to_install_are_listed_once_with_their_keys_most_first() {
        let mut report = Report::default();
        for (id, name, keys) in [
            ("com.example.a", "Alpha", 2),
            ("com.example.b", "Beta", 9),
            ("com.example.copied", "Copied", 50),
            ("com.example.nameless", "", 1),
        ] {
            report.plugins.insert(id.into(), (name.into(), keys));
        }
        let alpha = found("com.example.a", "Alpha", PluginKind::Encrypted);
        let zeta = found("com.example.z", "Zeta", PluginKind::Encrypted);
        let omega = found("com.example.o", "com.example.o", PluginKind::Unreadable);
        let list = to_install(&[&alpha, &zeta, &omega], &report, |id| {
            id == "com.example.copied"
        });
        assert_eq!(
            list,
            [
                ("Beta".to_owned(), 9),
                ("Alpha".to_owned(), 2),
                ("com.example.nameless".to_owned(), 1),
                ("com.example.o".to_owned(), 0),
                ("Zeta".to_owned(), 0),
            ]
        );
    }

    #[test]
    fn an_import_result_puts_failures_first_and_counts_what_came_across() {
        let scan = Scan {
            source: PathBuf::new(),
            profiles: Vec::new(),
            images: Vec::new(),
            skipped: Vec::new(),
            plugins: vec![
                found("com.example.a", "Alpha", PluginKind::Ready),
                found("com.example.b", "Beta", PluginKind::Encrypted),
                found("com.example.c", "Gamma", PluginKind::Installed),
            ],
            replaces: 0,
            order: Vec::new(),
            preferred: Vec::new(),
        };
        assert_eq!(scan.totals()[2], ("1".to_owned(), "plugin".to_owned()));
        let state = Arc::new(Mutex::new(crate::runtime::State::new(
            Vec::new(),
            deckly_core::Settings::default(),
        )));
        let (runtime, _) = std::sync::mpsc::channel();
        let progress = Progress {
            job: SharedJob::default(),
            ctx: egui::Context::default(),
        };
        let done = commit(scan, false, true, &state, None, &runtime, &progress);
        assert_eq!(done.totals, [("0".to_owned(), "of 2 plugins".to_owned())]);
        let titles: Vec<&str> = done.sections.iter().map(|s| s.title.as_str()).collect();
        // Nothing was copied, so there are no plugin logins to set up.
        assert_eq!(
            titles,
            ["1 plugin could not be copied", "1 plugin to install"]
        );
        assert_eq!(done.sections[0].tone, Tone::Problem);
        assert_eq!(done.sections[0].lines, ["Alpha: plugins are not available"]);
        assert_eq!(done.sections[1].names, [("Beta".to_owned(), 0)]);
    }

    #[test]
    fn plugin_folders_are_sorted_into_what_can_be_copied() {
        let dir = std::env::temp_dir().join(format!("deckly-found-{}", std::process::id()));
        let manifest = br#"{"UUID":"com.example.a","Name":"Alpha","CodePath":"a.js"}"#;
        std::fs::create_dir_all(dir.join("com.example.a.sdPlugin")).unwrap();
        std::fs::write(dir.join("com.example.a.sdPlugin/manifest.json"), manifest).unwrap();
        std::fs::create_dir_all(dir.join("com.example.b.sdPlugin")).unwrap();
        std::fs::write(
            dir.join("com.example.b.sdPlugin/manifest.json"),
            b"ELGATOrest",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("com.example.c.sdPlugin")).unwrap();
        std::fs::write(dir.join("com.example.c.sdPlugin/manifest.json"), b"{}").unwrap();
        std::fs::create_dir_all(dir.join("com.example.d.sdPlugin")).unwrap();
        std::fs::write(
            dir.join("com.example.d.sdPlugin/manifest.json"),
            br#"{"UUID":"com.example.d","Name":"Delta","IconEditorPath":"index.html"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("not-a-plugin")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            dir.join("com.example.a.sdPlugin"),
            dir.join("com.example.linked.sdPlugin"),
        )
        .unwrap();
        let state = Arc::new(Mutex::new(crate::runtime::State::new(
            Vec::new(),
            deckly_core::Settings::default(),
        )));
        let usage = HashMap::from([("com.example.b", "Beta")]);
        let found = find_plugins(&dir, &usage, &state);
        let kind = |name: &str| found.iter().find(|p| p.name == name).map(|p| p.kind);
        assert_eq!(kind("Alpha"), Some(PluginKind::Ready));
        // An encrypted plugin takes its name from the profiles that use it.
        assert_eq!(kind("Beta"), Some(PluginKind::Encrypted));
        assert_eq!(kind("com.example.c"), Some(PluginKind::Unreadable));
        // No actions and no code: nothing to import.
        assert_eq!(kind("Delta"), None);
        #[cfg(unix)]
        assert_eq!(found.iter().filter(|p| p.name == "Alpha").count(), 2);
        #[cfg(not(unix))]
        assert_eq!(found.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_is_not_a_backup() {
        let dir = std::env::temp_dir().join(format!("deckly-garbage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.streamDeckProfilesBackup");
        std::fs::write(&file, b"not a zip").unwrap();
        assert!(Tree::open(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

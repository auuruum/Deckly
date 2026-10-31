//! Moving in from the official Stream Deck app: finds its data, reads its
//! profiles from a folder or a backup file, and copies them and its plugins
//! into Deckly. Sources are untrusted; only files listed from the source are
//! ever opened, and sizes and counts are bounded.

use crate::official::{self, Converted, RawProfile, Report};
use crate::runtime::{self, Msg, Shared};
use crate::store;
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
    /// The import is read and waits for a go-ahead.
    Review(Box<Scan>),
    Done(Summary),
}

pub type SharedJob = Arc<Mutex<Job>>;

/// The result of a job, in lines for people.
#[derive(Clone, Default)]
pub struct Summary {
    pub title: String,
    pub lines: Vec<String>,
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
    let (raws, skipped) = read_profiles(&mut tree, &names)?;
    let root = store::config_dir().ok_or("no configuration folder")?;
    let mut scan = Scan {
        source: path.to_owned(),
        profiles: Vec::new(),
        images: Vec::new(),
        skipped,
        plugins: Vec::new(),
        replaces: 0,
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
) -> Summary {
    let mut summary = Summary {
        title: "Import finished".to_owned(),
        lines: Vec::new(),
    };
    let mut report = Report::default();
    if profiles {
        match write_profiles(&scan, &mut report, state, Some(progress)) {
            Ok(n) => summary.lines.push(count(n, "profile") + " imported."),
            Err(e) => summary.lines.push(format!("Profiles not imported: {e}")),
        }
        for line in &scan.skipped {
            summary.lines.push(format!("Skipped {line}"));
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
                Ok(_) => copied.push(*plugin),
                Err(e) => failed.push(format!("{}: {e}", plugin.name)),
            }
        }
        if !scan.plugins.is_empty() {
            summary
                .lines
                .push(count(copied.len(), "plugin") + " copied.");
        }
        let only_official: Vec<&str> = copied
            .iter()
            .filter(|p| needs_official_app(&p.dir))
            .map(|p| p.name.as_str())
            .collect();
        if !only_official.is_empty() {
            summary.lines.push(format!(
                "Copied, but they work only in the official app: {}.",
                only_official.join(", ")
            ));
        }
        for line in failed {
            summary.lines.push(format!("Not copied, {line}"));
        }
        let encrypted: Vec<&str> = scan
            .plugins
            .iter()
            .filter(|p| p.kind == PluginKind::Encrypted)
            .map(|p| p.name.as_str())
            .collect();
        let unreadable: Vec<&str> = scan
            .plugins
            .iter()
            .filter(|p| p.kind == PluginKind::Unreadable)
            .map(|p| p.name.as_str())
            .collect();
        if !unreadable.is_empty() {
            summary.lines.push(format!(
                "Not copied, their manifests cannot be read: {}.",
                unreadable.join(", ")
            ));
        }
        if !encrypted.is_empty() {
            summary.lines.push(format!(
                "Not copied, encrypted by the official marketplace ({}): {}. \
                 Get them from their authors' own downloads.",
                encrypted.len(),
                encrypted.join(", ")
            ));
        }
        if !scan.plugins.is_empty() {
            summary.lines.push(
                "Plugin logins and connections are kept by the official app in a form Deckly \
                 cannot read; set them up again in each plugin."
                    .to_owned(),
            );
        }
    }
    if profiles {
        summary.lines.extend(report_lines(&report, state));
    }
    summary
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
    let mut saved = Vec::new();
    for converted in &scan.profiles {
        merge_report(report, &converted.report);
        store::save_profile(&converted.profile)?;
        saved.push(converted.profile.clone());
    }
    fold_plugin_actions(report);
    let n = saved.len();
    runtime::lock(state).adopt_profiles(saved);
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

fn report_lines(report: &Report, state: &Shared) -> Vec<String> {
    let mut lines = vec![count(report.keys, "key") + " read."];
    let s = runtime::lock(state);
    let mut waiting: Vec<(&str, usize)> = report
        .plugins
        .iter()
        .filter(|(id, _)| {
            s.catalog.action(id).is_none() && !s.catalog.plugins.iter().any(|p| p.id == **id)
        })
        .map(|(id, (name, n))| (if name.is_empty() { id } else { name }.as_str(), *n))
        .collect();
    drop(s);
    waiting.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    if !waiting.is_empty() {
        let text: Vec<String> = waiting
            .iter()
            .map(|(name, n)| format!("{name} ({n})"))
            .collect();
        lines.push(format!(
            "Keys stay in place and start working once their plugin is installed: {}.",
            text.join(", ")
        ));
    }
    if !report.unsupported.is_empty() {
        let text: Vec<String> = report
            .unsupported
            .iter()
            .map(|(id, n)| format!("{} ({n})", id.rsplit('.').next().unwrap_or(id)))
            .collect();
        lines.push(format!(
            "Official actions Deckly does not have yet stay on their keys and do nothing: {}.",
            text.join(", ")
        ));
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
    for (n, what, note) in notes {
        if n > 0 {
            lines.push(format!("{} {note}", count(n, what)));
        }
    }
    lines
}

pub fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

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

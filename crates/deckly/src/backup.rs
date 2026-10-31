//! Deckly's own backup: one `.decklyBackup` file (a zip archive) holding the
//! profiles with their key pictures, the settings, what plugins saved and the
//! installed plugins, so a setup can move to another computer or system.
//!
//! Paths in a backup are relative to it; pictures chosen from anywhere on
//! this computer are copied in. A backup is restored as untrusted input: only
//! names that stay inside their folder are unpacked, and sizes are bounded.

use crate::runtime::{self, Msg, Shared};
use crate::store;
use crate::transfer::{Progress, Summary, clean_file_name, list_files, safe_name};
use deckly_core::profile::MAX_FILE_BYTES;
use deckly_core::{Profile, Settings};
use deckly_plugin_host::Host;
use deckly_plugin_host::manifest::valid_uuid;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use zip::write::SimpleFileOptions;

pub const EXTENSION: &str = "decklyBackup";
const FORMAT: u64 = 1;
const INFO_FILE: &str = "backup.json";
const SETTINGS_FILE: &str = "settings.json";
const MAX_SMALL_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RESTORE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;
/// Plugin folders nest deeper than a profile folder does (`node_modules`).
const MAX_PLUGIN_DEPTH: usize = 32;

type Zip = zip::ZipWriter<BufWriter<File>>;

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

/// Writes a backup; a failed one leaves no file behind.
pub fn create(
    path: &Path,
    state: &Shared,
    host: Option<&Host>,
    progress: &Progress,
) -> Result<Summary, String> {
    let tmp = path.with_extension("tmp");
    let done = write_backup(&tmp, path, state, host, progress).and_then(|s| {
        std::fs::rename(&tmp, path)
            .map(|()| s)
            .map_err(|e| e.to_string())
    });
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

fn write_backup(
    tmp: &Path,
    path: &Path,
    state: &Shared,
    host: Option<&Host>,
    progress: &Progress,
) -> Result<Summary, String> {
    let (mut profiles, mut settings) = {
        let s = runtime::lock(state);
        (s.profiles.clone(), s.settings.clone())
    };
    let file = File::create(tmp).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(BufWriter::new(file));
    let info = json!({ "format": FORMAT, "app": env!("CARGO_PKG_VERSION") });
    add_bytes(&mut zip, INFO_FILE, info.to_string().as_bytes())?;

    progress.show("Saving profiles and key pictures");
    let mut missing = 0;
    for profile in &mut profiles {
        for (from, to) in take_pictures(profile) {
            if add_file(&mut zip, &from, &to).is_err() {
                missing += 1;
            }
        }
        let text = profile.to_json().map_err(|e| e.to_string())?;
        let name = format!("profiles/{}.json", profile.id);
        add_bytes(&mut zip, &name, text.as_bytes())?;
    }

    for device in settings.devices.values_mut() {
        let Some(from) = device.screensaver.take() else {
            continue;
        };
        if let Some(name) = from.to_str().and_then(clean_file_name)
            && add_file(&mut zip, &from, &format!("screensavers/{name}")).is_ok()
        {
            device.screensaver = Some(format!("screensavers/{name}").into());
        }
    }
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    add_bytes(&mut zip, SETTINGS_FILE, text.as_bytes())?;

    if let Some(dir) = store::config_dir().map(|d| d.join("plugin-data")) {
        for file in list_files(&dir, 0).unwrap_or_default() {
            add_file(&mut zip, &dir.join(&file), &format!("plugin-data/{file}"))?;
        }
    }

    let mut plugins = 0;
    if let Some(host) = host {
        let dir = host.plugins_dir();
        let mut folders: Vec<String> = std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".sdPlugin"))
            .collect();
        folders.sort();
        for (i, folder) in folders.iter().enumerate() {
            let at = format!("Saving plugin {} of {}: {folder}", i + 1, folders.len());
            progress.show(&at);
            let root = dir.join(folder);
            for file in list_files(&root, MAX_PLUGIN_DEPTH)? {
                add_file(
                    &mut zip,
                    &root.join(&file),
                    &format!("plugins/{folder}/{file}"),
                )?;
            }
            plugins += 1;
        }
    }
    progress.show("Finishing the backup");
    zip.finish()
        .map_err(|e| e.to_string())?
        .flush()
        .map_err(|e| e.to_string())?;

    let mut lines = vec![
        format!("Saved to {}.", path.display()),
        format!("{} profiles and {plugins} plugins.", profiles.len()),
        "The file also holds what plugins saved, which can include logins. Keep it private."
            .to_owned(),
    ];
    if missing > 0 {
        lines.push(format!("{missing} key pictures could not be read."));
    }
    Ok(Summary {
        title: "Backup created".to_owned(),
        lines,
    })
}

/// Points a profile's pictures into the backup; returns what to copy where.
/// A profile keeps its pictures in a folder of its own.
fn take_pictures(profile: &mut Profile) -> Vec<(PathBuf, String)> {
    let id = profile.id.clone();
    let mut names: HashMap<PathBuf, String> = HashMap::new();
    let mut files = Vec::new();
    profile.for_each_key(|key| {
        let Some(path) = key.image.take() else {
            return;
        };
        if !names.contains_key(&path) {
            let Some(file) = path.to_str().and_then(clean_file_name) else {
                return;
            };
            let name = format!("images/{id}/{}-{file}", names.len());
            names.insert(path.clone(), name.clone());
            files.push((path.clone(), name));
        }
        key.image = names.get(&path).map(PathBuf::from);
    });
    files
}

fn add_bytes(zip: &mut Zip, name: &str, bytes: &[u8]) -> Result<(), String> {
    zip.start_file(name, options()).map_err(|e| e.to_string())?;
    zip.write_all(bytes).map_err(|e| e.to_string())
}

fn add_file(zip: &mut Zip, from: &Path, name: &str) -> Result<(), String> {
    let mut file = File::open(from).map_err(|e| e.to_string())?;
    let options = options();
    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        options.unix_permissions(mode & 0o755)
    };
    zip.start_file(name, options).map_err(|e| e.to_string())?;
    io::copy(&mut file, zip).map_err(|e| e.to_string())?;
    Ok(())
}

struct Reader {
    zip: zip::ZipArchive<File>,
    names: Vec<String>,
    budget: u64,
}

impl Reader {
    fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let zip = zip::ZipArchive::new(file).map_err(|_| "this is not a Deckly backup")?;
        if zip.len() > MAX_ENTRIES {
            return Err("the backup holds too many files".into());
        }
        let names = zip
            .file_names()
            .filter_map(Result::ok)
            .filter(|n| safe_name(n) && !n.ends_with('/'))
            .map(|n| n.into_owned())
            .collect();
        Ok(Self {
            zip,
            names,
            budget: MAX_RESTORE_BYTES,
        })
    }

    fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    fn matching(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .names
            .iter()
            .filter(|n| n.starts_with(prefix))
            .cloned()
            .collect();
        names.sort();
        names
    }

    fn read(&mut self, name: &str, limit: u64) -> Result<Vec<u8>, String> {
        let entry = self.zip.by_name(name).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        entry
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > limit {
            return Err(format!("{name} is too large"));
        }
        Ok(bytes)
    }

    fn unpack(&mut self, name: &str, to: &Path) -> Result<(), String> {
        let mut entry = self.zip.by_name(name).map_err(|e| e.to_string())?;
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut file = File::create(to).map_err(|e| e.to_string())?;
        let written = io::copy(&mut (&mut entry).take(self.budget + 1), &mut file)
            .map_err(|e| e.to_string())?;
        if written > self.budget {
            return Err("the backup is too large".into());
        }
        self.budget -= written;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode & 0o755))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Restores a backup. Profiles with the same id are replaced, others stay;
/// the settings are replaced.
pub fn restore(
    path: &Path,
    state: &Shared,
    host: Option<&Host>,
    runtime: &Sender<Msg>,
    progress: &Progress,
) -> Result<Summary, String> {
    let mut backup = Reader::open(path)?;
    if !backup.has(INFO_FILE) {
        return Err("this is not a Deckly backup".into());
    }
    let info: Value = serde_json::from_slice(&backup.read(INFO_FILE, MAX_SMALL_FILE_BYTES)?)
        .map_err(|_| "this is not a Deckly backup")?;
    if info
        .get("format")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX)
        > FORMAT
    {
        return Err("this backup was made by a newer Deckly".into());
    }
    let root = store::config_dir().ok_or("no configuration folder")?;
    let mut lines = Vec::new();

    // Before the plugins, which read these when they start.
    for name in backup.matching("plugin-data/") {
        let file = name.trim_start_matches("plugin-data/");
        if file
            .strip_suffix(".json")
            .is_some_and(|id| valid_uuid(id) && !file.contains('/'))
        {
            backup.unpack(&name, &root.join("plugin-data").join(file))?;
        }
    }

    progress.show("Restoring profiles and key pictures");
    let mut profiles = Vec::new();
    for name in backup.matching("profiles/") {
        let bytes = backup.read(&name, MAX_FILE_BYTES)?;
        let text = String::from_utf8_lossy(&bytes);
        match Profile::from_json(&text) {
            Ok(mut profile) => {
                restore_pictures(&mut profile, &mut backup, &root)?;
                match store::save_profile(&profile) {
                    Ok(()) => profiles.push(profile),
                    Err(e) => lines.push(format!("Skipped {name}: {e}")),
                }
            }
            Err(e) => lines.push(format!("Skipped {name}: {e}")),
        }
    }
    lines.insert(0, format!("{} profiles restored.", profiles.len()));
    runtime::lock(state).adopt_profiles(profiles);

    if backup.has(SETTINGS_FILE) {
        let bytes = backup.read(SETTINGS_FILE, MAX_SMALL_FILE_BYTES)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut settings = Settings::from_json(&text).map_err(|e| e.to_string())?;
        for device in settings.devices.values_mut() {
            device.screensaver = device.screensaver.take().and_then(|saved| {
                let name = saved.to_str()?.to_owned();
                let file = clean_file_name(&name)?;
                if !name.starts_with("screensavers/") || !backup.has(&name) {
                    return None;
                }
                let to = root.join("screensavers").join(file);
                backup.unpack(&name, &to).ok().map(|()| to)
            });
        }
        runtime::lock(state).adopt_settings(settings);
        lines.push("Settings restored.".to_owned());
    }
    let _ = runtime.send(Msg::Settings);
    let _ = runtime.send(Msg::Refresh);

    restore_plugins(&mut backup, host, progress, &mut lines)?;
    Ok(Summary {
        title: "Backup restored".to_owned(),
        lines,
    })
}

/// Unpacks a restored profile's pictures. Keys may only point at pictures of
/// their own profile inside the backup; anything else loses its picture.
fn restore_pictures(profile: &mut Profile, backup: &mut Reader, root: &Path) -> Result<(), String> {
    let folder = format!("images/{}/", profile.id);
    if safe_name(&folder[..folder.len() - 1]) {
        let _ = std::fs::remove_dir_all(root.join(&folder));
    }
    let mut done: HashMap<String, PathBuf> = HashMap::new();
    let mut failed = None;
    profile.for_each_key(|key| {
        let Some(name) = key.image.take().and_then(|p| Some(p.to_str()?.to_owned())) else {
            return;
        };
        if let Some(to) = done.get(&name) {
            key.image = Some(to.clone());
        } else if name.starts_with(&folder) && backup.has(&name) {
            let to = root.join(&name);
            match backup.unpack(&name, &to) {
                Ok(()) => {
                    key.image = Some(to.clone());
                    done.insert(name, to);
                }
                Err(e) => failed = Some(e),
            }
        }
    });
    failed.map_or(Ok(()), Err)
}

fn restore_plugins(
    backup: &mut Reader,
    host: Option<&Host>,
    progress: &Progress,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    let names = backup.matching("plugins/");
    let mut folders: Vec<&str> = names
        .iter()
        .filter_map(|n| n.split('/').nth(1))
        .filter(|f| f.strip_suffix(".sdPlugin").is_some_and(valid_uuid))
        .collect();
    folders.dedup();
    if folders.is_empty() {
        return Ok(());
    }
    let Some(host) = host else {
        lines.push("Plugins were not restored: plugins are not available.".to_owned());
        return Ok(());
    };
    // The prefix makes a later start clean up after a crash.
    let staging = host
        .plugins_dir()
        .join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
    let mut restored = 0;
    for (i, folder) in folders.iter().enumerate() {
        let at = format!("Restoring plugin {} of {}: {folder}", i + 1, folders.len());
        progress.show(&at);
        let prefix = format!("plugins/{folder}/");
        let dir = staging.join(folder);
        for name in names.iter().filter(|n| n.starts_with(&prefix)) {
            backup.unpack(name, &dir.join(&name[prefix.len()..]))?;
        }
        match host.install(&dir) {
            Ok(_) => restored += 1,
            Err(e) => lines.push(format!("Plugin {folder} not restored: {e}")),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = std::fs::remove_dir_all(&staging);
    lines.insert(1, format!("{restored} plugins restored."));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deckly_core::{DeviceKind, KeyConfig};
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deckly-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn writer(path: &Path) -> Zip {
        zip::ZipWriter::new(BufWriter::new(File::create(path).unwrap()))
    }

    fn pictured(image: &str) -> KeyConfig {
        let mut key = KeyConfig::new("com.example.a", json!({}));
        key.image = Some(image.into());
        key
    }

    #[test]
    fn pictures_get_names_inside_the_backup() {
        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.id = "abc".into();
        p.pages[0].keys.insert(0, pictured("/home/me/a b.png"));
        p.pages[0].keys.insert(1, pictured("/home/me/a b.png"));
        let mut multi = KeyConfig::new(deckly_core::profile::MULTI_ACTION, json!({}));
        multi.groups[0].push(pictured(r"C:\pics\c.png"));
        p.pages[0].keys.insert(2, multi);
        let files = take_pictures(&mut p);
        assert_eq!(files.len(), 2);
        let keys = &p.pages[0].keys;
        assert_eq!(keys[&0].image, keys[&1].image);
        let first = keys[&0].image.as_ref().unwrap().to_str().unwrap();
        assert!(first.starts_with("images/abc/") && first.ends_with(".png"));
        assert!(!first.contains(' '));
        let inner = keys[&2].groups[0][0].image.as_ref().unwrap();
        assert!(inner.starts_with("images/abc"));
    }

    #[test]
    fn restoring_pictures_stays_inside_the_profile() {
        let dir = temp("restore-pictures");
        let file = dir.join("b.decklyBackup");
        let mut zip = writer(&file);
        add_bytes(&mut zip, "images/abc/0-a.png", b"png").unwrap();
        add_bytes(&mut zip, "images/other/0-b.png", b"png").unwrap();
        add_bytes(&mut zip, "../evil.png", b"png").unwrap();
        zip.finish().unwrap().flush().unwrap();

        let mut p = Profile::new("Test", DeviceKind::Mk2);
        p.id = "abc".into();
        p.pages[0].keys.insert(0, pictured("images/abc/0-a.png"));
        p.pages[0].keys.insert(1, pictured("images/other/0-b.png"));
        p.pages[0].keys.insert(2, pictured("../evil.png"));
        p.pages[0].keys.insert(3, pictured("/etc/passwd"));
        p.pages[0]
            .keys
            .insert(4, pictured("images/abc/missing.png"));
        let root = dir.join("root");
        let mut backup = Reader::open(&file).unwrap();
        restore_pictures(&mut p, &mut backup, &root).unwrap();
        let keys = &p.pages[0].keys;
        assert_eq!(keys[&0].image, Some(root.join("images/abc/0-a.png")));
        assert!(root.join("images/abc/0-a.png").is_file());
        for key in 1..=4 {
            assert_eq!(keys[&key].image, None);
        }
        assert!(!dir.join("evil.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_reader_bounds_what_it_unpacks() {
        let dir = temp("restore-bounds");
        let file = dir.join("b.decklyBackup");
        let mut zip = writer(&file);
        add_bytes(&mut zip, "big", &[0u8; 1000]).unwrap();
        zip.finish().unwrap().flush().unwrap();
        let mut backup = Reader::open(&file).unwrap();
        assert!(backup.read("big", 10).is_err());
        backup.budget = 10;
        assert!(backup.unpack("big", &dir.join("big")).is_err());
        std::fs::write(&file, b"not a zip").unwrap();
        assert!(Reader::open(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

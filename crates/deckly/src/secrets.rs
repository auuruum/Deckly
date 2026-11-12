//! Secrets of plugin settings forms (passwords, tokens).
//!
//! A secret's value never goes into a profile, a profile export, a backup or
//! a plugin's global settings file. Those hold a reference instead,
//! `{"deckly_secret": "<id>"}`, and the value lives in the system's secret
//! store (`os::secrets`) under the plugin's id and that reference. On a
//! system without a usable store, the values go into one file in Deckly's
//! data folder that only the user can read, and the form says so. When a
//! store is found later, the file's secrets move into it and the file goes.
//!
//! Deckly plugins never see references: [`reveal`] puts the values back on
//! the way to the plugin, and [`keep`] takes them out of settings a plugin
//! sends back.

use crate::os::secrets::{self as system, Name};
use deckly_plugin_host::manifest::valid_uuid;
use deckly_plugin_host::proto::Form;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// The property of a reference object.
pub const MARK: &str = "deckly_secret";
/// The fallback file's name in the data folder.
const FILE: &str = "secrets.json";
/// A fallback file larger than this is not read.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// A secret longer than this is refused.
const MAX_SECRET_BYTES: usize = 8 * 1024;
/// How long a store that failed is left alone before it is asked again, so
/// a locked keyring does not slow every frame down.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// A reference to a secret, as the settings keep it.
pub fn reference(id: &str) -> Value {
    json!({ MARK: id })
}

/// The id in a reference.
pub fn reference_id(v: &Value) -> Option<&str> {
    let o = v.as_object().filter(|o| o.len() == 1)?;
    o.get(MARK)?.as_str().filter(|id| valid_id(id))
}

/// Ids are Deckly's own (UUIDs, `global.<field>`); anything else in a
/// reference was not written by Deckly and is ignored.
fn valid_id(id: &str) -> bool {
    (1..=96).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A new id for a key's secret. Keys have no stable id of their own, so
/// the reference carries one; a copied key shares its secret until either
/// copy is given a new one.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The id of a secret in a plugin's global settings.
pub fn global_id(field: &str) -> String {
    format!("global.{field}")
}

/// Where the values are kept.
enum Backend {
    System,
    File(PathBuf),
}

pub struct Vault {
    backend: Backend,
    /// Why the system's store is not used; shown under secret fields.
    fallback: Option<String>,
    /// Values read or written, by plugin and id; `None` is known missing.
    cache: Mutex<HashMap<(String, String), Option<String>>>,
    /// Reads that failed, with when and why.
    failed: Mutex<HashMap<(String, String), (Instant, String)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

static VAULT: OnceLock<Vault> = OnceLock::new();

/// The vault, set up on first use. That first use may wait for the
/// system's store to answer (a few seconds at most), so the app starts it
/// on a thread of its own early ([`init`]).
pub fn vault() -> &'static Vault {
    VAULT.get_or_init(Vault::open)
}

/// Sets the vault up off the UI thread.
pub fn init() {
    let _ = std::thread::Builder::new()
        .name("deckly-secrets".into())
        .spawn(|| {
            vault();
        });
}

fn fallback_file() -> Option<PathBuf> {
    crate::store::data_dir().map(|d| d.join(FILE))
}

impl Vault {
    fn open() -> Self {
        let Some(file) = fallback_file() else {
            // No data folder: nothing can be kept but by the system.
            return Self::with(Backend::System, None);
        };
        // Tests never touch the user's keyring.
        if cfg!(test) {
            return Self::with(Backend::File(file), Some("tests".into()));
        }
        if std::env::var_os("DECKLY_SECRET_STORE").is_some_and(|v| v == "file") {
            let why = "DECKLY_SECRET_STORE is set to file".to_owned();
            return Self::with(Backend::File(file), Some(why));
        }
        match system::probe() {
            Ok(()) => match migrate(&file) {
                Ok(()) => Self::with(Backend::System, None),
                Err(e) => {
                    let why = format!("the secrets in {} could not be moved: {e}", file.display());
                    Self::with(Backend::File(file), Some(why))
                }
            },
            Err(e) => Self::with(Backend::File(file), Some(e)),
        }
    }

    fn with(backend: Backend, fallback: Option<String>) -> Self {
        Self {
            backend,
            fallback,
            cache: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashMap::new()),
        }
    }

    /// A vault that keeps its secrets in `file`, for tests.
    #[cfg(test)]
    pub fn in_file(file: PathBuf) -> Self {
        Self::with(Backend::File(file), Some("tests".into()))
    }

    /// Why secrets are kept in Deckly's own file rather than the system's
    /// store; `None` when the system's store is used.
    pub fn fallback(&self) -> Option<&str> {
        self.fallback.as_deref()
    }

    /// A secret's value; `Ok(None)` when there is none.
    pub fn get(&self, plugin: &str, id: &str) -> Result<Option<String>, String> {
        let slot = (plugin.to_owned(), id.to_owned());
        if let Some(v) = lock(&self.cache).get(&slot) {
            return Ok(v.clone());
        }
        if let Some((at, why)) = lock(&self.failed).get(&slot)
            && at.elapsed() < RETRY_AFTER
        {
            return Err(why.clone());
        }
        let name = Name { plugin, id };
        let read = match &self.backend {
            Backend::System => system::read(name),
            Backend::File(file) => read_file(file).map(|all| file_value(&all, plugin, id)),
        };
        match read {
            Ok(v) => {
                lock(&self.failed).remove(&slot);
                lock(&self.cache).insert(slot, v.clone());
                Ok(v)
            }
            Err(e) => {
                lock(&self.failed).insert(slot, (Instant::now(), e.clone()));
                Err(e)
            }
        }
    }

    /// Stores a value; an empty one removes the secret.
    pub fn set(&self, plugin: &str, id: &str, value: &str) -> Result<(), String> {
        if value.len() > MAX_SECRET_BYTES {
            return Err(format!("Use at most {MAX_SECRET_BYTES} bytes."));
        }
        if !valid_uuid(plugin) || !valid_id(id) {
            return Err("not a secret of a known plugin".into());
        }
        let name = Name { plugin, id };
        let result = match (&self.backend, value.is_empty()) {
            (Backend::System, false) => system::write(name, value),
            (Backend::System, true) => system::delete(name),
            (Backend::File(file), _) => update_file(file, plugin, id, value),
        };
        result?;
        let slot = (plugin.to_owned(), id.to_owned());
        lock(&self.failed).remove(&slot);
        lock(&self.cache).insert(slot, (!value.is_empty()).then(|| value.to_owned()));
        Ok(())
    }
}

/// The fallback file: `{"plugin id": {"secret id": "value"}}`.
fn read_file(file: &Path) -> Result<Map<String, Value>, String> {
    let len = match std::fs::metadata(file) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    if len > MAX_FILE_BYTES {
        return Err(format!("{} is too large", file.display()));
    }
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(all)) => Ok(all),
        _ => Err(format!("{} is not valid", file.display())),
    }
}

fn file_value(all: &Map<String, Value>, plugin: &str, id: &str) -> Option<String> {
    all.get(plugin)?.get(id)?.as_str().map(str::to_owned)
}

fn update_file(file: &Path, plugin: &str, id: &str, value: &str) -> Result<(), String> {
    let mut all = read_file(file)?;
    let entry = all
        .entry(plugin.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    if let Some(secrets) = entry.as_object_mut() {
        if value.is_empty() {
            secrets.remove(id);
        } else {
            secrets.insert(id.to_owned(), Value::String(value.to_owned()));
        }
    }
    all.retain(|_, v| v.as_object().is_some_and(|o| !o.is_empty()));
    write_private(file, &Value::Object(all).to_string())
}

/// Writes a file only the user can read, through a temporary file so a
/// crash leaves the old one whole.
fn write_private(file: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let fail = |e: std::io::Error| format!("cannot write {}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let tmp = file.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // On Windows the data folder is inside the user's own profile, which
    // other users cannot read.
    let mut f = options.open(&tmp).map_err(fail)?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(fail)?;
    drop(f);
    std::fs::rename(&tmp, file).map_err(fail)
}

/// Moves the fallback file's secrets into the system's store and removes
/// the file. Leaves the file alone if any secret cannot be moved.
fn migrate(file: &Path) -> Result<(), String> {
    if !file.exists() {
        return Ok(());
    }
    let all = read_file(file)?;
    for (plugin, secrets) in &all {
        let Some(secrets) = secrets.as_object() else {
            continue;
        };
        for (id, value) in secrets {
            if let Some(value) = value.as_str().filter(|v| !v.is_empty()) {
                system::write(Name { plugin, id }, value)?;
            }
        }
    }
    std::fs::remove_file(file).map_err(|e| format!("cannot remove {}: {e}", file.display()))
}

/// Puts secret values in place of the references in a Deckly plugin's
/// settings, on their way to the plugin. A secret that cannot be read
/// reaches the plugin as an empty string.
pub fn reveal(vault: &Vault, plugin: &str, settings: &mut Value) {
    let Some(map) = settings.as_object_mut() else {
        return;
    };
    for value in map.values_mut() {
        if let Some(id) = reference_id(value) {
            let secret = vault.get(plugin, id).ok().flatten().unwrap_or_default();
            *value = Value::String(secret);
        }
    }
}

/// Takes the values of the form's secret fields out of settings a plugin
/// sent (`new`), stores them, and leaves references in their place. `old`
/// is what Deckly held before, so a key keeps its reference. A value equal
/// to the stored one, or one sent while the store could not be read, does
/// not overwrite the store. `make_id` names a secret that has no reference
/// yet.
pub fn keep(
    vault: &Vault,
    plugin: &str,
    form: &Form,
    old: &Value,
    new: &mut Value,
    make_id: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    let Some(map) = new.as_object_mut() else {
        return Ok(());
    };
    let mut result = Ok(());
    for key in form.secrets() {
        let id = old.get(key).and_then(reference_id).map(str::to_owned);
        match map.get(key) {
            Some(Value::String(text)) => {
                let text = text.clone();
                let id = match id {
                    Some(id) => id,
                    None if text.is_empty() => {
                        map.remove(key);
                        continue;
                    }
                    None => make_id(key),
                };
                match vault.get(plugin, &id) {
                    Ok(stored) if stored.as_deref().unwrap_or_default() == text => {}
                    Ok(_) => {
                        if let Err(e) = vault.set(plugin, &id, &text) {
                            result = Err(e);
                        }
                    }
                    // What the plugin holds may be the empty stand-in.
                    Err(_) => {}
                }
                map.insert(key.to_owned(), reference(&id));
            }
            // A reference stays as long as it is the key's own.
            Some(v) if reference_id(v).is_some() && reference_id(v) == id.as_deref() => {}
            _ => match id {
                Some(id) => {
                    map.insert(key.to_owned(), reference(&id));
                }
                None => {
                    map.remove(key);
                }
            },
        }
    }
    result
}

/// Stores a secret the user typed into a form and puts its reference into
/// the settings. An empty value removes the secret but keeps the reference.
pub fn store_typed(
    vault: &Vault,
    plugin: &str,
    settings: &mut Value,
    key: &str,
    value: &str,
    make_id: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    let id = settings
        .get(key)
        .and_then(reference_id)
        .map_or_else(|| make_id(key), str::to_owned);
    vault.set(plugin, &id, value)?;
    if !settings.is_object() {
        *settings = Value::Object(Map::new());
    }
    if let Some(map) = settings.as_object_mut() {
        map.insert(key.to_owned(), reference(&id));
    }
    Ok(())
}

/// The value of a secret field as the form shows it.
pub fn typed(vault: &Vault, plugin: &str, settings: &Value, key: &str) -> Result<String, String> {
    match settings.get(key).and_then(reference_id) {
        Some(id) => vault.get(plugin, id).map(Option::unwrap_or_default),
        None => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUGIN: &str = "com.example.obs";

    fn form() -> Form {
        serde_json::from_value(json!({"fields": [
            {"kind": "text", "key": "address"},
            {"kind": "secret", "key": "password"}
        ]}))
        .unwrap()
    }

    fn temp_vault(name: &str) -> (Vault, PathBuf) {
        let dir = crate::store::data_dir()
            .unwrap()
            .join(format!("secrets-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join(FILE);
        (Vault::in_file(file.clone()), file)
    }

    #[test]
    fn the_global_vault_in_tests_never_uses_the_system_store() {
        let v = vault();
        assert_eq!(v.fallback(), Some("tests"));
        assert!(
            matches!(&v.backend, Backend::File(f) if f.starts_with(crate::store::data_dir().unwrap()))
        );
    }

    #[test]
    fn the_fallback_file_keeps_secrets_for_the_user_only() {
        let (vault, file) = temp_vault("file");
        assert_eq!(vault.get(PLUGIN, "a").unwrap(), None);
        vault.set(PLUGIN, "a", "hunter2").unwrap();
        vault.set(PLUGIN, "b", "other").unwrap();
        // A fresh vault reads the same file.
        let again = Vault::in_file(file.clone());
        assert_eq!(again.get(PLUGIN, "a").unwrap().as_deref(), Some("hunter2"));
        again.set(PLUGIN, "a", "").unwrap();
        assert_eq!(Vault::in_file(file.clone()).get(PLUGIN, "a").unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Bad names and huge values are refused.
        assert!(vault.set("../x", "a", "v").is_err());
        assert!(vault.set(PLUGIN, "../a", "v").is_err());
        assert!(
            vault
                .set(PLUGIN, "a", &"x".repeat(MAX_SECRET_BYTES + 1))
                .is_err()
        );
        // A broken file is an error, not a crash, and is not overwritten.
        std::fs::write(&file, "not json").unwrap();
        let broken = Vault::in_file(file.clone());
        assert!(broken.get(PLUGIN, "z").is_err());
        assert!(broken.set(PLUGIN, "z", "v").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not json");
    }

    #[test]
    fn references_hide_values_from_settings() {
        let (vault, _) = temp_vault("refs");
        let ids = |_: &str| "id-1".to_owned();
        let mut settings = json!({"address": "localhost"});
        store_typed(&vault, PLUGIN, &mut settings, "password", "pw", &ids).unwrap();
        assert_eq!(
            settings,
            json!({"address": "localhost", "password": {MARK: "id-1"}})
        );
        assert!(!settings.to_string().contains("pw\""));
        assert_eq!(typed(&vault, PLUGIN, &settings, "password").unwrap(), "pw");
        // The plugin sees the value...
        let mut shown = settings.clone();
        reveal(&vault, PLUGIN, &mut shown);
        assert_eq!(shown, json!({"address": "localhost", "password": "pw"}));
        // ...and only the plugin it belongs to.
        let mut other = settings.clone();
        reveal(&vault, "com.example.other", &mut other);
        assert_eq!(other["password"], "");
        // What the plugin sends back loses the value again.
        let mut back = json!({"address": "remote", "password": "pw"});
        keep(&vault, PLUGIN, &form(), &settings, &mut back, &ids).unwrap();
        assert_eq!(
            back,
            json!({"address": "remote", "password": {MARK: "id-1"}})
        );
        // A new value from the plugin is stored under the same reference.
        let mut changed = json!({"password": "new"});
        keep(&vault, PLUGIN, &form(), &settings, &mut changed, &ids).unwrap();
        assert_eq!(changed["password"], json!({MARK: "id-1"}));
        assert_eq!(vault.get(PLUGIN, "id-1").unwrap().as_deref(), Some("new"));
        // Settings that leave the secret out keep the reference.
        let mut without = json!({"address": "x"});
        keep(&vault, PLUGIN, &form(), &settings, &mut without, &ids).unwrap();
        assert_eq!(without["password"], json!({MARK: "id-1"}));
        // A secret first set by the plugin gets a reference of its own.
        let fresh = |_: &str| "id-2".to_owned();
        let mut first = json!({"password": "from plugin"});
        keep(&vault, PLUGIN, &form(), &json!({}), &mut first, &fresh).unwrap();
        assert_eq!(first["password"], json!({MARK: "id-2"}));
        // Something that is not text never lands in the settings.
        let mut odd = json!({"password": 12});
        keep(&vault, PLUGIN, &form(), &json!({}), &mut odd, &fresh).unwrap();
        assert_eq!(odd, json!({}));
    }

    #[test]
    fn a_store_that_cannot_be_read_is_not_overwritten_by_the_stand_in() {
        let (vault, file) = temp_vault("unreadable");
        vault.set(PLUGIN, "id-1", "real").unwrap();
        std::fs::write(&file, "broken").unwrap();
        let broken = Vault::in_file(file.clone());
        let settings = json!({"password": {MARK: "id-1"}});
        let mut shown = settings.clone();
        reveal(&broken, PLUGIN, &mut shown);
        assert_eq!(shown["password"], "");
        // The plugin echoes the empty stand-in back.
        let mut echoed = shown.clone();
        keep(&broken, PLUGIN, &form(), &settings, &mut echoed, &|_| {
            "x".into()
        })
        .unwrap();
        assert_eq!(echoed, settings);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "broken");
    }

    #[test]
    fn references_that_deckly_did_not_write_are_left_alone() {
        assert_eq!(reference_id(&json!({MARK: "a/b"})), None);
        assert_eq!(reference_id(&json!({MARK: "ok", "more": 1})), None);
        assert_eq!(reference_id(&json!({MARK: 5})), None);
        assert_eq!(
            reference_id(&json!({MARK: global_id("password")})),
            Some("global.password")
        );
    }
}

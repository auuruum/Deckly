//! `deckly-plugin.json`: what a Deckly plugin is and offers. The SDK writes
//! it (`<plugin> --describe`); the host reads it. Unknown fields are
//! ignored; everything else is checked against the limits.

use crate::limits::*;
use crate::message::{check_bytes, check_chars, check_count, json_len, valid_id};
use crate::{PROTOCOL, Validate};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The manifest's file name inside the plugin folder.
pub const MANIFEST_FILE: &str = "deckly-plugin.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub description: String,
    /// An image file inside the plugin folder.
    #[serde(default)]
    pub icon: String,
    /// The group in the action list; the plugin name when empty.
    #[serde(default)]
    pub category: String,
    /// The protocol number the plugin needs.
    pub protocol: u32,
    /// The SDK version that wrote the manifest.
    #[serde(default)]
    pub sdk: String,
    /// Host services the plugin uses; shown to the user at install.
    #[serde(default)]
    pub services: Vec<String>,
    /// The program to run per target triple (`x86_64-unknown-linux-gnu`),
    /// as a path inside the plugin folder.
    #[serde(default)]
    pub programs: BTreeMap<String, String>,
    #[serde(default)]
    pub actions: Vec<ManifestAction>,
    /// The plugin-wide settings form. Reserved: its shape is decided with
    /// the settings forms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ManifestAction {
    pub id: String,
    pub name: String,
    /// An image file inside the plugin folder, for the action list.
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub tooltip: String,
    /// One or more states; a key without states shows the action icon.
    #[serde(default)]
    pub states: Vec<ManifestState>,
    /// The action's settings form. Reserved, like [`Manifest::form`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ManifestState {
    #[serde(default)]
    pub title: String,
    /// An image file inside the plugin folder.
    #[serde(default)]
    pub image: String,
}

impl Manifest {
    /// Reads and checks a manifest.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "{MANIFEST_FILE} is too large ({} bytes)",
                bytes.len()
            ));
        }
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let m: Self = serde_json::from_slice(bytes)
            .map_err(|e| format!("{MANIFEST_FILE} is not valid: {e}"))?;
        m.validate()
            .map_err(|e| format!("{MANIFEST_FILE} is not valid: {e}"))?;
        Ok(m)
    }

    pub fn action(&self, id: &str) -> Option<&ManifestAction> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// The program for a target: the exact triple, else one for the same
    /// processor and system (a `musl` build on a `gnu` system).
    pub fn program_for(&self, target: &str) -> Option<&str> {
        if let Some(p) = self.programs.get(target) {
            return Some(p);
        }
        let (arch, os) = arch_and_os(target)?;
        self.programs
            .iter()
            .find(|(t, _)| arch_and_os(t) == Some((arch, os)))
            .map(|(_, p)| p.as_str())
    }

    /// Whether this host can speak the protocol the plugin needs.
    pub fn supported(&self) -> bool {
        self.protocol <= PROTOCOL
    }
}

/// The processor and system of a target triple.
fn arch_and_os(target: &str) -> Option<(&str, &str)> {
    let mut parts = target.split('-');
    let arch = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    let os = ["linux", "windows", "darwin", "freebsd"]
        .into_iter()
        .find(|os| rest.contains(os))?;
    Some((arch, os))
}

/// The target triple this program was built for, as plugin manifests name
/// it. Covers the targets Deckly builds for; others get a best guess.
pub fn current_target() -> String {
    let arch = std::env::consts::ARCH;
    if cfg!(target_os = "macos") {
        format!("{arch}-apple-darwin")
    } else if cfg!(windows) {
        let env = if cfg!(target_env = "gnu") {
            "gnu"
        } else {
            "msvc"
        };
        format!("{arch}-pc-windows-{env}")
    } else if cfg!(target_os = "linux") {
        let env = if cfg!(target_env = "musl") {
            "musl"
        } else {
            "gnu"
        };
        format!("{arch}-unknown-linux-{env}")
    } else {
        format!("{arch}-unknown-{}", std::env::consts::OS)
    }
}

fn check_path(what: &str, path: &str) -> Result<(), String> {
    check_bytes(what, path, MAX_PATH_BYTES)?;
    let p = path.replace('\\', "/");
    if p.starts_with('/') || p.split('/').any(|part| part == "..") || p.contains(':') {
        return Err(format!("{what} {path:?} leaves the plugin folder"));
    }
    Ok(())
}

fn check_form(form: &Option<Value>) -> Result<(), String> {
    match form {
        Some(f) if json_len(f) > MAX_FORM_BYTES => {
            Err(format!("a form is larger than {MAX_FORM_BYTES} bytes"))
        }
        _ => Ok(()),
    }
}

impl Validate for Manifest {
    fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!("bad plugin id {:?}", crate::truncate(&self.id, 64)));
        }
        if self.name.trim().is_empty() {
            return Err("no name".into());
        }
        check_chars("the name", &self.name, MAX_NAME_CHARS)?;
        check_bytes("the version", &self.version, MAX_SHORT_TEXT_BYTES)?;
        check_chars("the author", &self.author, MAX_NAME_CHARS)?;
        check_chars("the licence", &self.license, MAX_NAME_CHARS)?;
        check_chars("the description", &self.description, MAX_DESCRIPTION_CHARS)?;
        check_chars("the category", &self.category, MAX_NAME_CHARS)?;
        check_bytes("the SDK version", &self.sdk, MAX_SHORT_TEXT_BYTES)?;
        if !self.icon.is_empty() {
            check_path("the icon", &self.icon)?;
        }
        check_count("services", self.services.len(), MAX_SERVICES)?;
        for s in &self.services {
            check_bytes("a service name", s, MAX_SERVICE_BYTES)?;
        }
        check_count("programs", self.programs.len(), MAX_PROGRAMS)?;
        for (target, program) in &self.programs {
            check_bytes("a target", target, MAX_SHORT_TEXT_BYTES)?;
            check_path("a program", program)?;
        }
        check_count("actions", self.actions.len(), MAX_ACTIONS)?;
        for (i, a) in self.actions.iter().enumerate() {
            if !valid_id(&a.id) {
                return Err(format!("bad action id {:?}", crate::truncate(&a.id, 64)));
            }
            if self.actions[..i].iter().any(|b| b.id == a.id) {
                return Err(format!("action {} is listed twice", a.id));
            }
            check_chars("an action name", &a.name, MAX_NAME_CHARS)?;
            check_chars("a tooltip", &a.tooltip, MAX_NAME_CHARS)?;
            if !a.icon.is_empty() {
                check_path("an action icon", &a.icon)?;
            }
            check_count("states", a.states.len(), MAX_STATES)?;
            for s in &a.states {
                check_chars("a state title", &s.title, MAX_TITLE_CHARS)?;
                if !s.image.is_empty() {
                    check_path("a state image", &s.image)?;
                }
            }
            check_form(&a.form)?;
        }
        check_form(&self.form)
    }
}

//! The Secret Service over D-Bus (GNOME Keyring, KWallet and others that
//! implement `org.freedesktop.secrets`). Needs the OS: the secrets live in
//! the user's keyring daemon, which only D-Bus reaches.
//!
//! Secrets travel in a `plain` session: unencrypted, but only over the
//! user's own session bus, which other users cannot read. Items are found by
//! their attributes; Deckly never asks the user to unlock a locked keyring
//! itself, it reports that the keyring is locked.

use super::Name;
use std::collections::HashMap;
use std::time::Duration;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";
/// A keyring daemon that does not answer must not hold Deckly up for the
/// D-Bus default of 25 seconds.
const TIMEOUT: Duration = Duration::from_secs(3);
const SCHEMA: &str = "app.deckly.PluginSecret";
/// Larger values are refused rather than sent.
const MAX_SECRET_BYTES: usize = 64 * 1024;

/// `(session, parameters, value, content type)`, as the service sends it.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

struct Session {
    connection: Connection,
    path: OwnedObjectPath,
}

fn err(e: impl std::fmt::Display) -> String {
    let text = e.to_string();
    if text.contains("ServiceUnknown") || text.contains("was not provided") {
        "no Secret Service is running (such as GNOME Keyring or KWallet)".into()
    } else {
        format!("the Secret Service failed: {text}")
    }
}

fn open() -> Result<Session, String> {
    let connection = zbus::blocking::connection::Builder::session()
        .map_err(err)?
        .method_timeout(TIMEOUT)
        .build()
        .map_err(err)?;
    let service = Proxy::new(&connection, DEST, SERVICE_PATH, SERVICE).map_err(err)?;
    let (_, path): (OwnedValue, OwnedObjectPath) = service
        .call("OpenSession", &("plain", Value::from("")))
        .map_err(err)?;
    Ok(Session { connection, path })
}

fn attributes(name: Name<'_>) -> HashMap<&str, &str> {
    HashMap::from([
        ("xdg:schema", SCHEMA),
        ("deckly-plugin", name.plugin),
        ("deckly-secret", name.id),
    ])
}

impl Session {
    fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> Result<Proxy<'a>, String> {
        Proxy::new(&self.connection, DEST, path, interface).map_err(err)
    }

    /// The items for a name: (unlocked, locked).
    fn find(&self, name: Name<'_>) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), String> {
        self.proxy(SERVICE_PATH, SERVICE)?
            .call("SearchItems", &(attributes(name),))
            .map_err(err)
    }

    fn close(self) {
        if let Ok(p) = Proxy::new(
            &self.connection,
            DEST,
            self.path.as_str(),
            "org.freedesktop.Secret.Session",
        ) {
            let _ = p.call_method("Close", &());
        }
    }
}

const LOCKED: &str = "the system keyring is locked; unlock it and try again";

fn is_root(path: &ObjectPath<'_>) -> bool {
    path.as_str() == "/"
}

pub fn probe() -> Result<(), String> {
    let session = open()?;
    // A service without a default collection cannot store anything.
    let found = session
        .proxy(DEFAULT_COLLECTION, COLLECTION)
        .and_then(|c| c.get_property::<bool>("Locked").map_err(err))
        .map(drop);
    session.close();
    found.map_err(|e| format!("the Secret Service has no default keyring: {e}"))
}

pub fn read(name: Name<'_>) -> Result<Option<String>, String> {
    let session = open()?;
    let result = (|| {
        let (unlocked, locked) = session.find(name)?;
        let Some(item) = unlocked.first() else {
            return if locked.is_empty() {
                Ok(None)
            } else {
                Err(LOCKED.to_owned())
            };
        };
        let (_, _, value, _): Secret = session
            .proxy(item.as_str(), ITEM)?
            .call("GetSecret", &(&session.path,))
            .map_err(err)?;
        if value.len() > MAX_SECRET_BYTES {
            return Err("a stored secret is too large".into());
        }
        String::from_utf8(value)
            .map(Some)
            .map_err(|_| "a stored secret is not text".into())
    })();
    session.close();
    result
}

pub fn write(name: Name<'_>, value: &str) -> Result<(), String> {
    if value.len() > MAX_SECRET_BYTES {
        return Err("the secret is too large".into());
    }
    let session = open()?;
    let result = (|| {
        let attributes: HashMap<String, String> = attributes(name)
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let properties: HashMap<&str, Value<'_>> = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                Value::from(name.label()),
            ),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(attributes),
            ),
        ]);
        let secret = (
            session.path.as_ref(),
            Vec::<u8>::new(),
            value.as_bytes().to_vec(),
            "text/plain; charset=utf8",
        );
        let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = session
            .proxy(DEFAULT_COLLECTION, COLLECTION)?
            .call("CreateItem", &(properties, secret, true))
            .map_err(err)?;
        if is_root(&prompt) {
            Ok(())
        } else {
            // Storing needs the user to unlock the keyring first.
            Err(LOCKED.to_owned())
        }
    })();
    session.close();
    result
}

pub fn delete(name: Name<'_>) -> Result<(), String> {
    let session = open()?;
    let result = (|| {
        let (unlocked, locked) = session.find(name)?;
        if !locked.is_empty() {
            return Err(LOCKED.to_owned());
        }
        for item in unlocked {
            let prompt: OwnedObjectPath = session
                .proxy(item.as_str(), ITEM)?
                .call("Delete", &())
                .map_err(err)?;
            if !is_root(&prompt) {
                return Err(LOCKED.to_owned());
            }
        }
        Ok(())
    })();
    session.close();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs against a real Secret Service only when asked to, and only in a
    /// private D-Bus session, so it never touches the user's keyring:
    ///
    /// ```text
    /// dbus-run-session -- sh -c 'printf test | gnome-keyring-daemon --unlock \
    ///   --components=secrets >/dev/null; DECKLY_TEST_SECRET_SERVICE=1 \
    ///   cargo test -p deckly secret_service_round_trip -- --ignored'
    /// ```
    #[test]
    #[ignore = "needs a private Secret Service; see the comment"]
    fn secret_service_round_trip() {
        if std::env::var_os("DECKLY_TEST_SECRET_SERVICE").is_none() {
            return;
        }
        let name = Name {
            plugin: "com.deckly.test",
            id: "round-trip",
        };
        probe().unwrap();
        delete(name).unwrap();
        assert_eq!(read(name).unwrap(), None);
        write(name, "first").unwrap();
        write(name, "pässwörd").unwrap();
        assert_eq!(read(name).unwrap().as_deref(), Some("pässwörd"));
        delete(name).unwrap();
        assert_eq!(read(name).unwrap(), None);
    }

    #[test]
    fn attributes_name_the_plugin_and_the_secret() {
        let a = attributes(Name {
            plugin: "p",
            id: "s",
        });
        assert_eq!(a["deckly-plugin"], "p");
        assert_eq!(a["deckly-secret"], "s");
        assert_eq!(a["xdg:schema"], SCHEMA);
    }
}

//! Keys through the desktop's RemoteDesktop portal, for Wayland desktops
//! without the virtual keyboard protocol (GNOME, KDE). The desktop asks the
//! user once; the answer is remembered with a token the portal hands out,
//! saved in Deckly's config folder.

use super::Failure;
use super::keys::{Key, Stroke};
use crate::store;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{DynamicType, ObjectPath, OwnedObjectPath, OwnedValue, Value};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.RemoteDesktop";
const KEYBOARD: u32 = 1;
/// Keep the permission until it is revoked.
const PERSIST: u32 = 2;
const PRESSED: u32 = 1;
const RELEASED: u32 = 0;
const KEY_GAP: Duration = Duration::from_millis(2);
/// How long the user has to answer the desktop's question.
const ANSWER_TIME: Duration = Duration::from_secs(120);

type Options<'a> = HashMap<&'a str, Value<'a>>;
type Results = HashMap<String, OwnedValue>;

struct Session {
    connection: Connection,
    handle: OwnedObjectPath,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner())
}

fn unavailable(e: impl std::fmt::Display) -> Failure {
    Failure::Unavailable(e.to_string())
}

/// A token for one portal call; the portal builds object paths from them, so
/// a repeat could collide with a session that is still closing.
fn call_token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("deckly_{}_{n}", std::process::id())
}

fn token_file() -> Option<PathBuf> {
    Some(store::config_dir()?.join("portal-token"))
}

fn remote_desktop(connection: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(connection, DESTINATION, PATH, INTERFACE)
}

/// Calls a portal method that answers later, on a Request object whose path
/// follows from the token. Listens before calling, so the answer can't pass
/// unseen.
fn request<B>(
    connection: &Connection,
    token: &str,
    method: &str,
    body: &B,
) -> Result<Results, String>
where
    B: Serialize + DynamicType,
{
    let sender = connection
        .unique_name()
        .ok_or("no D-Bus name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let answers = Proxy::new(
        connection,
        DESTINATION,
        path,
        "org.freedesktop.portal.Request",
    )
    .map_err(|e| e.to_string())?;
    let mut answers = answers
        .receive_signal("Response")
        .map_err(|e| e.to_string())?;
    remote_desktop(connection)
        .and_then(|p| p.call_method(method, body))
        .map_err(|e| e.to_string())?;
    let message = answers.next().ok_or("the portal stopped answering")?;
    let (code, results): (u32, Results) =
        message.body().deserialize().map_err(|e| e.to_string())?;
    match code {
        0 => Ok(results),
        1 => Err("permission was refused".into()),
        _ => Err("the portal could not do it".into()),
    }
}

fn start_session() -> Result<Session, String> {
    let connection = Connection::session().map_err(|e| e.to_string())?;
    let types: u32 = remote_desktop(&connection)
        .and_then(|p| p.get_property("AvailableDeviceTypes"))
        .map_err(|e| e.to_string())?;
    if types & KEYBOARD == 0 {
        return Err("the portal offers no keyboard".into());
    }

    let (token, session_token) = (call_token(), call_token());
    let mut options = Options::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("session_handle_token", Value::from(session_token.as_str()));
    let results = request(&connection, &token, "CreateSession", &(options,))?;
    let handle = results
        .get("session_handle")
        .and_then(|v| <&str>::try_from(v).ok())
        .ok_or("the portal made no session")?;
    let handle = OwnedObjectPath::from(ObjectPath::try_from(handle).map_err(|e| e.to_string())?);

    let saved = token_file().and_then(|path| std::fs::read_to_string(path).ok());
    let token = call_token();
    let mut options = Options::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("types", Value::from(KEYBOARD));
    options.insert("persist_mode", Value::from(PERSIST));
    if let Some(saved) = saved.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        options.insert("restore_token", Value::from(saved));
    }
    request(&connection, &token, "SelectDevices", &(&handle, options))?;

    let token = call_token();
    let mut options = Options::new();
    options.insert("handle_token", Value::from(token.as_str()));
    let results = request(&connection, &token, "Start", &(&handle, "", options))?;
    let token = results
        .get("restore_token")
        .and_then(|v| <&str>::try_from(v).ok());
    if let (Some(token), Some(path)) = (token, token_file()) {
        let _ = std::fs::write(path, token);
    }
    Ok(Session { connection, handle })
}

/// Starts a session on a thread of its own: it waits for the user, and
/// nothing can bound a wait on the signal itself.
fn session() -> Result<Session, Failure> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("deckly-portal".into())
        .spawn(move || {
            let _ = tx.send(start_session());
        })
        .map_err(unavailable)?;
    match rx.recv_timeout(ANSWER_TIME) {
        Ok(result) => result.map_err(Failure::Unavailable),
        Err(RecvTimeoutError::Timeout) => Err(unavailable("no answer from the desktop")),
        Err(RecvTimeoutError::Disconnected) => Err(unavailable("the portal stopped answering")),
    }
}

impl Session {
    fn key(&self, proxy: &Proxy<'_>, key: Key, state: u32) -> zbus::Result<()> {
        let options = Options::new();
        proxy.call_method(
            "NotifyKeyboardKeysym",
            &(&self.handle, options, key.sym as i32, state),
        )?;
        std::thread::sleep(KEY_GAP);
        Ok(())
    }

    fn stroke(&self, proxy: &Proxy<'_>, stroke: &Stroke) -> zbus::Result<()> {
        for &key in &stroke.mods {
            self.key(proxy, key, PRESSED)?;
        }
        self.key(proxy, stroke.key, PRESSED)?;
        self.key(proxy, stroke.key, RELEASED)?;
        for &key in stroke.mods.iter().rev() {
            self.key(proxy, key, RELEASED)?;
        }
        Ok(())
    }
}

fn send_all(session: &Session, strokes: &[Stroke]) -> Result<(), Failure> {
    let proxy = remote_desktop(&session.connection).map_err(unavailable)?;
    for (index, stroke) in strokes.iter().enumerate() {
        if let Err(e) = session.stroke(&proxy, stroke) {
            // Asking again is right while nothing was typed.
            return Err(if index == 0 {
                unavailable(e)
            } else {
                Failure::Failed(e.to_string())
            });
        }
    }
    Ok(())
}

pub fn send(strokes: &[Stroke]) -> Result<(), Failure> {
    let mut slot = lock();
    if slot.is_none() {
        *slot = Some(session()?);
    }
    let Some(session) = slot.as_ref() else {
        return Err(unavailable("no portal session"));
    };
    let result = send_all(session, strokes);
    if result.is_err() {
        // The desktop may have ended the session.
        *slot = None;
    }
    result
}

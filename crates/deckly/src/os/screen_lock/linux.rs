//! Linux: systemd-logind, or elogind, over the system bus. The desktop's
//! locker reports through the session's `LockedHint`, and `Lock` and `Unlock`
//! signals ask the session to lock. A locker that does neither goes unseen.
//!
//! Signals come from the session's real path, not from `session/auto`, so the
//! path is taken from the user's display session. That also works for an app
//! started outside any session, as a systemd user unit is.

use crate::lock_gate::Gate;
use std::collections::HashMap;
use std::time::Duration;
use zbus::blocking::proxy::Builder;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::{Message, Type};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{MatchRule, Result};

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const USER: &str = "org.freedesktop.login1.User";
const SESSION: &str = "org.freedesktop.login1.Session";
const HINT: &str = "LockedHint";
/// Between attempts to reach logind again after it went away.
const RETRY: Duration = Duration::from_secs(30);

pub fn watch(gate: Gate) {
    let undetected = gate.clone();
    let run = move || {
        let mut reached = false;
        loop {
            // Only logind going away ends this; the state stays as it was.
            let result = follow(&gate, &mut reached);
            if let (Err(e), false) = (result, reached) {
                eprintln!("deckly: cannot watch the lock screen: {e}");
                gate.set_undetected();
                return;
            }
            std::thread::sleep(RETRY);
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("deckly-screen-lock".into())
        .spawn(run)
    {
        eprintln!("deckly: cannot watch the lock screen: {e}");
        undetected.set_undetected();
    }
}

/// Reports the session's state until the connection to it ends.
fn follow(gate: &Gate, reached: &mut bool) -> Result<()> {
    let connection = Connection::system()?;
    let session = display_session(&connection)?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(LOGIND)?
        .path(session.as_str())?
        .build();
    // Subscribed before reading, so a change between the two is not lost.
    let signals = MessageIterator::for_match_rule(rule, &connection, None)?;
    let proxy: Proxy<'_> = Builder::new(&connection)
        .destination(LOGIND)?
        .path(session.as_str())?
        .interface(SESSION)?
        .cache_properties(CacheProperties::No)
        .build()?;
    gate.set_locked(proxy.get_property(HINT)?);
    *reached = true;
    for message in signals {
        match change(&message?) {
            Change::To(locked) => gate.set_locked(locked),
            Change::HintChanged => gate.set_locked(proxy.get_property(HINT)?),
            Change::None => {}
        }
    }
    Ok(())
}

fn display_session(connection: &Connection) -> Result<OwnedObjectPath> {
    let manager = Proxy::new(connection, LOGIND, MANAGER_PATH, MANAGER)?;
    let uid = rustix::process::getuid().as_raw();
    let user: OwnedObjectPath = manager.call("GetUser", &(uid,))?;
    let user = Proxy::new(connection, LOGIND, user.as_str(), USER)?;
    let (_, session): (String, OwnedObjectPath) = user.get_property("Display")?;
    if session.as_str() == "/" {
        return Err(zbus::Error::Failure("no display session".into()));
    }
    Ok(session)
}

#[derive(Debug, PartialEq)]
enum Change {
    To(bool),
    HintChanged,
    None,
}

/// What a signal from the session means. Other property changes are ignored:
/// after a `Lock`, `LockedHint` may stay false on a locker that never sets it.
fn change(message: &Message) -> Change {
    let header = message.header();
    match header.member().map(|member| member.as_str()) {
        Some("Lock") => Change::To(true),
        Some("Unlock") => Change::To(false),
        Some("PropertiesChanged") => {
            type Body = (String, HashMap<String, OwnedValue>, Vec<String>);
            match message.body().deserialize::<Body>() {
                Ok((interface, changed, stale))
                    if interface == SESSION
                        && (changed.contains_key(HINT) || stale.iter().any(|n| n == HINT)) =>
                {
                    Change::HintChanged
                }
                _ => Change::None,
            }
        }
        _ => Change::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    const PATH: &str = "/org/freedesktop/login1/session/_31";
    const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

    fn signal(member: &str) -> Message {
        Message::signal(PATH, SESSION, member)
            .unwrap()
            .build(&())
            .unwrap()
    }

    fn properties(interface: &str, changed: &[&str], stale: &[&str]) -> Message {
        let changed: HashMap<&str, Value<'_>> =
            changed.iter().map(|n| (*n, Value::from(true))).collect();
        Message::signal(PATH, PROPERTIES, "PropertiesChanged")
            .unwrap()
            .build(&(interface, changed, stale))
            .unwrap()
    }

    #[test]
    fn lock_signals_set_the_state() {
        assert_eq!(change(&signal("Lock")), Change::To(true));
        assert_eq!(change(&signal("Unlock")), Change::To(false));
        assert_eq!(change(&signal("PauseDevice")), Change::None);
    }

    #[test]
    fn only_the_hint_of_the_session_counts() {
        let hint = properties(SESSION, &[HINT], &[]);
        assert_eq!(change(&hint), Change::HintChanged);
        let stale = properties(SESSION, &[], &[HINT]);
        assert_eq!(change(&stale), Change::HintChanged);
        let idle = properties(SESSION, &["IdleHint"], &[]);
        assert_eq!(change(&idle), Change::None);
        let other = properties("org.example.Other", &[HINT], &[]);
        assert_eq!(change(&other), Change::None);
    }
}

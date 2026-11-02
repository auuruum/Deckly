//! Linux: X11 window managers keep the active window in a root window
//! property, and a change of it arrives as an event. Wayland keeps this from
//! other programs on purpose and has no common way to ask, and XWayland only
//! sees part of the windows, so a Wayland session reports that it cannot.

use super::Report;
use std::error::Error;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, Window,
};

/// `WM_CLASS` is two short names; a window can claim more.
const MAX_CLASS_WORDS: u32 = 64;

pub fn watch(report: Report) {
    if is_wayland() {
        report(Err(
            "Wayland does not tell applications which window is active. \
             Use an X11 session to switch profiles by window."
                .into(),
        ));
        return;
    }
    let failed = report.clone();
    let run = move || {
        if let Err(e) = follow(&report) {
            report(Err(format!("Cannot follow the active window: {e}")));
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("deckly-active-window".into())
        .spawn(run)
    {
        failed(Err(format!("Cannot follow the active window: {e}")));
    }
}

fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
}

struct Atoms {
    active: Atom,
    pid: Atom,
}

/// Reports the active window until the connection to X ends.
fn follow(report: &Report) -> Result<(), Box<dyn Error>> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or("no X11 screen")?
        .root;
    let atoms = Atoms {
        active: connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom,
        pid: connection.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom,
    };
    let events = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
    connection
        .change_window_attributes(root, &events)?
        .check()?;
    // Read after subscribing, so a change between the two is not lost.
    report(Ok(active_application(&connection, root, &atoms)?));
    loop {
        if let Event::PropertyNotify(event) = connection.wait_for_event()?
            && event.atom == atoms.active
        {
            report(Ok(active_application(&connection, root, &atoms)?));
        }
    }
}

/// The names of the active window's application. A window that went away in
/// the meantime has none; only a lost connection is an error.
fn active_application(
    connection: &impl Connection,
    root: Window,
    atoms: &Atoms,
) -> Result<Vec<String>, Box<dyn Error>> {
    let window = connection
        .get_property(false, root, atoms.active, AtomEnum::WINDOW, 0, 1)?
        .reply()?
        .value32()
        .and_then(|mut values| values.next())
        .filter(|window| *window != 0);
    let Some(window) = window else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    let class = connection.get_property(
        false,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        0,
        MAX_CLASS_WORDS,
    )?;
    if let Some(reply) = gone_is_none(class.reply())? {
        names.extend(class_names(&reply.value));
    }
    let pid = connection.get_property(false, window, atoms.pid, AtomEnum::CARDINAL, 0, 1)?;
    if let Some(reply) = gone_is_none(pid.reply())?
        && let Some(pid) = reply.value32().and_then(|mut values| values.next())
        && let Ok(path) = std::fs::read_link(format!("/proc/{pid}/exe"))
    {
        names.extend(path.to_str().map(str::to_owned));
    }
    Ok(names)
}

fn gone_is_none<T>(reply: Result<T, ReplyError>) -> Result<Option<T>, Box<dyn Error>> {
    match reply {
        Ok(reply) => Ok(Some(reply)),
        Err(ReplyError::X11Error(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `WM_CLASS` holds an instance and a class name, each ending in a null.
fn class_names(value: &[u8]) -> Vec<String> {
    value
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .take(2)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_names_come_from_the_property() {
        assert_eq!(
            class_names(b"Navigator\0firefox\0"),
            ["Navigator", "firefox"]
        );
        assert_eq!(class_names(b"xterm\0"), ["xterm"]);
        assert_eq!(class_names(b"one\0two\0three\0"), ["one", "two"]);
        assert!(class_names(b"").is_empty());
        assert!(class_names(b"\0\0").is_empty());
        assert_eq!(class_names(b"bad\xff\0"), ["bad\u{fffd}"]);
    }
}

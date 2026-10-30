//! Deckly's tray icon: the notification area on Windows, the menu bar on
//! macOS and a StatusNotifierItem on Linux. Closing the window leaves Deckly
//! running here, so a desktop without a tray gets an error instead of an
//! icon nobody can see, and closing the window then quits.

use crate::logo;
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const OPEN: &str = "open";
const PREFERENCES: &str = "preferences";
const QUIT: &str = "quit";
/// Starts the menu id of a profile, followed by the device serial, a line
/// break and the profile id.
const PROFILE: &str = "profile\n";
/// Tray areas are 16-32 pixels tall; the system scales the icon down.
const ICON_SIZE: u32 = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Bring the window back.
    Show,
    Preferences,
    /// Show this profile on this device.
    Profile {
        serial: String,
        id: String,
    },
    /// What the menu lists may have changed.
    Refresh,
    Quit,
}

impl Command {
    fn from_menu_id(id: &str) -> Option<Self> {
        match id {
            OPEN => Some(Self::Show),
            PREFERENCES => Some(Self::Preferences),
            QUIT => Some(Self::Quit),
            _ => {
                let (serial, id) = id.strip_prefix(PROFILE)?.rsplit_once('\n')?;
                Some(Self::Profile {
                    serial: serial.to_owned(),
                    id: id.to_owned(),
                })
            }
        }
    }
}

/// A connected device as the menu lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub name: String,
    /// Id and name of each profile for this device's model.
    pub profiles: Vec<(String, String)>,
    /// The profile the device shows.
    pub active: Option<String>,
}

/// Follows the official tray menu; see
/// docs/clean-room/2026-10-30-tray-menu.md.
pub fn menu(devices: &[Device]) -> Result<Menu, String> {
    let menu = Menu::new();
    let add = |item: &dyn IsMenuItem| menu.append(item).map_err(|e| e.to_string());
    add(&MenuItem::with_id(OPEN, "Configure Deckly", true, None))?;
    add(&PredefinedMenuItem::separator())?;
    for device in devices {
        add(&MenuItem::new(label(&device.name), false, None))?;
        for (id, name) in &device.profiles {
            add(&CheckMenuItem::with_id(
                format!("{PROFILE}{}\n{id}", device.serial),
                label(name),
                true,
                device.active.as_ref() == Some(id),
                None,
            ))?;
        }
        add(&PredefinedMenuItem::separator())?;
    }
    add(&MenuItem::with_id(PREFERENCES, "Preferences…", true, None))?;
    add(&PredefinedMenuItem::separator())?;
    add(&MenuItem::with_id(QUIT, "Quit Deckly", true, None))?;
    Ok(menu)
}

/// Menus read `&` as the mark of a mnemonic; these names are the user's.
fn label(name: &str) -> String {
    name.replace('&', "&&")
}

/// Adds the icon. Call on the main thread once the event loop runs (macOS
/// needs both). Removing the icon is dropping the [`TrayIcon`].
pub fn start(
    on_command: impl Fn(Command) + Clone + Send + Sync + 'static,
) -> Result<TrayIcon, String> {
    available()?;
    let on_menu = on_command.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some(command) = Command::from_menu_id(&event.id.0) {
            on_menu(command);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            on_command(Command::Show);
        }
    }));
    let logo = logo::icon(ICON_SIZE);
    let icon = Icon::from_rgba(logo.rgba, logo.width, logo.height).map_err(|e| e.to_string())?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu(&[])?))
        .with_menu_on_left_click(false)
        .with_tooltip("Deckly")
        .with_icon(icon)
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
fn available() -> Result<(), String> {
    Ok(())
}

/// The tray library reports success on a desktop without a tray, such as
/// GNOME without its AppIndicator extension, so ask D-Bus first.
#[cfg(target_os = "linux")]
fn available() -> Result<(), String> {
    use zbus::blocking::{Connection, fdo::DBusProxy};
    use zbus::names::BusName;

    let connection = Connection::session().map_err(|e| e.to_string())?;
    let bus = DBusProxy::new(&connection).map_err(|e| e.to_string())?;
    let watcher = BusName::try_from("org.kde.StatusNotifierWatcher").map_err(|e| e.to_string())?;
    if bus.name_has_owner(watcher).map_err(|e| e.to_string())? {
        Ok(())
    } else {
        Err("this desktop has no tray".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Device {
        Device {
            serial: "A1\nB2".into(),
            name: "Desk".into(),
            profiles: vec![
                ("one".into(), "Rock & Roll".into()),
                ("two".into(), "Work".into()),
            ],
            active: Some("two".into()),
        }
    }

    #[test]
    fn menu_ids_turn_into_commands() {
        assert_eq!(Command::from_menu_id(OPEN), Some(Command::Show));
        assert_eq!(Command::from_menu_id(QUIT), Some(Command::Quit));
        assert_eq!(
            Command::from_menu_id(PREFERENCES),
            Some(Command::Preferences)
        );
        assert_eq!(Command::from_menu_id("something else"), None);
        assert_eq!(Command::from_menu_id("profile\nno-separator"), None);
        let device = device();
        let id = format!("{PROFILE}{}\n{}", device.serial, device.profiles[0].0);
        assert_eq!(
            Command::from_menu_id(&id),
            Some(Command::Profile {
                serial: device.serial,
                id: "one".into(),
            })
        );
    }

    // shortcut: muda needs the main thread on macOS, which tests do not get.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn menu_lists_each_device_with_its_active_profile_checked() {
        let items = menu(&[device()]).unwrap().items();
        assert_eq!(items.len(), 9);
        let header = items[2].as_menuitem().unwrap();
        assert_eq!(header.text(), "Desk");
        assert!(!header.is_enabled());
        let first = items[3].as_check_menuitem().unwrap();
        assert_eq!(first.text(), "Rock && Roll");
        assert!(!first.is_checked());
        assert!(items[4].as_check_menuitem().unwrap().is_checked());
        assert_eq!(items[6].as_menuitem().unwrap().text(), "Preferences…");
        assert_eq!(items[8].as_menuitem().unwrap().text(), "Quit Deckly");
    }
}

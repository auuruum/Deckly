//! Media control. A simulated media key does nothing on a desktop that binds
//! no action to it, as on a bare Hyprland or Sway, so players are asked
//! directly over MPRIS (the D-Bus interface Linux players share), and the
//! sound server is asked for the volume. `Ok(false)` means nothing answered.

use crate::os::MediaKey;
use std::process::{Command, Stdio};
use zbus::blocking::{Connection, Proxy, fdo::DBusProxy};

const PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

pub fn control(key: MediaKey) -> Result<bool, String> {
    Ok(match key {
        MediaKey::PlayPause | MediaKey::Next | MediaKey::Previous | MediaKey::Stop => player(key),
        MediaKey::VolumeUp | MediaKey::VolumeDown | MediaKey::Mute => volume(key),
    })
}

/// The MPRIS method for a media key.
fn method(key: MediaKey) -> Option<&'static str> {
    match key {
        MediaKey::PlayPause => Some("PlayPause"),
        MediaKey::Next => Some("Next"),
        MediaKey::Previous => Some("Previous"),
        MediaKey::Stop => Some("Stop"),
        _ => None,
    }
}

/// Which player gets the key: one playing now, else a paused one, else the
/// first. `statuses` keeps bus order.
fn chosen(statuses: &[(String, String)]) -> Option<&str> {
    let with = |wanted: &str| {
        statuses
            .iter()
            .find(|(_, status)| status == wanted)
            .map(|(name, _)| name.as_str())
    };
    with("Playing")
        .or_else(|| with("Paused"))
        .or_else(|| statuses.first().map(|(name, _)| name.as_str()))
}

fn players(connection: &Connection) -> Option<Vec<(String, String)>> {
    let names = DBusProxy::new(connection).ok()?.list_names().ok()?;
    let mut names: Vec<String> = names
        .iter()
        .map(|name| name.as_str())
        // `playerctld` only repeats the other players.
        .filter(|name| name.starts_with(PLAYER_PREFIX) && !name.ends_with(".playerctld"))
        .map(str::to_owned)
        .collect();
    names.sort();
    let status = |name: String| {
        let status = Proxy::new(connection, name.as_str(), PLAYER_PATH, PLAYER_INTERFACE)
            .and_then(|p| p.get_property::<String>("PlaybackStatus"))
            .unwrap_or_default();
        (name, status)
    };
    Some(names.into_iter().map(status).collect())
}

fn player(key: MediaKey) -> bool {
    let call = || -> Option<()> {
        let method = method(key)?;
        let connection = Connection::session().ok()?;
        let statuses = players(&connection)?;
        let name = chosen(&statuses)?;
        let proxy = Proxy::new(&connection, name, PLAYER_PATH, PLAYER_INTERFACE).ok()?;
        proxy.call_method(method, &()).ok()?;
        Some(())
    };
    call().is_some()
}

/// Volume step of one key press, in percent.
const STEP: u32 = 5;

/// The commands that change the volume, in the order to try them: PipeWire,
/// PulseAudio, ALSA.
fn volume_commands(key: MediaKey) -> [Vec<String>; 3] {
    let words = |line: String| line.split(' ').map(str::to_owned).collect();
    let (wpctl, pactl, amixer) = match key {
        MediaKey::VolumeUp => (
            format!("wpctl set-volume -l 1 @DEFAULT_AUDIO_SINK@ {STEP}%+"),
            format!("pactl set-sink-volume @DEFAULT_SINK@ +{STEP}%"),
            format!("amixer -q sset Master {STEP}%+"),
        ),
        MediaKey::VolumeDown => (
            format!("wpctl set-volume @DEFAULT_AUDIO_SINK@ {STEP}%-"),
            format!("pactl set-sink-volume @DEFAULT_SINK@ -{STEP}%"),
            format!("amixer -q sset Master {STEP}%-"),
        ),
        _ => (
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle".to_owned(),
            "pactl set-sink-mute @DEFAULT_SINK@ toggle".to_owned(),
            "amixer -q sset Master toggle".to_owned(),
        ),
    };
    [words(wpctl), words(pactl), words(amixer)]
}

fn volume(key: MediaKey) -> bool {
    volume_commands(key).iter().any(|words| {
        let Some((program, args)) = words.split_first() else {
            return false;
        };
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    #[test]
    fn the_playing_player_gets_the_key() {
        let statuses = list(&[("a", "Stopped"), ("b", "Paused"), ("c", "Playing")]);
        assert_eq!(chosen(&statuses), Some("c"));
    }

    #[test]
    fn a_paused_player_wins_over_a_stopped_one() {
        let statuses = list(&[("a", "Stopped"), ("b", "Paused")]);
        assert_eq!(chosen(&statuses), Some("b"));
        assert_eq!(chosen(&list(&[("a", "Stopped")])), Some("a"));
        assert_eq!(chosen(&[]), None);
    }

    #[test]
    fn every_playback_key_has_a_method() {
        assert_eq!(method(MediaKey::PlayPause), Some("PlayPause"));
        assert_eq!(method(MediaKey::Stop), Some("Stop"));
        assert_eq!(method(MediaKey::VolumeUp), None);
    }

    #[test]
    fn volume_commands_stay_within_their_tool() {
        let up = volume_commands(MediaKey::VolumeUp);
        assert_eq!(up[0][0], "wpctl");
        assert!(up[0].contains(&"5%+".to_owned()));
        assert!(up[1].contains(&"+5%".to_owned()));
        let mute = volume_commands(MediaKey::Mute);
        assert!(mute.iter().all(|c| c.last().is_some_and(|w| w == "toggle")));
    }
}

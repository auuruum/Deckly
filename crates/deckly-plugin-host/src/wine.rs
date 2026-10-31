//! Windows plugins on Linux and macOS, through Wine.
//!
//! Wine is the compatibility layer: it runs the plugin's own `.exe`, whose
//! WebSocket connection to Deckly works as on Windows. Deckly does not ship
//! it. Whether a plugin works depends on what it asks of Windows; one that
//! fails shows why in its log, and the rest of Deckly is not affected.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// A Wine that sets up its prefix on the first run is slow to start.
pub const REGISTER_DEADLINE: Duration = Duration::from_secs(180);

const NAMES: [&str; 2] = ["wine", "wine64"];
/// Where the common macOS installs keep Wine outside `PATH`.
const MAC_FOLDERS: [&str; 5] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/Applications/Wine Stable.app/Contents/Resources/wine/bin",
    "/Applications/Wine Staging.app/Contents/Resources/wine/bin",
    "/Applications/Wine Devel.app/Contents/Resources/wine/bin",
];

/// What a plugin's status says when Wine is missing.
pub const MISSING: &str = "needs Wine to run this Windows program";

fn find_in(folders: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    folders
        .flat_map(|dir| NAMES.map(|name| dir.join(name)))
        .find(|path| path.is_file())
}

/// The system's Wine.
pub fn find() -> Option<PathBuf> {
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let extra = cfg!(target_os = "macos")
        .then_some(MAC_FOLDERS.map(PathBuf::from))
        .into_iter()
        .flatten();
    find_in(path.into_iter().chain(extra))
}

/// The arguments that run a plugin's program under Wine.
pub fn arguments(program: PathBuf, plugin_args: &[OsString]) -> Vec<OsString> {
    let mut args = vec![program.into_os_string()];
    args.extend_from_slice(plugin_args);
    args
}

/// Plugins get a Wine prefix of their own in the runtimes folder, so they
/// stay away from the user's prefix and add nothing to the desktop's menus.
pub fn configure(command: &mut Command, runtimes: &Path) {
    let prefix = runtimes.join("wine");
    let _ = std::fs::create_dir_all(runtimes);
    command
        .env("WINEPREFIX", prefix)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "winemenubuilder.exe=d");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_wine_in_the_first_folder_that_has_it() {
        let root = std::env::temp_dir().join(format!("deckly-wine-{}", std::process::id()));
        let (empty, first, second) = (root.join("a"), root.join("b"), root.join("c"));
        for dir in [&empty, &first, &second] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(first.join("wine64"), "").unwrap();
        std::fs::write(second.join("wine"), "").unwrap();
        let folders = [empty.clone(), first.clone(), second.clone()];
        assert_eq!(find_in(folders.into_iter()), Some(first.join("wine64")));
        assert_eq!(find_in([empty].into_iter()), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_program_comes_before_the_plugin_arguments() {
        let args = arguments("/p/tool.exe".into(), &["-port".into(), "1".into()]);
        assert_eq!(args, ["/p/tool.exe", "-port", "1"]);
    }

    #[test]
    fn plugins_get_a_prefix_of_their_own() {
        let mut command = Command::new("wine");
        let runtimes = std::env::temp_dir().join(format!("deckly-rt-{}", std::process::id()));
        configure(&mut command, &runtimes);
        let prefix = command
            .get_envs()
            .find(|(name, _)| *name == "WINEPREFIX")
            .and_then(|(_, value)| value);
        assert_eq!(prefix, Some(runtimes.join("wine").as_os_str()));
        let _ = std::fs::remove_dir_all(runtimes);
    }
}

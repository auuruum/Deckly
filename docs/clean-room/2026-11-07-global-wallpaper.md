# Global wallpaper

Follows [Key appearance](2026-11-05-key-appearance.md), which added the
profile default, and [Wallpaper across all keys](2026-11-04-key-wallpaper.md).

- Date: 2026-11-07.
- Method: Deckly's own settings, profile, renderer, runtime, backup and
  editor code; synthetic pictures made with ImageMagick. The official app
  was not run and its documentation was not consulted again for this
  step. No proprietary code, profiles or assets were used.
- System: Linux/CachyOS (Hyprland, the app under XWayland). Devices were
  not opened (`DECKLY_NO_DEVICES`); no device or firmware was involved.

## Observed

- Nothing new about the official app. The earlier note records Elgato's
  description of a profile default (**Use current as default**); it does
  not describe a wallpaper shared by all profiles.

## Inferred

- None about the official app.

## Unknown

- Whether the official app has a wallpaper shared by all profiles, where
  it would be set, and how a profile would turn it off.
- How such a setting would be stored or exported. Deckly imports no
  wallpaper from official profiles or backups.
- How the global wallpaper looks on a real device, and on Windows and
  macOS.

## Deckly decisions

- The settings hold one optional global wallpaper, with the same picture
  path, zoom and position as a profile or page wallpaper. Its position is
  relative to the picture, so each model shows it across its own keys,
  cut by that model's panel and key layout like any other wallpaper.
- The order is global, profile, page or folder; the nearest level that
  says something wins. Each level below global either inherits, uses its
  own picture or turns wallpaper off. A profile that turns it off hides
  the global picture; its pages and folders can still use their own. A
  page or folder that turns it off shows none. One function,
  `resolve_wallpaper`, decides this for the device runtime, the canvas,
  the key inspector and the Wallpaper window.
- Profiles store the opt-out as `wallpaper_disabled`, written only when
  set. Older settings and profile files load without the new fields and
  behave as before: no global wallpaper, profiles inheriting it.
- Preferences > Style shows a small picture of the global wallpaper with
  **Choose…**/**Edit…** and **Remove**. The Global Wallpaper window lets the
  user choose a picture, drag and zoom it, remove it, and preview it on
  any model. Changes apply at once; a drag is saved when the mouse is let
  go. The Wallpaper window's Profile tab says when the profile uses the
  global picture and offers **Use Global** and **No Wallpaper**.
- Chosen pictures go through the existing wallpaper decoding and limits
  and are saved as PNG in Deckly's `wallpapers` folder, not in a profile's
  images. After a saved change of picture, earlier pictures in that folder
  are deleted.
- Backups carry the global picture under `wallpapers/` and restore it
  only from there. A profile export carries the profile's own opt-out but
  never the global picture.
- A picture that cannot be read leaves the keys as they were and shows a
  warning in Preferences and in the Global Wallpaper window.

## Validation

- Tests: `the_nearest_wallpaper_setting_wins`,
  `profiles_follow_the_global_wallpaper_unless_they_opt_out`,
  `older_profiles_follow_the_global_wallpaper`,
  `profile_wallpaper_respects_page_and_folder_overrides_after_saving`
  (profile); `round_trip`, `older_settings_files_have_no_global_wallpaper`,
  `the_global_wallpaper_is_brought_within_limits` (settings);
  `global_wallpapers_have_their_own_folder_and_old_ones_are_pruned`
  (store); `the_global_wallpaper_goes_into_the_backup_and_comes_back`,
  `an_exported_profile_carries_no_global_wallpaper` (backup).
- Run on Linux with a throwaway configuration folder: the global picture
  showed on an inheriting page and inside its folder; a profile's
  **No Wallpaper** hid it while a page with its own picture kept it; a
  page and a folder opted out; **Use Global** brought it back. Zooming and
  dragging in the Global Wallpaper window changed the canvas tiles to
  match and were saved to the settings file. The preview showed the MK.2
  and XL layouts. **Remove** cleared the setting and deleted the picture.
  A corrupt picture showed the warnings and left the keys black.
- Not verified: choosing a picture through the system file dialog (it
  opened but was not driven), a real device, Windows and macOS.

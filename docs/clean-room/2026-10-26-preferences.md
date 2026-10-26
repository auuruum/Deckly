# The official app's Preferences window

- **Date:** 2026-10-26
- **Target:** Stream Deck app for Windows 7.6.0.23012, `StreamDeck.exe`
  SHA-256 `2963DF27B13F97E5736E3F931348D89D2F693109F3CB9DC3C9899300E35EA9E3`
- **Host:** Windows 10 Pro 22H2, display at 100% scale
- **Device:** Stream Deck MK.2 (15 keys), firmware 1.02.000
- **Methods:**
  1. Runtime observation: seven screenshots taken by the owner (the main
     window with the preferences button, then each Preferences tab). Colours
     were sampled from them with a small pixel script; the screenshots are not
     stored in the repository.
  2. Runtime observation through Windows UI Automation: the owner's running
     app was asked to open its Preferences window (`streamdeck://open/preferences`),
     tabs were selected programmatically and the control tree was read (control
     type, visible name, object name, screen rectangle, current value). No
     setting was changed. Opening the drop-down lists through UI Automation
     did not work (they expose only Invoke and Value), so their item lists were
     not observed at runtime.
  3. Static inventory of printable identifiers and short UI strings in
     `StreamDeck.exe` with a small Python scan. No code was decompiled.
  4. Names of values the app keeps under
     `HKCU\Software\Elgato Systems GmbH\StreamDeck` (names only; encrypted
     plugin and account values were not read or decoded).
- **Implemented in:** `crates/deckly-core/src/settings.rs`,
  `crates/deckly/src/prefs.rs`, `crates/deckly/src/runtime.rs`,
  `crates/deckly/src/os/autostart.rs`

## Observed: window

- A separate top-level window titled "Preferences - <app name>", client area
  740 × 489 px, opened by the gear button at the right end of the main
  window's device header. It is a Qt tab widget (`ESDPreferenceWindow`,
  one `ESDPreferenceTab<Name>` class per tab).
- Tabs, in order: General, Devices, Accounts, Profiles, Plugins, Style. Each tab
  is 91 × 25 px; the first starts 7 px from the left and 10 px from the top.
- Colours: tab strip `#191919`; selected tab and page body `#303030`; tab text
  `#969696`, selected tab text `#E6E6E6`. Form labels are right-aligned in a
  182 px column starting 20 px from the left, `#969696`; values start 224 px
  from the left, `#E6E6E6`. Rows are 30 px tall (checkbox rows 22 px).
- Push buttons have no fill and a 1 px `#969696` border with `#E6E6E6` text.
  Checked check boxes and the on/off switch are blue (`#367AF6`); a selected
  list row is `#0078FF`. Text fields and drop-downs are `#3D3D3D`.
- Devices and Profiles draw their content in a framed panel (`#292929`, 1 px
  `#222222` border) 20 px inside the page.

## Observed: General

Rows from the top (y relative to the client area): Software Version (value and
"Check for Updates…" button), "Automatically check for updates" check box,
Startup: "Open automatically at login" check box, Dialog Warnings: a button
that resets "Don't Ask Me" warnings, a help link, then a separate block for an
AI integration toggle and its help link. A centred line about the UI toolkit
sits at the bottom. Registry value names `auto_launch_enabled` and
`background_check_for_update_disabled` match the two check boxes.

## Observed: Devices

- Rows inside the panel, 36 px apart: Device Name (text field, 163 px wide,
  then an "Enabled" switch), Visual Orientation (drop-down), Sleep After
  (drop-down and "Set Screensaver…"), Brightness (dim-sun icon, slider
  0–100 step 1, bright-sun icon), Firmware Version (value and "Update
  Firmware…"), Serial Number. An "Advanced…" button sits in the bottom-right
  corner.
- The device name typed here is what the main window shows as the device
  title ("Stream Deck 20" in the screenshots instead of the model name).
- Per-device values are kept in one map per device serial; names seen:
  `DeviceName`, `map_dev_enabled`, `map_dev_orientation`, `map_dev_sleep_time`,
  `map_dev_brightness`, `map_dev_sleep_wallpaper_path`, and the preferred
  (default) profile `ESDProfilesPreferred`.
- Short strings stored next to the Devices tab layout: "Standard",
  "Upside Down", "90", "180", "270", "Never", "%1 minutes", "%1 hour",
  "%1 hours", "No device found".
- Per-model variants of the device panel exist (`ESDPreferenceDeviceUSB`,
  `…USBPedal`, `…VSD`, …), and a device picker type is referenced.

## Observed: Profiles

- Left: a list of the current device's profiles (rows 32 px, list 220 px
  wide) with an expand arrow per profile (its pages) and a drag handle; "+"
  and "−" buttons and an options button under it.
- Right: an application picker for automatic switching, then
  "Make this my default profile" with an explanation that this profile shows
  when no application-specific profile applies.
- Renaming profiles and pages happens in this list (`onProfileNameChanged`,
  `onPageNameChanged`).

## Observed: Style

- Multi Action: "Show progress overlay" check box, then a divider.
- Global Font Settings, Keys: "Show Title" check box, a three-way title
  position control (bottom, middle, top), a font drop-down ("Default"), a size
  field in px with up/down buttons (12 px shown), bold/italic/underline
  toggles, a colour button and a 78 px key preview.
- Dials: "Show Title" and a colour button.
- "Apply" and "Revert" buttons: changes take effect only on Apply.
- A `reset_font` icon exists in the resources, which suggests a key can go
  back to the global font after being customised.

## Inferred

- The global title style is the default for keys whose title style was not
  customised, and Apply updates every such key.
- "Enabled" off makes the app ignore that device.
- "Upside Down" turns the key layout by 180° so the device can be used
  upside down; "90/180/270" belong to other device families.
- Sleep minute choices are numbers in code; the exact list was not seen.

## Checked

- Unit tests: settings defaults, round trip and limits, the upside-down key
  mapping, forgetting a deleted profile, old profiles' title styles following
  the global style, key image hashes following the global style, upside-down
  device images, and the login entry files.
- Deckly's Preferences window was run with `DECKLY_NO_DEVICES=1` on Windows
  and each tab captured: General, Profiles, Style and the Devices tab's
  "No device found" state match the layout above. Style's Apply/Revert
  state was exercised. The Devices tab with a device connected, sleep,
  orientation and the open-at-login switch were not tried at runtime.
- Linux and macOS: compile checks only.

## Unknowns and next steps

1. The item lists of Visual Orientation and Sleep After (open each list and
   take a screenshot).
2. What a disabled device shows, and whether sleeping clears the keys or only
   turns the backlight off.
3. Whether the first key press on a sleeping device also runs its action.
4. Whether "Make this my default profile" also changes which profile shows at
   startup when one was picked by hand before closing the app.

## Design choices in Deckly (not observed)

- Deckly shows General, Devices, Profiles and Style. Accounts, Plugins,
  updates, the AI integration and firmware updates are left out until
  features behind them exist. Orientation, the screensaver and the standby
  screen (Advanced…) are covered in
  [2026-10-26-orientation-screensaver-standby.md](2026-10-26-orientation-screensaver-standby.md).
- Sleep After offers Never, 1, 2, 5, 10, 15 and 30 minutes, 1, 2 and 4
  hours.
- A disabled device is returned to its own idle screen and its keys are
  ignored. A sleeping device turns its backlight off; the first key press
  only wakes it.
- A device's default profile is shown when the device connects.
- The Style tab covers show title, position, size and colour; Deckly has one
  title font, so font family and bold/italic/underline are not offered.
  The inspector gets a button that makes a key follow the global style again.
- The General tab shows Deckly's version, the open-at-login switch, the
  configuration folder and the unofficial notice.

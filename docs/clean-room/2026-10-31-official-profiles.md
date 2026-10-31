# Official profiles, backups and plugin folders

- **Date:** 2026-10-31
- **Target:** Stream Deck app for Windows 7.6.0.23012, `StreamDeck.exe`
  SHA-256 `2963DF27B13F97E5736E3F931348D89D2F693109F3CB9DC3C9899300E35EA9E3`
- **Host:** Windows 10 Pro 22H2
- **Device:** Stream Deck MK.2 (15 keys)
- **Methods:**
  1. Reading the owner's own backup file (SHA-256
     `4AC3A3D3AA78C2F9DC38C6E1444DE9F14E4154A10A6D0AAF02A7786B02EBA746`) and
     folders under `%APPDATA%\Elgato\StreamDeck`: structure, field names and
     value types. Nothing of it is stored in the repository; tests use
     invented data.
  2. Reading the manifests of the built-in action plugins and the default
     profile files in the install folder: action UUIDs and model codes.
  3. Listing strings in `StreamDeck.exe` and the order in which its code loads
     them, with a small script of ours. Nothing was executed in the target and
     no code was copied.
  4. Reading the names and types of registry values under
     `HKCU\Software\Elgato Systems GmbH\StreamDeck`, without printing them.
  5. Qt's public documentation for key codes; Elgato's public SDK documentation
     for the plugin folder on macOS.
- **Implemented in:** `crates/deckly/src/official.rs`,
  `crates/deckly/src/transfer.rs`, `crates/deckly/src/backup.rs`,
  `crates/deckly/src/prefs.rs`

## Observed

### Files

- A `.streamDeckProfilesBackup` file is a zip archive holding the same tree as
  the data folder's `ProfilesV3`: `Profiles/<UUID>.sdProfile/` per profile, and
  a `Resources/manifest.json`. Checked by
  `transfer::tests::a_backup_file_is_read_with_its_pages_and_only_its_own_pictures`.
- A `.streamDeckProfile` file is a zip archive with one such folder, at the top
  level or under `Profiles/`. It may hold a `manifest.json.bak` and resources.
- A profile folder holds a `manifest.json`, and under `Profiles/` one folder per
  page with its own `manifest.json` and `Images/`.
- The profile manifest has `Name`, `Version` ("3.0"), `Device` (`Model`, `UUID`)
  and `Pages` (`Current`, `Default`, `Pages`). Page ids are written in lowercase.
  Plugins that installed a profile leave `InstalledByPluginUUID` and
  `PreconfiguredName`.
- `Pages.Pages` lists the pages in order. Pages not on the list are folders, and
  the `Default` page, which was never on the list and empty in all eight of the
  owner's profiles. Checked by
  `official::tests::pages_and_folders_become_pages_and_folders`.
- Page folders on disk and in backups are the page UUID in capitals. Inside
  `.streamDeckProfile` files they are 27 characters: the UUID's 128 bits in 5-bit
  digits from `0-9A-T`, `V`, `W` (no `U`), then `Z`. This matched all three pages
  of the file it was worked out on. Checked by
  `official::tests::folder_names_decode_to_uuids`.

### Devices

- A real device's `UUID` reads `@(1)[4057/128/<serial>]`: vendor and product id
  in decimal, then the serial. Files meant for any device hold another UUID or
  an empty string.
- `Model` codes seen in profiles for a named product: `20GBA9901` MK.2,
  `20GAA9902` the 15-key model, `20GAI9901` Mini, `20GAT9902` XL, `20GBJ9901`
  Neo, `20GBD9901` +, `20GBX9901` + XL. Other values include `AI Stream Deck`, a
  virtual device with its own default profile, and Corsair product names.
- A page's `Controllers` is a list with `Type` `Keypad`, `Encoder` or
  `{Invalid}`, and `Actions`: an object keyed by `column,row`, or `null`.

### Keys

- An action has `UUID`, `Name`, `Settings`, `State`, `States`, `LinkedTitle`,
  `ActionID` and `Resources`, and may have `Plugin` (`Name`, `UUID`, `Version`),
  `Actions`, `MultiActionDelays`, `OverrideState` and `AutoExit`. Actions inside
  a Multi Action often have no `Plugin`.
- A state may hold `Title`, `Image` (a path relative to the page folder, such as
  `Images/<27 characters>.png`; PNG and WebP were seen), `FontFamily`,
  `FontSize`, `FontStyle`, `FontUnderline`, `OutlineThickness`, `ShowTitle`,
  `TitleAlignment` and `TitleColor`.
- Two pictures named by keys of the owner's profiles were absent from the
  backup and the data folder, so a missing picture is normal input.
- Multi Actions hold a list of lists: `Actions[n].Actions`. Key Logic holds its
  three actions directly in `Actions`.
- Settings of built-in actions, with field names as stored:
  - Hotkey: `Hotkeys`, four entries; unused ones have Qt's "unknown" key code.
    Each has `QTKeyCode`, `VKeyCode`, `NativeCode`, `KeyModifiers`, `KeyCtrl`,
    `KeyShift`, `KeyOption`, `KeyCmd`.
  - Open: `path`, which can hold quotes and arguments.
  - Open Application: `exec`, `args`, `bundle_path` and others.
  - Website: `path`, `openInBrowser`, `browser`.
  - Text: `pastedText`, `isSendingEnter`, `isTypingMode`.
  - Delay: `delay` in milliseconds.
  - Create Folder: `ProfileUUID` holds the page.
  - Switch Profile: `ProfileUUID`, `DeviceUUID`, `PageIndex`.
  - Go to Page: `PageIndex`.
  - Multimedia: `actionIdx`, sometimes absent.
- Checked by `official::tests::system_actions_are_translated`,
  `multi_actions_keep_their_lists_delays_and_state` and
  `key_logic_holds_one_action_per_press`.

### Plugins and their settings

- Installed plugins are `<id>.sdPlugin` folders under `Plugins` in the data
  folder. A folder there can be a link: a plugin under development was a
  junction to its project. Checked by
  `transfer::tests::plugin_folders_are_sorted_into_what_can_be_copied`.
- In 22 of the 48 installed plugins `manifest.json` starts with `ELGATO` and is
  encrypted. It was the only such file in each of them.
- The Elgato tutorial plugin's `Name` is a single space, which Deckly's manifest
  reader refuses.
- A plugin's global settings are registry values named after the plugin, twice
  over. They begin `@ByteArray(`, and what follows is not JSON and differs in its
  first byte from plugin to plugin. They were not decoded further.

## Inferred

- `PageIndex` counts from 1: keys in three profiles of two pages hold 2.
- `actionIdx` follows the order the code loads the list in: Previous Track, Play
  / Pause, Next Track, Stop, Mute, Increase Volume, Decrease Volume. A missing
  value means Play / Pause. The owner's keys fit this for 0, 1 and 2: `0`, none,
  `2` sit side by side in three layouts, and the action's default image is Play /
  Pause. Values 3 to 6 rest on the order alone. Checked by
  `official::tests::media_keys_follow_the_official_order`.
- `20GAA9901`, `20GAI9902`, `20GAT9901` stand for the first 15-key model, Mini v2
  and the first XL, from their place in a list of model codes in the executable.
- `20GBF9901`, `20GBL9901`, `20GBO9901` and `20GBD9901LL` are other hardware;
  which is unknown.
- `State` is the state the key was last in, since it is what changes while a
  switch is used.

## Deckly's own choices

- An imported profile keeps the official profile's UUID, in lowercase, so
  importing again replaces it, and Switch Profile keys lead to the right profile.
  The import dialog says how many profiles it will replace.
- The device id decides the model; the `Model` code is only used without one. A
  profile for a model Deckly does not know is left out and named in the result.
- A key's title and picture come from its current state, or from the first state
  that has one, since a Deckly key has one of each. Such keys are counted in the
  result.
- Plugin actions keep their UUID and settings whether or not the plugin is
  installed, so the key works once it is. Official built-in actions without a
  Deckly counterpart (Close, Timer, Play Audio and others) are kept the same way
  and listed in the result.
- A website address without a scheme gets `https://`.
- Pictures are copied to `images/<profile>/<page>/` in the settings folder and
  removed with the profile.
- Only files listed from the source are opened, so a picture path in a manifest
  cannot reach outside it. Checked by
  `backup::tests::restoring_pictures_stays_inside_the_profile` for Deckly's own
  backups.
- Encrypted plugins are not decrypted and not copied: that would bypass an access
  control (`AGENTS.md`). Their keys stay in place and the result names the plugins.
- Global plugin settings are not imported, for the same reason.
- `.decklyBackup` is Deckly's own format, not based on the official one: a zip
  with `backup.json`, `settings.json`, `profiles/`, `images/`, `screensavers/`,
  `plugin-data/` and `plugins/`.

## Run with real data (2026-10-31, Windows 10 22H2)

- The owner's backup through a temporary test against an empty settings folder:
  7 of 8 profiles imported (the eighth is for `AI Stream Deck`), 716 keys, 150 of
  150 pictures decoded, each saved profile read back equal, all folder keys
  leading to a folder.
- The result written as a `.decklyBackup` (5.1 MB, no plugins in that run) and
  restored into a second empty settings folder: 7 profiles, 150 pictures decoded,
  none pointing outside the folder.
- The data folder scanned directly: 24 plugins copied into a temporary folder
  through the installer (287 MB), 22 reported encrypted, the tutorial reported
  unreadable. The linked plugin was found missing from this run and fixed after
  it; it is counted in the 25 the import dialog then showed.
- General tab, import dialog and result dialog seen in the running app, with
  `DECKLY_NO_DEVICES=1`.

## Unknowns and next steps

- Not run: an import or backup with plugins through the running app's plugin
  host, and restoring on Linux or macOS. Those platforms were compile-checked.
- The macOS data folder is taken from the public plugin documentation and
  assumed to hold `ProfilesV3` too. Not checked.
- `ProfilesV2` from older versions of the app is not read.
- Random Action's stored layout was not seen; it is read like a Multi Action, and
  a flat list of actions also works.
- Whether `PageIndex` of Switch Profile counts from 1. Deckly does not use it.
- Dial actions (`Encoder`) are counted and not imported.
- Icon packs, the soundboard's audio and animated pictures (first frame only).

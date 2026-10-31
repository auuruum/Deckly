# Profile list order, pages in the list, import and restore

- **Date:** 2026-10-31
- **Target:** Stream Deck app for Windows 7.6.0.23012, `StreamDeck.exe`
  SHA-256 `2963DF27B13F97E5736E3F931348D89D2F693109F3CB9DC3C9899300E35EA9E3`
- **Host:** Windows 10 Pro 22H2
- **Device:** Stream Deck MK.2 (15 keys), serial not recorded here
- **Methods:**
  1. The owner's screenshot of Preferences, Profiles tab, with the list open.
  2. The `Devices` registry value under
     `HKCU\Software\Elgato Systems GmbH\StreamDeck`, read as bytes. Only its
     structure and the profile ids of the owner's own profiles were used.
  3. The owner's profile folders in `%APPDATA%\Elgato\StreamDeck\ProfilesV3`.
  4. Strings of `StreamDeck.exe`, and a Ghidra session through REA over the code
     that uses a few of them (evidence ids `ev_7551a276…`, `ev_185b0fb6…`,
     `ev_7cca673c…`). Nothing was executed in the target and no code was copied.
- **Implemented in:** `crates/deckly-core/src/settings.rs`,
  `crates/deckly-core/src/profile.rs`, `crates/deckly/src/official.rs`,
  `crates/deckly/src/transfer.rs`, `crates/deckly/src/prefs.rs`

## Observed

### Profile order

- `Devices` is a Qt `@Variant(` value stored as UTF-16 with one unit per byte of
  a data stream. It holds a map per device; a device's map has `ESDProfilesInfo`
  with `ESDProfilesSorting`, `ESDProfilesPreferred` and `ESDProfilesExpanded`.
- A text in the stream is its length in bytes, then UTF-16 big-endian. A value is
  a 4-byte type, then the data; there is no null flag before the text. An earlier
  guess of a 5-byte header was refuted by the real value.
- `ESDProfilesSorting` is a comma-separated list of lowercase profile UUIDs. All
  seven ids of the MK.2 matched the order of the list in the owner's screenshot.
  Checked by `official::tests::the_profile_order_is_read_for_every_device`.
- A `.streamDeckProfilesBackup` holds no order: its only other file is
  `Resources/manifest.json`, and the profile manifests have no such field.

### The list

- The tab's list is a sortable tree view class. Its model has profiles with
  their pages below them. The executable has a `list_handle` picture, which fits
  the three-line mark on each profile row in the screenshot.
- Page rows in the screenshot have no such mark.
- The tab has a separate options button (`optionProfileButton`) under the list
  and a context menu. Menu entries include Rename, Duplicate, Export, Delete,
  Copy to, Add Page, Change Icon, Import.
- A page's details are a name field and a square icon button with a reset button
  beside it. A page without a name shows `Page %1`, with a picture
  `icon_page_number_<digit>` for each digit.

### Page files

- A page manifest has `Name` and `Icon`, both empty by default. On the owner's
  named page `Icon` is `Images/<27 characters>.svg`, relative to the page folder,
  a 25 by 24 white SVG. Checked by
  `official::tests::pages_keep_their_name_and_icon`.
- The app ships icon sets as `.sdIcons` folders under `PageIcons`, with a browser
  window for them. Those files are Elgato's and are not used.

### Import and restore

- The restore prompt warns that it replaces all profiles of all devices and
  cannot be undone. The importer has a step that deletes all profiles.
- Importing one profile goes through a separate path with a step that renames
  what it brings in.
- When a profile is cloned, its name loses any ending of the word "copy" and a
  number; the first free one of the plain name, the name with " copy", and the
  name with " copy 1", " copy 2" and so on is taken.
- A device with no profile gets one by a step named for its initial default
  profile.

## Inferred

- Dragging a row by its handle reorders it: the view signals a sorting change
  and the tab handles it. The drag itself was not performed in the official app.
- `ESDProfilesExpanded` lists open profiles: it named the profile open in the
  screenshot. One id was seen, so a list of several is a guess.
- `ESDProfilesPreferred` is not the "Make this my default profile" flag: it named
  a profile with no sign of being the default. What it stands for is unknown.
- The rename on single import follows the clone rule above. Only the clone's
  code was read.

## Deckly's own choices

- The official order is per device, and Deckly's profiles belong to a model. One
  list, `profile_order` in the settings, holds them all; dragging keeps other
  models' profiles in the places they hold. Profiles not on it follow, by name.
  Checked by `settings::tests::reordering_one_model_leaves_the_others_in_place`.
- Import is additive in Deckly, unlike the official restore. Imported profiles
  follow those already there, in the official order where the ids match, so a
  backup made on this computer gets it too. Only Windows has this setting read.
- A device with no profile gets an empty "Default Profile" at once, so importing
  on a clean install used to leave it beside the imported one of that name. After
  an import or a restore such a profile goes, if it is still empty and its model
  has another one. A plugin that installs a profile does not trigger this. Checked
  by `runtime::tests::an_empty_default_profile_gives_way_to_real_ones_of_its_model`.
- Change Icon opens a file dialog for SVG, PNG or JPEG, like a key's image. There
  is no icon library, since the official one cannot be copied. The path is stored
  as chosen, as key images are.
- The page strip of the main window shows a page's icon in place of its number,
  and its name as a tooltip. How the official window shows them was not checked.
- Which profiles are open in the list is not saved.

## Run with real data (2026-10-31, Windows 10 22H2)

A temporary test, not kept, against an empty settings folder holding one empty
"Default Profile", with the owner's data folder and registry value:

- 7 of 8 profiles imported; the empty profile was gone and one "Default Profile"
  remained, with 2 pages.
- Order: Default Profile, obs new, bebracup 2026, obs rust, rust, Rustplusplus,
  Profile 1, as in the screenshot.
- The Profiles tab was rendered with the running code through a headless egui
  harness: pointer events opened a profile, selected a page, typed in its name,
  reset its icon, and dragged a profile by its handle. The new order, name and icon
  were read back from the saved files. The page's SVG was copied and drawn.

## Unknowns and next steps

- Not seen in a real window: the drag with a real mouse, the file dialog of Change
  Icon, the page strip with an icon. Not run on Linux or macOS.
- Where macOS keeps the same setting.
- Icon library, Duplicate, Export, Add Page and the other menu entries of the list.
- Renaming imported profiles whose name is taken, when the other profile is not
  an empty placeholder.

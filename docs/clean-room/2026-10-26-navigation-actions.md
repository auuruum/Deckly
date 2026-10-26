# The official app's Navigation actions

- **Date:** 2026-10-26
- **Target:** Stream Deck app for Windows 7.6.0.23012, `StreamDeck.exe`
  SHA-256 `2963DF27B13F97E5736E3F931348D89D2F693109F3CB9DC3C9899300E35EA9E3`
- **Host:** Windows 10 Pro 22H2
- **Device:** Stream Deck MK.2 (15 keys)
- **Methods:**
  1. Runtime observation: a screenshot of the action list taken by the owner,
     with the Navigation category open. Not stored in the repository.
  2. Reading the manifests of the built-in action plugins in the install
     folder (`Plugins/com.elgato.streamdeck.page.sdPlugin`,
     `…profile.openchild…`, `…profile.backtoparent…`, `…profile.rotate…`,
     `…system.pagination…`): action names, UUIDs, tooltips, image file names.
     These follow the public plugin manifest format. No image was copied.
  3. Reading the owner's own profiles under `%APPDATA%\Elgato\StreamDeck\ProfilesV3`
     (structure and settings field names only).
  4. Static inventory of printable identifiers in `StreamDeck.exe` with a
     small Python scan (class and method names only). No code was decompiled.
  5. Public documentation: Elgato's help article on Pages
     (help.elgato.com, "Elgato Stream Deck Pages").
- **Implemented in:** `crates/deckly-core/src/profile.rs` (folders),
  `crates/deckly/src/actions.rs`, `crates/deckly/src/runtime.rs`,
  `crates/deckly/src/render.rs`, `crates/deckly/src/app.rs`

## Observed

- The Navigation category lists six actions in this order: Create Folder,
  Switch Profile, Previous Page, Next Page, Go to Page, Page Indicator. The
  category list icons are line drawings: a folder with a plus, a grid of dots,
  an arrow or caret in a square for the page actions, a number in a square for
  Page Indicator. Checked by
  `actions::tests::navigation_list_follows_the_official_order`.
- Page actions live in one built-in plugin; Page Indicator's key image file
  is an empty drawing, so the app draws the number itself. The public help
  article says Page Indicator shows the current page number and does nothing
  when pressed. Checked by `render::tests::page_indicator_shows_the_live_page`
  and `actions::tests::navigation_outcomes`.
- A separate "Page Indicator" action exists for the Stream Deck Neo's touch
  points (`Controllers: Neo`); it is not a key action and is not in this list.
- Create Folder and Parent Folder are two separate built-in actions. Parent
  Folder has no list entry or list icon; it is not in the action list.
- In the owner's profiles a folder is stored as its own page-like object next
  to the profile's pages but not listed among them. The folder key's settings
  hold the folder's id. Every folder observed has a Parent Folder key on the
  top-left key. Folders hold folder keys of their own (nesting observed one
  level deep). Checked by `profile::tests::new_folder_keys_get_a_folder_with_a_parent_key`.
- Identifier names show a Create Folder inspector view with an "open folder"
  button, a Parent Folder inspector view and controller with saved settings,
  and a Go to Page view with a page index.

## Inferred

- The Parent Folder settings identifiers (an "enabled" flag and a "delay")
  suggest an option to return to the parent folder automatically after a
  time. Not implemented.
- Opening a folder replaces the keys on the device with the folder's keys;
  Parent Folder returns to the page or folder it was opened from.

## Deckly's own choices

- Folders are one page of keys each, stored in the profile's `folders` list.
  A folder key's `folder` setting holds the id. Loading a profile repairs
  folders: a missing folder becomes an empty one, a folder reached by two keys
  is copied for the second, a key leading into its own enclosing folder gets
  an empty one, unreachable folders are dropped, and nesting and count are
  bounded (`MAX_FOLDER_DEPTH`, `MAX_FOLDERS`). Checked by the
  `profile::tests::*folder*` tests.
- The Parent Folder key can be moved but not cleared, overwritten, or copied.
- In the editor, double-clicking a folder key opens it, and a breadcrumb in
  place of the page strip shows the page and open folders; each part goes
  back to it.
- Next Page, Previous Page and Go to Page pressed inside a folder close the
  folders and go to that page.
- The editor and the device show the same page and folder, as with pages.

## Unknowns and next steps

- Whether the official app allows more than one page inside a folder.
- What Page Indicator shows inside a folder (Deckly shows the page the folder
  was opened from) and how it draws numbers above 9.
- What page actions do when pressed inside a folder in the official app.
- How the official app copies and pastes a folder key (Deckly copies the
  folder's keys and nested folders).
- Switch Profile's page and device choices (`PageIndex`, `DeviceUUID`
  settings) are not implemented yet.

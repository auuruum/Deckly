# Profile commands in Preferences

- Date: 2026-11-02
- Method: Deckly source inspection and Rust tests using invented profiles and
  pictures. No new inspection of the official application or its artifacts.
- Reference behavior: `2026-10-31-profile-list-and-pages.md`, especially its
  observed menu entries and clone naming rule. That entry records the target,
  device and firmware uncertainty for those observations.
- Hardware: none opened; the development launch used `DECKLY_NO_DEVICES=1`
  and an isolated `DECKLY_CONFIG_DIR`.

## Implemented and checked by tests

- Profiles have the same Rename, Duplicate, Export, Add Page, Delete and Import
  commands in the row context menu and the options button under the list.
  The icons use the existing egui-phosphor library.
- Duplicate assigns fresh profile, page and folder ids, remaps folder links,
  and preserves actions and settings. Pictures, including nested action images
  and page/folder icons, are copied into the new profile's image directory.
  Copying and exporting run outside the UI thread.
  Tests: `copies_keep_pages_actions_and_their_own_folder_links`,
  `a_duplicate_owns_page_folder_and_nested_action_pictures`.
- Add Page appends an empty page, expands its profile and selects the page in
  Preferences. It stops at `MAX_PAGES` (100); a save failure rolls the addition
  back. Existing runtime page and folder selections are preserved.
  Test: `adding_pages_preserves_existing_pages_and_stops_at_the_limit` checks
  data and limits; window selection behavior awaits manual checking.
- Export writes a `.decklyProfile` zip containing `profile.json` in Deckly's
  format and the referenced pictures under `images/<profile-id>/`. It contains
  no application settings or plugin installations. It is a Deckly format, not
  an official `.streamDeckProfile` export. Import reads both formats.
  A failed export leaves the previous destination intact and removes its temp
  file. Tests: `a_native_export_round_trips_pages_folders_settings_and_pictures`,
  `native_profiles_cannot_import_images_outside_their_folder`.
- Import keeps profile ids and the existing replacement warning for a repeated
  id. Name conflicts with different ids are resolved per device model, ignoring
  case and surrounding spaces. A trailing whole `copy`/`copy N` is removed
  when resolving a conflict; the first free base, `base copy`, `base copy 1`,
  etc. is chosen. Names stay within 64 characters without splitting Unicode.
  Conflicts between profiles in one import batch are included, while empty
  default placeholders still give way to imported profiles. The result names
  any renamed profiles.
  Tests: `free_names_use_copy_suffixes_and_respect_model_case_and_length`,
  `imports_resolve_names_within_a_batch_and_keep_ids_and_placeholders`.

## Inferred

- The existing clone naming observation is also used for names on import.
  Exact official single-profile import behavior was not newly observed.

## Unknown / manual verification

The owner asked to handle mouse testing themselves. No visual or mouse pass
is claimed. Check in a real window:

1. Open Preferences > Profiles; use both the row menu and options button to
   duplicate a profile, add a page, export and import it again.
2. Drag a profile by its handle, including across expanded profiles and near
   the list edges; reopen Deckly and check that the order persists.
3. Choose an SVG/PNG/JPEG through Change Icon; check the list, details and main
   window page strip, then reset it.
4. Import a different profile with a taken name, and repeat an import of the
   same id; check the renamed result and replacement warning respectively.

Linux/macOS runtime and hardware behavior were not tested in this task.

# Page management and icon workflow in Preferences

- Date: 2026-11-02.
- Sources/methods: the owner's screenshot and report of Deckly Preferences;
  Deckly source inspection and Rust tests with invented data. No official
  application artifacts were inspected for these changes.
- Device/firmware: not involved; no hardware behavior is claimed.

## Observed in Deckly

- The owner reported that right-clicking a page did not offer deletion.
  Source inspection confirmed that only profile rows had a context menu,
  page rows did not select on right click, and the minus button was disabled
  while a page was selected.

## Implemented: page deletion

- A page row selects on either mouse button and offers Delete Page from its
  context menu, the options button and the minus button. The last page cannot
  be deleted. Deletion requires confirmation in the selected page's details.
- Removal preserves a valid current page and open folders that still exist.
  Folders reachable only from the deleted page are removed. The main window's
  page strip uses the same model removal method.
- The edited profile is saved before runtime state changes. A save failure
  preserves both the profile and its runtime navigation state.
- Checks: `deleting_pages_removes_their_folders_but_preserves_the_last_page`,
  `deleting_a_page_preserves_live_selection_and_rolls_back_failed_saves`,
  `right_click_selects_the_page_and_delete_requires_confirmation` (egui pointer
  events, including the disabled last-page menu item).

## Implemented: icon alignment

- The owner's screenshot shows the drag handle glyph above the row's vertical
  center. These glyphs were positioned using the surrounding system font's
  line rectangle, which includes different whitespace than the icon's ink.
- The profile carets, drag handles, page numbers and Preferences icon buttons
  now center the visible glyph mesh. Non-square page pictures keep their aspect
  ratio and center in the list, details and main window page strip.
- Check: `page_controls_center_visible_glyphs_and_preserve_picture_aspect_ratio`
  checks visible bounds at 100%, 125%, 150% and 200% scale, plus a 2:1 picture.

## Manual verification

The owner previously asked to do real mouse testing themselves. Native window
interaction and Linux/macOS runtime are not claimed by these Rust tests.

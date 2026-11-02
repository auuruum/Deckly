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

## Implemented: shared image workflow

- Change Icon now opens the same Upload image, Icon library, Create image and
  Reset to default menu used for keys. It is available from the page's icon
  preview, its context menu and the options button under the list.
- Pages use the existing editor with page-specific labels, a destination
  snapshot, and a separate 852 by 742 window so its controls fit outside the
  smaller Preferences window. The library, layers and draft undo/redo are
  shared with key images. Reset restores the numbered page icon.
- Page compositions default to a transparent background. Key compositions
  retain their black default. Both expose a transparency toggle; transparency
  survives PNG export. Images are immutable PNGs owned by the profile and use
  the existing backup/export picture traversal.
- SVG upload is retained through bounded vector rasterization. Uploaded SVGs
  cannot read external files or allocate embedded bitmap images. Raster files
  keep the existing decoder limits and first-frame behavior.
- Cancel, closing Preferences, a changed selection, a deleted page or a changed
  page snapshot prevents stale results from applying. Saving a page icon
  validates the destination again and saves the edited profile before updating
  runtime data.
- Checks: `change_icon_offers_the_shared_library_and_creator_without_changing_the_page`,
  `page_icon_editor_uses_page_labels_transparency_and_guards_its_destination`,
  `page_icon_results_only_update_the_unchanged_destination_after_a_successful_save`,
  `page_icons_keep_transparency_through_composition_and_png_encoding`,
  `vector_uploads_fit_the_canvas_and_cannot_load_external_or_embedded_images`.

## Manual verification

The owner previously asked to do real mouse testing themselves. Native window
interaction and Linux/macOS runtime are not claimed by these Rust tests.

# Key title fonts

- Date: 2026-11-04.
- Sources/method: our earlier notes on the official app 7.6 Preferences
  window ([2026-10-26-preferences.md](2026-10-26-preferences.md), Style tab)
  and on official profile files
  ([2026-10-31-official-profiles.md](2026-10-31-official-profiles.md),
  state fields); the public Stream Deck SDK documentation of manifest states
  and of the `titleParametersDidChange` event; runtime observation of
  Deckly's own build. No shipped application binaries, fonts, other Stream
  Deck implementations or proprietary assets were used.
- System: Linux/CachyOS on Hyprland 0.57, 964 font faces in 317 families
  installed. The test editor used a temporary configuration folder and
  `DECKLY_NO_DEVICES=1`. The owner asked to stop GUI testing part way and
  will check the rest of the UI.
- Device/firmware: no hardware accessed.

## Observed

- The official Style tab offers, for keys: Show Title, title position, a font
  drop-down showing "Default", size in px, bold/italic/underline toggles and
  a colour (recorded 2026-10-26).
- Official profile states may hold `FontFamily`, `FontSize`, `FontStyle`,
  `FontUnderline`, `OutlineThickness`, `ShowTitle`, `TitleAlignment` and
  `TitleColor` (recorded 2026-10-31).
- The public SDK documentation describes the state font style as one of
  `Regular`, `Bold`, `Italic` and `Bold Italic`, and the title parameters a
  plugin receives as font family, size, style, underline, show title,
  alignment and colour.
- Deckly before this change drew every title in one bold font (DejaVu Sans
  Bold, Segoe UI Bold or Arial Bold, else egui's bundled font) and told
  plugins an empty font family and style and a white colour.
- In Deckly's Linux build, keys drawn with Default, Regular, Italic,
  Underline, DejaVu Serif, DejaVu Sans Mono italic, a missing "Verdana" and
  Noto Serif bold italic underlined each showed as asked; "Verdana" showed in
  the default font. The key's title popup showed the font menu with the
  installed families and the toggles, stayed open while the menu was used,
  and choosing Cantarell redrew the key.
- Cantarell is installed as one variable font file; the system font list
  (fontdb) reports it as a single face at its default weight.
- Scanning the system fonts took about 11 ms in a release build and 46 ms in
  a debug build, once each, on this machine with a warm disk cache. This is
  one measurement, not a general figure.

## Inferred

- An empty font family in the official format and the "Default" choice mean
  the app's own default font; Deckly maps both to its default.
- An empty or missing `FontStyle` keeps Deckly's default, which is bold so
  that old Deckly profiles look unchanged. Unknown spellings are read by
  whether they contain "bold" and "italic".
- `OutlineThickness` controls the dark edge around titles. Deckly draws a
  fixed edge and does not import it; the import report counts those titles.

## Unknown

- The official app's default font family, whether its default style is bold,
  and its list of offered families. Deckly lists what the system has.
- The meaning and range of `OutlineThickness` values.
- How the official app draws a family the computer does not have.
- Windows and macOS: the default font files named in `fonts.rs` and fontdb's
  system font scan were not run there; only Linux was.
- The Preferences > Style tab after its rows moved down, and typing to jump in
  the font menu, were not looked at in the running app (owner will check).

## Deckly decisions

- `TitleStyle` gets `font` (empty is the default font), `bold` (default on),
  `italic` and `underline`. Untouched values are not written, so files of
  older Deckly versions round-trip unchanged. Font names are trimmed, have
  control characters removed and are cut at 64 characters.
- Fonts come only from the system (through fontdb, already in the tree via
  resvg) or egui's bundled font. No font is copied from the official app or
  added to the repository. Font files over 32 MB, files that do not parse and
  unknown families fall back to the default font; nothing panics on a font.
- Variable fonts are set to weight 400 or 700 and to their italic or slant
  axis when asked. A face without bold is widened by a sub-pixel dilation of
  the title's coverage; one without italic is sheared by 0.21. A broken font
  that names a minimum above its maximum cannot cause a panic.
- Loaded faces are shared by all renderers, at most 32 at a time. The font
  menu's family list is built on a background thread so the UI never waits
  for the scan.
- The font menu jumps to the first family starting with what was typed within
  a second; one letter typed again goes through the families with it; Enter
  takes the highlighted family.
- The key hash includes the whole title style, so a key is drawn and sent
  again only when its font options change.

## Validation

- Tests: `title_styles_without_font_options_keep_their_look`,
  `font_options_round_trip`, `font_names_are_trimmed_and_limited`
  (deckly-core); `title_parameters_carry_the_font` (deckly-plugin-host);
  `every_variant_of_the_default_font_loads`,
  `unknown_families_fall_back_to_the_default_font`,
  `broken_and_oversized_font_data_is_refused`,
  `families_are_sorted_without_duplicates`,
  `missing_variants_are_drawn_bolder_or_slanted`,
  `font_options_change_the_pixels_and_the_hash`,
  `a_missing_font_family_draws_with_the_default_font`,
  `synthetic_bold_and_slant_stay_inside_the_key`,
  `title_fonts_come_from_the_states`,
  `the_report_names_fonts_this_computer_lacks_and_outlines`,
  `typing_jumps_to_font_names`, `typed_names_are_bounded` (deckly).
- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` pass on Linux.

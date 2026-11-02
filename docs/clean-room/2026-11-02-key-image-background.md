# Default key image background

- Date: 2026-11-02.
- Sources/method: the owner's three screenshots of Deckly and report about
  the hardware display; inspection of Deckly's own composition and renderer.
- System/model: Linux; Stream Deck MK.2 shown in the screenshots. Firmware
  unknown. No hardware accessed by the agent.
- Regression test:
  `default_icon_background_matches_builtin_previews_png_and_device_output`.

## Observed

- The owner showed that a library icon had a visible dark background next to
  built-in keys, and reported that black should leave only the glyph visible
  on the hardware.
- Built-in keys render on RGB `[0, 0, 0]`. New library/creator drafts instead
  used `[24, 26, 32]` (`#181A20`). The regression test failed with those exact
  differing pixels before the fix.

## Deckly decision and validation

- Default new drafts to pure black. Explicitly selected background colors
  stay supported, as checked by the existing composition test.
- The regression test checks the default against a built-in key, PNG
  persistence, key previews at multiple sizes and encoded output for every
  device model in the table. These are software checks, not hardware tests.
- Previously saved PNGs keep their existing colors. Selecting the icon again
  from the library creates a new image with the corrected default background;
  no heuristic recoloring of the user's existing pictures is performed.
- Checks run in the background. No desktop windows or hardware are opened,
  following the owner's request.
- `cargo fmt --all`, workspace Clippy with warnings denied,
  `cargo test --workspace` (313 passed, four existing manual tests ignored),
  and `cargo dev --no-run` pass. Windows GNU x86_64 and macOS aarch64
  editor/test targets compile-check with build-std on Linux.

## Unknown

- Visual confirmation on the physical device and native runtime behavior on
  Windows/macOS. No support claims are changed by this fix.

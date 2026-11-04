# Wallpaper across all keys

- Date: 2026-11-04.
- Sources/method: Deckly's own device table, screensaver cutting, renderer,
  runtime and editor code; our earlier notes on the official Customize Image
  window ([2026-10-26-orientation-screensaver-standby.md](2026-10-26-orientation-screensaver-standby.md)).
  No official app binaries, assets, profiles or other Stream Deck
  implementations were used.
- System: Linux/CachyOS. The owner asked for code and tests only in this
  step; the app was not run, and no device was attached.
- Device/firmware: none.

## Observed

- Deckly already cuts one picture into key images for screensavers: the
  picture is fitted to the model's panel (its screen where known, else the
  key grid with gaps of 5/12 of a key) and each key takes its rectangle, row
  by row as the user sees the keys.
- Before this change a key drew its custom picture, its plugin's picture or
  its action icon on black, and an empty key was black.

## Inferred

- A wallpaper looks continuous on the device only if the gaps between keys
  are part of the picture, the way the screensaver cuts it; keys are windows
  onto one picture behind the frame.
- People expect their own key pictures to stay on top, and transparent
  parts of them to show the wallpaper.

## Unknown

- Whether and how the official app offers one picture across all keys, and
  how its profiles would store it. Nothing is imported for it.
- Whether the panel sizes used for models without a known screen match the
  real spacing of their keys.
- How it looks on a real device, and on Windows and macOS.

## Deckly decisions

- `Page.wallpaper`: a picture path, a zoom of 100 to 400 percent (100 covers
  all keys with as much of the picture as fits their shape) and the middle of
  the part shown in thousandths of the picture's width and height. Pages and
  folders both have one. Values are clamped on load and an empty path drops
  the wallpaper; backups, exports and imports carry the picture like other
  pictures of the profile.
- Chosen pictures are decoded off the UI thread within the existing picture
  limits, made no larger than 2048 px, and saved as PNG under the profile's
  images folder. Renderers keep at most eight decoded wallpapers.
- Drawing order: the key's piece of the wallpaper, then the key's own
  picture (its transparent parts show the wallpaper) or the action icon,
  then the title, marks and press effect. Go to Page tiles keep their own
  look. A wallpaper that cannot be read leaves the keys as they were.
- The key hash includes the wallpaper, the model's layout and the key's
  place, so changing the wallpaper re-renders and re-sends each key once and
  nothing is sent again while it stays the same.
- Animated keys (on the separate `feature/animated-keys` branch) need no
  extra work beyond merging: their frames go through the same picture path.
  Their frames are flattened onto black when decoded, so a transparent GIF
  covers the wallpaper instead of letting it through until that is changed.
- The Wallpaper window opens from a button at the right end of the page
  strip's row and applies to the page or folder on the canvas. It shows the
  whole panel with the frame between keys shaded; dragging moves the
  picture, the slider or the mouse wheel zooms, and each drag or slide is
  one undo step.

## Validation

- Tests: `wallpapers_round_trip_and_old_pages_have_none`,
  `wallpapers_are_brought_within_limits`, `wallpapers_are_pictures_of_the_profile`
  (deckly-core); `the_wallpaper_view_covers_the_panel_and_stays_inside_the_picture`,
  `wallpapers_split_across_the_keys_of_every_model`,
  `keys_draw_over_their_piece_of_the_wallpaper`,
  `a_missing_wallpaper_leaves_the_keys_as_they_were`,
  `large_wallpapers_are_kept_small` (render);
  `the_view_is_brought_back_inside_the_picture` (wallpaper);
  `key_images_are_owned_immutable_pngs_and_profile_roundtrips` (store).
- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` pass on Linux. Not run in the app, on a
  device, or on Windows or macOS.

# Key press feedback on the device

- **Date:** 2026-10-28
- **Target:** Stream Deck app for Windows 7.6.0.23012
- **Device:** Stream Deck MK.2 (15 keys)
- **Methods:**
  1. Runtime observation by the owner: holding a key (for example a
     Multimedia Play/Pause key) with the official app running.
  2. The HID protocol notes in [2026-10-25-mk2-hid.md](2026-10-25-mk2-hid.md)
     and [2026-10-26-elgato-hid-docs.md](2026-10-26-elgato-hid-docs.md).
- **Implemented in:** `crates/deckly/src/render.rs` (`Live::pressed`,
  `shrink`), `crates/deckly/src/runtime.rs` (`refresh` after every key event),
  `crates/deckly-core/src/settings.rs` (`KeyPress`), Preferences > Style

## Observed

- With the official app, a key's picture on the device gets smaller while
  the key is held and returns to full size on release.
- With Deckly before this change the picture on the device did not change,
  so the effect does not come from the device itself.
- The documented protocol has key state reports, key image uploads,
  brightness, reset and standby commands, and no command for a press effect.

## Inferred

- The host app draws the effect: on press it uploads a smaller version of the
  key's picture, and on release it uploads the normal picture again.
  Deckly does the same. Checked by `render::tests::held_keys_are_drawn_smaller`.

## Unknown and next steps

1. The exact scale. Deckly draws the held picture at 80% of the key by
   default, centred on black; compare side by side with the official app, or
   capture the uploaded image over USB, and adjust the default.
2. Whether the official app lets the user turn the effect off or change its
   size. Deckly does (Preferences > Style > Key Press, 50% to 95%); this is
   Deckly's own addition until observed otherwise.
3. Whether the official app shrinks empty keys (Deckly leaves them as they
   are, since black shrinks to black) and keys whose action changes the page
   on press.
4. Whether the title shrinks with the picture (Deckly shrinks both).

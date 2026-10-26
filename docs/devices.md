# Tested devices

Only models and systems listed here as tested are supported. Others in the
device table are detected and driven from Elgato's published HID
documentation (see `docs/clean-room/2026-10-26-elgato-hid-docs.md`), but
nobody has checked them on hardware yet. Only keys are driven: dials, touch
strips, the Neo's touch sensors and info window are not implemented.

| Device | Firmware | OS | Status | Date |
| --- | --- | --- | --- | --- |
| Stream Deck MK.2 (`0x0080`) | 1.02.000 | Windows 10 22H2 | Tested: detection, key input, key images (upright, correct order), brightness, pages, logo on exit. | 2026-10-25 |
| Stream Deck MK.2 (`0x0080`) | 1.02.000 | Windows 10 22H2 | Re-checked after the per-model rotation and protocol split: key images upright and in order, key input, brightness. Logo on exit not re-checked. | 2026-10-26 |
| Stream Deck (v2) (`0x006D`) | | | Untested | |
| Stream Deck MK.2 scissor keys (`0x00A5`) | | | Untested | |
| Stream Deck 15-key module (`0x00B9`) | | | Untested | |
| Stream Deck XL (`0x006C`) | | | Untested | |
| Stream Deck XL (v2) (`0x008F`) | | | Untested | |
| Stream Deck 32-key module (`0x00BA`) | | | Untested | |
| Stream Deck Neo (`0x009A`) | | | Untested (keys only) | |
| Stream Deck + (`0x0084`) | | | Untested (keys only) | |
| Stream Deck + XL (`0x00C6`) | | | Untested (keys only) | |
| Stream Deck Mini (`0x0063`) | | | Untested, legacy protocol | |
| Stream Deck Mini (v2) (`0x0090`) | | | Untested, legacy protocol | |
| Stream Deck Mini Discord (`0x00B3`) | | | Untested, legacy protocol | |
| Stream Deck 6-key module (`0x00B8`) | | | Untested, legacy protocol | |

Not supported: the first Stream Deck (`0x0060`), whose protocol is not
documented, and the Stream Deck Pedal.

## Testing a new model

Run `cargo run -p deckly-device --example probe` with the device plugged in
and press a few keys. Then run Deckly with a numbered test profile and check
that every key shows its own number upright, in the right place, and that
pressing it triggers the right action, plus brightness and the logo on exit.
Record the firmware, OS, and what was checked in the table above.

# Elgato's published Stream Deck HID documentation

- **Date:** 2026-10-26
- **Source:** Elgato's public HID reference at
  <https://docs.elgato.com/streamdeck/hid/>: the general "Main Protocol"
  page and the pages for the Classic (15-key), XL (32-key), Neo, Stream
  Deck +, Stream Deck + XL and Mini families. Read as published on this date.
- **Method:** reading public documentation and summarizing it in our own
  words. No device other than the MK.2 was available.
- **Tests:** `crates/deckly-device/src/protocol.rs`,
  `crates/deckly-device/src/mini.rs`, `crates/deckly-core/src/device.rs`

Everything below is **documented**, not observed, unless it says otherwise.
Only the MK.2 has been checked on hardware
([2026-10-25-mk2-hid.md](2026-10-25-mk2-hid.md)).

## Two protocol families

1. **Main protocol**: Classic, XL, Neo, + and + XL. This is the protocol we
   already implemented from MK.2 observations; the documentation agrees with
   every MK.2 finding (input report `0x01`/command `0x00` with key bytes from
   offset 4, key image output report `0x02`/`0x07` with the 8-byte header,
   brightness `[0x03, 0x08, percent]`, show-logo `[0x03, 0x02]`, firmware in
   feature report `0x05`, serial in `0x06`, unit information in `0x08`).
2. **Legacy Mini protocol**: the 6-key Mini family. Different report ids,
   commands, input layout and image format.

The first Stream Deck (PID `0x0060`) uses a third protocol that the
documentation does not describe yet. Deckly does not support it.

## Main protocol details

- Input reports are at most 512 bytes: report id, command, a 16-bit payload
  length, then the payload. Output reports are 1024 bytes, feature reports
  32 bytes, both zero-padded.
- Command `0x00`: one byte per key, `0x01` pressed and `0x00` released.
- Image uploads are JPEG, split into 1024-byte reports. The final chunk sets
  the "done" flag.
- Key image rotation differs per family (see the table below).
- Unit information (feature `0x08`): rows, columns, key width and height,
  LCD width and height (all 16-bit little-endian), then bits per pixel,
  colour scheme and gallery/demo counts. This matches the MK.2 report we read
  (rows 3, columns 5, 72 × 72, 480 × 272). The docs warn that the layout can
  vary across firmware versions.
- Other documented setters we do not use yet: fill the whole LCD or one key
  with a colour (`0x05`, `0x06`), sleep timeout (`0x0D`), full-screen image
  (output `0x08`), backgrounds (output `0x0D`, XL setter `0x13`).

### Stream Deck + and + XL extras

- Touch strip events: command `0x02`. Byte 4 is the gesture: `0x01` tap,
  `0x02` long press (both with an x, y position at offsets 6 and 8), `0x03`
  flick (start x, y at 6 and 8, end x, y at 10 and 12). The event comes after
  the gesture ends. The + XL page gives two different payload lengths for a
  tap (`0x10` and `0x0A` for the long press), so Deckly does not rely on the
  length field to tell gestures apart.
- Encoder events: command `0x03`. Byte 4 is `0x00` for dial presses (one byte
  per dial, pressed or not, from offset 5) or `0x01` for rotation (one signed
  byte per dial from offset 5, positive = clockwise ticks).
- Touch strip images: output `0x0B` (full strip) and `0x0C` (part of it).

### Neo extras

- Two touch sensors are reported as two extra key bytes after the eight keys.
- An info window (248 × 58) has its own image commands (`0x0B`, `0x0C`).

## Mini protocol details

- Input report `0x01`, then one byte per key starting right after the report
  id (offset 1). The report is 65 bytes long; only the first six key bytes
  are used.
- Key images are **BMP**, not JPEG. Output report `0x02`, command `0x01`,
  then chunk index (one byte), a zero byte, a "show image" flag, the key
  index, ten zero bytes, and the image data from offset 16 (so 1008 data
  bytes per 1024-byte report).
- Brightness: feature report `[0x05, 0x55, 0xAA, 0xD1, 0x01, percent]`.
- Show logo: feature report `[0x0B, 0x63, 0x00]`.
- Firmware: feature report `0xA1` (primary), version text from offset 5.
  Serial: feature report `0x03`, text from offset 5.

## Model table

Columns × rows, key image size and rotation before upload, as documented.

| Model | PID | Keys | Key px | Rotation | Protocol |
| --- | --- | --- | --- | --- | --- |
| Stream Deck (2019, "v2") | `0x006D` | 5 × 3 | 72 | 180° | Main |
| Stream Deck MK.2 | `0x0080` | 5 × 3 | 72 | 180° | Main |
| Stream Deck MK.2 (scissor keys) | `0x00A5` | 5 × 3 | 72 | 180° | Main |
| Stream Deck 15-key module | `0x00B9` | 5 × 3 | 72 | 180° | Main |
| Stream Deck XL | `0x006C` | 8 × 4 | 96 | 180° | Main |
| Stream Deck XL (2022) | `0x008F` | 8 × 4 | 96 | 180° | Main |
| Stream Deck 32-key module | `0x00BA` | 8 × 4 | 96 | 180° | Main |
| Stream Deck Neo | `0x009A` | 4 × 2 | 96 | 180° | Main |
| Stream Deck + | `0x0084` | 4 × 2 | 120 | none | Main |
| Stream Deck + XL | `0x00C6` | 9 × 4 | 112 | 90° counter-clockwise | Main |
| Stream Deck Mini | `0x0063` | 3 × 2 | 80 | 90° clockwise | Mini |
| Stream Deck Mini (2022) | `0x0090` | 3 × 2 | 80 | 90° clockwise | Mini |
| Stream Deck Mini Discord | `0x00B3` | 3 × 2 | 80 | 90° clockwise | Mini |
| Stream Deck 6-key module | `0x00B8` | 3 × 2 | 80 | 90° clockwise | Mini |

## Inferred

- The docs write the key matrix as columns × rows (the Classic family is
  "5 × 3" and the MK.2 reports 5 columns, 3 rows), so the + XL "9 × 4" is
  9 columns and 4 rows.
- The Mini upload has no separate "done" flag. Deckly sets the "show image"
  flag on the last chunk only, so the key changes once the whole image has
  arrived.
- Key indices run row by row from the top left on every model, as on the
  MK.2. The docs show this in pictures we did not check per model.

## Unknowns and next steps

1. Every model except the MK.2 is untested. Each needs a run of the `probe`
   example and a check of key order, image orientation and brightness before
   it moves to "tested" in `docs/devices.md`.
2. Which BMP variant the Mini accepts. Deckly sends a plain 24-bit BMP
   (54-byte header, bottom-up rows). If the image is mirrored or refused, the
   header or row order is the first thing to check.
3. Deckly drives only the keys. The Neo touch sensors and info window and
   the + dials and touch strip are documented above but not implemented:
   without the hardware they cannot be checked, so they wait until someone
   with a + or Neo can test them.

# Official app 7.6: visual orientation, screensaver and standby screen

- **Date:** 2026-10-26
- **Target:** Stream Deck app for Windows 7.6.0.23012, `StreamDeck.exe`
  SHA-256 `2963DF27B13F97E5736E3F931348D89D2F693109F3CB9DC3C9899300E35EA9E3`
  (the same build as [2026-10-26-preferences.md](2026-10-26-preferences.md))
- **Host:** Windows 10 Pro 22H2
- **Device:** Stream Deck MK.2 (`0x0080`), firmware 1.02.000
- **Methods:**
  1. Runtime observation: seven screenshots taken by the owner of the
     Devices tab with the Visual Orientation list open, the "Set
     Screensaver…" and "Reset" buttons, the "Customize Image" window, the
     "Advanced…" button and the "Advanced Settings" window. Not stored in
     the repository.
  2. Static analysis with REA 6.8 and Ghidra 12.1.4 (default auto-analysis)
     of the binary above. C++ class names come from the binary's RTTI; Qt
     slot names from its meta-object strings. Functions were found through
     vtables and string references and read as decompiler output. No code
     was copied; the behaviour below is described in our own words.
  3. The owner's registry value `HKCU\Software\Elgato Systems GmbH\StreamDeck\Devices`
     (a serialized per-device map) was read to see how each setting is
     stored. Only the MK.2 entry's setting names and types were used.
  4. Elgato's public HID pages for the "Main" protocol and the Classic
     family (<https://docs.elgato.com/streamdeck/hid/>) for the sleep timer
     and image commands.
- **Implemented in:** `crates/deckly-core/src/settings.rs`,
  `crates/deckly-core/src/device.rs`, `crates/deckly-device/src/protocol.rs`,
  `crates/deckly-device/src/lib.rs`, `crates/deckly/src/render.rs`,
  `crates/deckly/src/runtime.rs`, `crates/deckly/src/prefs.rs`,
  `crates/deckly/src/cropper.rs`

## Observed: Visual Orientation

- The drop-down has two titled groups. "Icons and text": Standard, 90°,
  180°, 270°. "Device": Upside Down. Exactly one entry is checked at a time.
- The choice is stored as one integer per device (`map_dev_orientation`):
  Standard 0, 90° 1, 270° 2, 180° 4, Upside Down −4. (Static: the list is
  filled with these item values; registry: the MK.2 entry holds an int 0.)
- When a key image is prepared for upload, the model's own image rotation is
  applied first (MK.2: half a turn) and then an extra turn chosen by the
  orientation: Upside Down and 180° add 180°, 90° adds 270° clockwise
  (a quarter turn counter-clockwise), 270° adds 90° clockwise, Standard adds
  nothing. (Static, key image upload task.)
- Only Upside Down changes which physical key a profile key lands on: the
  key index is mirrored (`count − 1 − index`). The other choices leave key
  positions alone. (Static, the key index mapping functions compare the
  stored value with −4 only.)
- Changing the orientation stores the value and asks the device to redraw
  every key.

## Observed: Sleep After and the screensaver

- The sleep time is stored in minutes (`map_dev_sleep_time`) and handed to
  the device logic in milliseconds.
- Without a screensaver, on models whose firmware has a sleep timer (MK.2
  and XL have the flag set), the app gives the timeout to the device:
  feature report `[0x03, 0x0D, seconds as u32 little-endian]`, zero-padded
  to 32 bytes; 0 disables it. This matches Elgato's published "Set Sleep
  Mode Duration".
- With a screensaver set, the firmware timer is set to 0 and the app runs
  its own idle timer instead. When it runs out the app draws the
  screensaver on the keys (an image screensaver class that renders a frame
  for the whole key set). Several animated screensaver classes exist (clock,
  starfield, plasma, …); the Preferences button only offers an image.
- "Set Screensaver…" opens the "Customize Image" window. After an image is
  set, a "Reset" button appears next to it (screenshot) and clears it. The
  image is kept per device (`map_dev_sleep_wallpaper_path`, a null string on
  the MK.2 today).
- A "Sleep" action sits in the "Stream Deck" group of the action list
  (moon icon; class `KA_Sleep`, UUID `com.elgato.streamdeck.system.sleep`).
  Its inspector has only the title field, the hint "Open Preferences to
  customize Sleep settings and select a screensaver image." and an "Open
  Preferences…" button (owner's screenshots).

## Observed: Customize Image window

- Title "Customize Image - <app>", about 800 × 690 px. Top bar: "Snap to
  edges" check box (checked), a zoom slider, then three icon buttons: fit to
  width, fit to height, original size. Middle: a rounded frame with the
  aspect ratio of the device's screen and the hint "Drop Image here". Bottom
  bar: "Image from file…" on the left, "Set Image" (disabled until an image
  is loaded) and "Cancel" on the right.
- Strings next to it: "Show/Hide keys" (an overlay of the key windows),
  "Reset image", "Align to height", "Align to width", "Set Animation".

## Observed: standby screen ("Advanced…")

- "Advanced…" at the bottom right of the Devices tab opens "Advanced
  Settings - <app>": a "Standby screen" box with "Set Image…" and "Reset"
  and the note that the image appears when the device is powered but not
  connected to the app.
- "Set Image…" goes through the same Customize Image window. The upload
  task takes the cropped image at the model's screen size, applies the
  model's image rotation (not the user's orientation), and encodes it as
  JPEG. It starts at quality 98 and lowers the quality by 4 until the file
  fits a per-model limit, giving up below quality 71.
- Model values found in the per-model setup code:

  | Model (product code) | Key grid | Screen | Key image | Limit |
  | --- | --- | --- | --- | --- |
  | MK.2 (`20GBA9901`) | 5 × 3 | 480 × 272 | 72 px, 180° | 194 560 bytes |
  | XL (`20GAT9901`) | 8 × 4 | 1024 × 600 | 96 px, 180° | 614 400 bytes |

- The image goes to the device in output reports of 1024 bytes:
  `[0x02, 0x09, 0x00, last, chunk index u16 LE, chunk size u16 LE]`, then up
  to 1016 bytes of JPEG data, zero-padded. `last` is 1 on the final chunk.
  The index and size are in the same order as Elgato's documented "Update
  Background" command (`0x0D`), not the key image command.
- "Reset" reads a default image file that ships with the official app for
  that model and uploads it the same way. There is no separate "reset
  standby image" command.
- Only the classes for the newer 15-key and 32-key families implement the
  upload; the base and Mini classes leave it empty.

## Inferred

- "90°" and "270°" are meant for a device mounted on its side: the content
  of every key is turned to stay upright while the key grid stays as it is.
- The standby image is stored in the device's flash and replaces the
  power-on logo until changed.
- MK.2 key windows sit on the 480 × 272 screen with no outer margin: five
  72 px keys 102 px apart horizontally, three keys 100 px apart vertically.
  This is worked out from the screen and key sizes; the official app's key
  position table was not found.
- The screensaver stays on the keys until a key is pressed; the first press
  only wakes the device.
- The Sleep action puts the device to sleep at once, the same way Sleep
  After does when it runs out (its code was not read).

## Unknowns and next steps

1. A USB capture (USBPcap is installed) of the official app setting a
   standby image on the MK.2, to confirm the report layout above on the
   wire.
2. Whether the other Classic and XL variants accept the same standby
   command; only the MK.2 and XL setup code was read.
3. The exact key positions on each model's screen.
4. Whether the screensaver dims the device after a while.
5. Whether the official editor turns the key previews with the
   orientation.

## Checked

- Unit tests: orientation key mapping and image turns per choice, settings
  round trip with the new orientation names and screensaver path, standby
  report chunking, the sleep timer report, screensaver key crops, crop
  output size.
- Runtime and hardware checks are listed in `docs/devices.md` once done.

## Design choices in Deckly (not observed)

- Orientation names are stored as words (`standard`, `rotate90`,
  `rotate180`, `rotate270`, `upside_down`) rather than the official
  integers.
- Deckly keeps its own idle timer for Sleep After on every model (it already
  worked that way). Without a screensaver the backlight turns off; with one,
  the screensaver image is shown on the keys instead. When a main-protocol
  device connects, Deckly sets the device's own sleep timer to 0 so a value
  left by another app cannot put the device to sleep on its own.
- Deckly's Sleep action (`com.deckly.device.sleep`) is in its "Device"
  category. Its "Open Preferences…" button opens Preferences on the Devices
  tab.
- "Advanced…" acts at once: "Set Image…" and "Reset" send the image to the
  device immediately; OK and Cancel only close the window.
- The screensaver image is cropped in Deckly's Customize Image window and
  saved as a PNG in Deckly's configuration folder; the setting holds that
  file's path.
- The editor canvas keeps showing keys upright; only device images turn.
- Deckly cannot ship the official default standby images. Its "Reset"
  uploads a standby image drawn by Deckly: an "aurora" of blue light from
  the top left and violet from the bottom right (the logo's colours) over a
  near-black screen. It has no centre, so every key grid frames it evenly.
- The standby screen is offered only for models whose screen size is known
  (the 15-key Classic family and the 32-key XL family). Others report it as
  unsupported.

# Key appearance

- Date: 2026-11-05.
- Method: public Elgato and FFmpeg documentation, Deckly's renderer and
  runtime, and synthetic images and clips. The official app was not run
  in this investigation. No proprietary code, profiles or assets were used.

## Observed

- Elgato's Stream Deck 6.6 release notes describe placing an image behind
  the keys of a page and promoting it to the profile default with
  **Use current as default**. Pages and folders with their own image keep
  it. [Stream Deck 6.6 release notes](https://help.elgato.com/hc/en-us/articles/26005306818701-Elgato-Stream-Deck-6-6-Release-Notes)
- The owner previously observed an action image shrinking on press on an
  MK.2 with the official Windows app 7.6.0.23012. That test did not record
  how wallpaper layers behave. [Key press feedback](2026-10-28-key-press-feedback.md)
- The official key icon guide lists still images, GIF and animated WebP.
  [Customizing key icons](https://help.elgato.com/hc/en-us/articles/360028237271-Elgato-Stream-Deck-Customizing-Key-Icons)
- Before this change Deckly's canvas shrank the entire key rectangle on a
  physical key press. The runtime uploaded a key-sized bitmap with the
  composite shrunk onto black. Both scaled the wallpaper with the action.

## Inferred

- Keeping the wallpaper at its original scale preserves alignment across
  keys. Alpha can distinguish the visible contents of an action image
  from the wallpaper beneath it. An opaque image with the icon and
  background baked together has no separate layer to keep stationary.

## Unknown

- How the official app disables a profile default for a single page or
  folder, how it serializes wallpaper, and how it animates a press with
  wallpaper or a flattened custom image. Deckly imports no wallpaper from
  official profiles.
- Whether the official app accepts video as a key image through another
  workflow. The icon guide does not list video containers.

## Deckly decisions

- A profile can store a default wallpaper. A page or folder can store its
  own wallpaper or opt out. Older files keep their page or folder images
  and default to inheritance. A missing imported asset drops its wallpaper
  reference. Profile duplication, native exports and backups visit the
  default wallpaper through the same picture traversal as local overrides.
- The editor and device share the foreground composition. A press scales
  the title, icon and custom image, including any baked-in background,
  over a fixed tile. Empty keys show the same pixels throughout. Animated
  GIF, WebP and APNG frames keep their alpha.
- The font list uses fixed-height rows in a dedicated scroll area. Search
  computes the offset from the full list before laying out visible rows.
- Import uses FFmpeg 7+ as an optional child process. This avoids shipping
  or linking codec libraries and uses the same import path on all three
  desktops. An AV1-only decoder would cover a narrower range of clips,
  and system decoders would need three integrations. FFmpeg runs only on
  a user-selected file and the profile stores the resulting APNG.
  [FFmpeg protocols](https://ffmpeg.org/ffmpeg-protocols.html)
- One conversion runs at a time on a worker. A seekable, private copy
  limits input to 64 MiB. The child receives only file descriptors, with
  the MOV or Matroska demuxer and the `fd` input protocol selected
  explicitly. Conversion is limited to ten seconds, 100 frames, 96 × 96
  output pixels, 16,777,216 decoded pixels per frame, 32 streams, one
  decoder and filter thread, a 64 MiB single-allocation limit and a
  30-second deadline. The allocation limit applies to individual FFmpeg
  allocations. The worker checks that the APNG animates before saving it.
- The shared playback cache holds up to 32 entries and 128 MiB of decoded
  frames and evicts least recently used entries. The editor and runtime
  no longer keep their own copies of animated entries.

## Validation

- Linux/CachyOS: the editor showed a persistent font scrollbar, jumped
  from the top of the list to the first family starting with N, and took
  that family with Enter. The profile and local wallpaper controls showed
  inheritance, opt-out and restoration on a page, and inheritance inside
  a folder. A corrupt MP4 reported an error in the editor before the
  error was moved to the dedicated import dialog.
- Regression tests cover a font jump beyond the viewport, alpha after
  reducing an animation frame, fixed wallpaper pixels through a press,
  device-format encoding, old profiles, save and reload, native export
  and import, backup asset restoration, and wallpaper overrides.
- The optional test passed with FFmpeg 9.0.2 on Linux. It creates its own
  MP4 and WebM clips and checks playback timing, the duration limit and
  a corrupt source. Successful import through the final GUI was not
  verified.
- Workspace formatting, Clippy and tests passed on Linux. Cross-target
  workspace checks passed for Windows x86-64 and macOS ARM64. Neither
  platform was run. A connected MK.2 was left with the installed app;
  this build did not access it. Firmware was not recorded.

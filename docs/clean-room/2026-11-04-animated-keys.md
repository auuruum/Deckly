# Animated keys

- Date: 2026-11-04.
- Sources/method: Deckly's own renderer, runtime and editor code; the
  documentation of the `image` crate's GIF, WebP and APNG decoders; GIF
  files written by the tests themselves. No official app binaries, assets,
  profiles or other Stream Deck implementations were used.
- System: Linux/CachyOS. The owner asked for code and tests only in this
  step; the app was not run with animated keys, and no device was attached.
- Device/firmware: none.

## Observed

- Before this change a key showed the first frame of a GIF: key pictures
  went through one still-image decoder.
- The `image` crate (already a dependency, pure Rust) decodes GIF, animated
  WebP and APNG frame by frame, each frame composited onto the full canvas,
  with a delay per frame, and checks canvas size and allocations against
  limits set on the decoder.
- Many GIFs store a delay of 0 or 10 ms. Showing them that fast is not what
  their authors expect; common viewers show such frames for about 100 ms.
- The runtime loop already blocked on its message channel until the next
  timed event (sleep, folder countdown, marks), so timed frames fit the same
  wait without polling.

## Inferred

- The official app plays animated GIFs on keys (it is a widely described
  feature); its frame rate limit, its handling of tiny delays and its frame
  limits are not known to us.
- Keys showing the same file should stay in step; one clock for all keys
  does this without per-key state.

## Unknown

- Whether the official app plays video files on keys, which formats, and
  how its profiles store them.
- Whether official profiles store animated pictures as GIF files; imported
  GIFs now play, but no such profile was examined.
- How many animated keys a device takes at 30 fps before USB writes fall
  behind. Nothing was measured; no hardware was used.
- GIF repeat counts: every animation loops forever.
- Plugins' `setImage` with an animated picture still shows its first frame.

## Video

No realistic pure-Rust decoder exists for the formats people have (H.264 or
HEVC in MP4, VP9 in WebM). Options, none added yet:

1. AV1 only, through `rav1d` (Rust port of dav1d) with an MP4/WebM demuxer:
   no C, but a large dependency, slow to build, and AV1 files are rare.
2. Convert a chosen video once on import with an installed `ffmpeg` program
   run as a child process (no linking): any format, but only where the user
   has ffmpeg, which is uncommon on Windows and macOS.
3. FFmpeg libraries through FFI (`ffmpeg-next`): every format, but C
   libraries on three systems, LGPL/GPL obligations and a much bigger build.
4. The systems' own decoders through FFI: Media Foundation on Windows,
   AVFoundation on macOS, GStreamer on Linux: three platform modules and
   uneven codec support.
5. `openh264` (Cisco's C decoder built from source) with a pure-Rust MP4
   demuxer: covers most MP4 files; C code compiled into the app.

Waiting for the owner's choice.

## Deckly decisions

- No profile change: a key's `image` may name an animated file. Upload keeps
  an animated GIF, WebP or APNG as it is, copied into the profile's images
  folder, for keys; page icons and image-creator layers use the first frame.
- Limits: files up to 32 MB, canvases up to 2048 × 2048, up to 200 frames,
  up to 256 MB of decoded pixels over all frames, frames kept as squares of
  at most 192 px, 30 fps at most, a frame shows at most 60 s. A file that
  breaks the frame or byte limit keeps the frames read so far.
- Decoded animations are shared between the editor and the runtime; the
  cache holds at most 32 files or 128 MB of frames before it starts over.
- A picture file that exists but cannot be decoded shows a warning sign on
  the key over the action's icon. A missing file shows the icon, as before.
- The runtime wakes for the earliest next frame of an animated key on an
  awake, enabled device without a screensaver, renders only keys whose hash
  (now including the frame) changed, and does not repaint the editor for
  frame wakes. With no animated key on a device there is no wake at all.
- The editor asks egui for a repaint at the next frame only while it draws
  an animated key, and redraws one texture per animated key and size.

## Validation

- Tests: `frames_play_with_their_own_delays_and_loop`,
  `a_single_picture_is_not_an_animation`, `broken_files_are_errors`,
  `oversized_canvases_are_refused`, `frame_count_and_decoded_bytes_are_bounded`,
  `frames_are_kept_small_and_square`, `delays_stay_within_the_frame_rate`
  (animation); `animated_keys_show_the_frame_of_the_moment`,
  `keys_that_do_not_move_ask_for_no_wakeups`, `a_broken_picture_shows_a_warning`
  (render); `animated_key_images_are_copied_as_they_are` (store).
- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` pass on Linux. Not run in the app, on a
  device, or on Windows or macOS.

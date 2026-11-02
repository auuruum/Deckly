# Key image library and creator

- Date: 2026-11-02.
- Sources/method: Deckly's own profile, rendering, backup and editor code;
  public Elgato documentation:
  [Customizing icons and icon packs](https://www.elgato.com/us/en/explorer/products/stream-deck/customize-stream-deck-with-icons-and-icon-packs/)
  and [Key Creator](https://www.elgato.com/us/en/s/keycreator).
  No shipped application binaries, other Stream Deck implementations,
  proprietary icons, screenshots or marketplace packs were used.
- System: Linux/CachyOS. Test editor instances used temporary configuration
  directories and `DECKLY_NO_DEVICES=1`. The owner asked to stop foreground
  GUI testing and perform remaining checks in the background; test windows
  were closed. Visual acceptance of the gallery and creator is left to the
  owner, as requested.
- Device/firmware: no hardware accessed; offline MK.2 profile layout only.

## Observed

- Public documentation describes separate choices for a local picture, an
  icon library and a creator. The documented creator is web-based. This is
  evidence about the workflow, not a source of assets or implementation.
- Deckly previously opened a file dialog directly from Image and stored a
  path to the user's file. It only showed Reset when a custom image existed.
- The changed native Linux editor was launched and its inspector was seen
  with Image and a visible, disabled Reset for a default image. Further
  desktop interaction was stopped at the owner's request.
- Headless egui input tests cover selecting a library icon, switching to the
  creator, adding a text layer, cancellation, destination changes, and
  Reset with Undo/Redo. Rendering tests exercise every bundled catalog
  glyph, text, two overlapping pictures, alpha, aspect ratio, clipping,
  invalid positions, bounds and import failures. Store tests check immutable
  PNGs and profile serialization; renderer tests verify Reset returns both
  built-in and live plugin pixels and restores the corresponding cache hash.

## Deckly decisions

- Use the already bundled, licensed Phosphor catalog and font. Search the
  entire catalog offline, with an optional logos filter. All UI and image
  generation are independent Rust/egui code; there are no new dependencies,
  downloaded assets, browser pages or platform-specific image paths.
- The small creator supports a background color and at most eight layers:
  icons, text, or imported raster pictures. It supports positioning, size,
  layer ordering and removal. Text is limited to 100 characters/four lines.
- Decode and save on a named worker, wake the editor on completion, and
  apply only to the unchanged profile/page/key/action that opened the dialog.
  Cancellation and failures keep the assigned image unchanged.
- Read at most 16 MiB of input, limit decoder dimensions to 4096 per edge
  and allocations to 64 MiB, then retain at most 512 pixels per edge per
  picture layer. Only the first animation frame is used. Premultiplied
  filtering keeps hidden RGB from leaving fringes on transparent logos.
- Apply a flattened, opaque 288 × 288 PNG through the existing serializable
  `KeyConfig.image` field and common key renderer. Save to an immutable,
  UUID-named file under the profile's images directory, so caches, Undo/Redo
  and existing backup traversal keep working. No profile schema change.
- Draft layers last only while the dialog is open. A previous result can be
  loaded into a new draft as one image layer. Files are retained after Reset
  so session Undo can still restore them; profile deletion cleans its images.

## Inferred

- The common catalog, composition and egui code should behave equivalently
  on all three desktop platforms. Native file dialog behavior still depends
  on the platform. Compile checks do not verify native dialogs or rendering
  on another OS.

## Unknown

- Visual acceptance of the complete gallery/creator flow on Linux.
- Windows and macOS runtime behavior; x86_64 Windows GNU and aarch64 macOS
  editor/test targets were compile-checked using build-std on Linux.
- Hardware display and presses for newly created images. No device or OS
  support claims are added to `docs/devices.md` by this work.

## Validation

- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo test --workspace`: 312 passed, four existing manual tests ignored.
  Local socket tests were run outside the network sandbox; no GUI was opened.
- `cargo dev --no-run`: complete workspace build without launching apps.
- `cargo check -Z build-std=std,panic_abort -p deckly --all-targets` with
  `RUSTC_BOOTSTRAP=1` for `x86_64-pc-windows-gnu` and
  `aarch64-apple-darwin`: passed. LLVM's dlltool supplied the GNU import
  libraries for the Windows standard library; neither check runs the app.

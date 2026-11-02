# Image creator controls and draft history

- Date: 2026-11-02.
- Sources/method: the owner's screenshot of Deckly's creator and requests
  to distinguish adding layers from selecting them, and to add Undo/Redo;
  inspection of Deckly's own editor and profile history code.
- System/model: Linux; the screenshot shows an MK.2 profile layout. No
  hardware accessed; firmware unknown.
- Desktop interaction: none. The owner requested background/code checks
  and will check the running interface themselves.

## Observed

- The creator put Text/Icon/Image buttons immediately under Layers. Their
  appearance and placement made adding a layer hard to distinguish from
  selecting an existing one in the list below.
- Image assignment already had profile Undo, but changes to an open image
  draft had no history.
- A headless drag test found that the first press on the canvas could apply
  the pointer's movement from the previous control, making the layer jump.

## Deckly decisions

- Separate an Add layer toolbar with explicit Add text/Add icon/Add image
  buttons, a framed Layers list with type labels and full-width selection,
  and Edit selected layer settings. Explain the list's front-to-back order.
- Add Undo/Redo buttons and the existing platform shortcut conventions to
  the image dialog. Keep image draft history separate from profile history;
  changes to the assigned key still happen only on Set image.
- Restore the entire composition and selection. Group a pointer gesture as
  one edit and consecutive typing in the same field/layer within one second
  as one edit. A new edit discards redo. Search keeps its own text undo;
  icon search matching and ranking are unchanged, per the owner's request.
- Bound draft history to 50 edits and 32 MiB of retained buffers, counting
  shared immutable picture pixels once across snapshots. Draft comparisons
  use picture identity instead of scanning pixels every frame.
- On the first drag frame, count only movement after the pointer press.

## Validation

- Headless egui tests exercise add, Undo/Redo buttons, Ctrl+Z/Ctrl+Y,
  Cmd+Z/Cmd+Shift+Z, focused text edits, imported image restoration, a
  complete preview drag, unchanged assigned keys and minimum-window layout.
- History unit tests cover composition/selection restoration, gesture and
  typing grouping, redo invalidation, shared pictures and retention bounds.
- `cargo fmt --all`, workspace Clippy with warnings denied,
  `cargo test --workspace` (320 passed, four existing manual tests ignored),
  and `cargo dev --no-run` pass. Windows GNU x86_64 and macOS aarch64
  editor/test targets compile-check with build-std on Linux.
- Native visual acceptance and hardware behavior remain unverified by the
  agent. No device or platform support claims are changed.

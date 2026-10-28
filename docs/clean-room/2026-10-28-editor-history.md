# Editor undo and redo — 2026-10-28

Source/method: the owner's requested safety workflow, Deckly's existing
profile model, and installed egui 0.36.2 input/text-edit APIs. No proprietary
application was inspected. Device/firmware: none; tests use simulated MK.2
profiles and headless egui frames.

## Observed

- Editor mutations converge on the frame's edited profile. Deckly records
  before/after snapshots at that point, after folder normalization.
- Ctrl+Z undoes and Ctrl+Y redoes edits for the displayed profile. macOS uses
  Cmd+Z and Cmd+Shift+Z. Keyboard events use egui's platform command modifier;
  no platform input injection or X11-specific API is involved.
- Key placement, confirmed replacement, clearing, movement, built-in inspector
  edits, and page changes are recorded. Page/folder/key selection is restored
  with the profile. Navigation and copying do not add history entries.
- History is separate for each profile. New edits clear that profile's redo.
  Sequential typing in one inspector field is grouped within a one-second
  gap; distinct clicks remain distinct edits. Undo/redo ends a typing group.
- Focused text fields retain egui's own text undo; grid history is disabled
  during hotkey recording, rename windows, menus and confirmation modals.
  Ordinary button focus still allows editor undo.
- Restored profiles are marked dirty and sent through the normal save/refresh
  path. Plugin inspectors close so they can reopen with restored settings.
- The combined session history has at most 100 entries and 32 MiB of allocated
  JSON snapshot buffer capacity, plus metadata and temporary serialization/
  restoration allocations. Oversized edits clear stale history and report
  that they cannot be kept. Nothing is serialized while the profile is idle.
- An unexpected external profile change invalidates its history and reports
  the conflict instead of overwriting the external change.

Regression checks: `history::tests` verifies full page/folder/settings restore,
profile isolation, redo invalidation, typing groups, external changes, and
entry/byte limits. `app::clipboard_tests` sends Ctrl and Cmd shortcuts through
egui to undo/redo a paste and deletion, and checks text/recording/button focus.

Validation: formatting, workspace Clippy with warnings denied, all 133
workspace tests and a release workspace build passed. Editor compile checks
passed for Windows GNU, Linux x86_64 (X11 and Wayland features enabled) and
macOS ARM64. The native WebView tests ran outside the sandbox.

## Inferred

The shared event path should work on Windows, macOS, Linux X11 and Wayland.
Cross-target compilation checks support portability, not native desktop
runtime verification.

## Unknown / scope

Native shortcut/runtime and hardware checks for this change are pending; the
owner chose manual UI testing in this conversation. History is session-only.
Device preferences, profile administration, third-party plugin changes and
external effects of executed actions are outside editor undo.

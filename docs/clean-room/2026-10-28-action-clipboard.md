# Action copy and paste — 2026-10-28

Source/method: the owner's requested editor workflow, Deckly's own source,
and the installed egui-winit 0.36.2 source. No official application binary,
assets or implementation were inspected for this change.
Device/firmware: none; editor-only tests use simulated MK.2 and Mini profiles.

## Observed

- Deckly previously offered Copy/Paste in a key's context menu. Paste replaced
  an occupied key without asking; keyboard copy/paste was not handled.
- egui-winit turns Ctrl+C/V on Windows and Linux, and Cmd+C/V on macOS, into
  `Copy`/`Paste` events. It uses a physical-key fallback for non-Latin layouts.
  It emits Paste only when its clipboard returns nonempty text.
- Deckly now writes a marked, size-limited profile snapshot to egui's clipboard
  on Copy, and validates marked clipboard text before pasting. The snapshot
  includes the selected action and only the folders it opens.
- Empty destinations are filled immediately. Occupied destinations open an
  egui modal with Cancel and Replace. Escape/backdrop click cancels. Pending
  replacement is tied to the original profile, page/folder, key and settings;
  a changed destination cancels it. The inline plugin inspector is hidden
  while the modal is open.
- Pasted folders receive new IDs, preserve their copied contents even after
  the source is deleted, and respect folder count/depth limits. The Parent
  Folder action cannot be copied or overwritten.
- Text-field focus, hotkey recording, open menus and modals prevent grid
  shortcuts from acting on the selected key.

Regression checks: `clipboard::tests` exercises full-key round trips, folder
snapshots, protected/missing destinations, hostile clipboard data and folder
limits. `app::clipboard_tests` drives egui's Copy/Paste events, Cancel/Escape,
Replace button clicks, text-field focus, recording and stale-target checks.

Validation: formatting, workspace Clippy with warnings denied, all 125 workspace
tests, a release workspace build, and editor compile checks for Windows
(`x86_64-pc-windows-gnu`), Linux (`x86_64-unknown-linux-gnu`, both X11 and Wayland
features enabled) and macOS (`aarch64-apple-darwin`) passed. Native WebView tests
needed execution outside the sandbox to access the desktop.

Owner runtime check (2026-10-28, Windows): after taking over the visual check,
the owner reported that the copy/paste workflow works. This is owner-reported
validation; Computer Use did not inspect or operate the window.

## Inferred

The shared egui clipboard/input path avoids application-specific OS shortcuts
or X11-only APIs. Compile checks and simulated UI tests do not establish native
clipboard behavior on every desktop.

## Unknown / not tested

Native clipboard behavior on Linux X11, Linux Wayland and macOS; physical
hardware was not used. An isolated test-profile window was launched without
device access for the owner's Windows check.

# Installed application picker

- Date: 2026-11-01
- Sources/method: the owner's two screenshots of Deckly on Linux; Deckly's
  own code and a separate test instance; installed desktop-entry metadata.
  Public documentation: [XDG Desktop Entry Specification](https://xdg.pages.freedesktop.org/xdg-specs/desktop-entry/latest-single/),
  [Windows SHGetKnownFolderItem](https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderitem),
  [IShellItem::BindToHandler](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellitem-bindtohandler),
  [SIGDN](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/ne-shobjidl_core-sigdn),
  [Apple File System Basics](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemOverview/FileSystemOverview.html).
  No other Stream Deck implementation, proprietary code or assets were used.
- System: Linux/CachyOS, nested Hyprland on Wayland. The editor used its own
  temporary configuration directory and `DECKLY_NO_DEVICES=1`.
- Device model/firmware: none accessed; MK.2 layout in the offline editor.
- Tests: `os::applications`, `app_picker`,
  `opening_and_cancelling_application_picker_keeps_the_action`, and the
  explicitly run GIO integration test
  `desktop_launcher_preserves_arguments_and_expands_field_codes`.

## Observed

- Open's old Choose button opened a file picker, leaving the owner to locate
  an application executable or desktop entry manually.
- The new discovery found 81 visible applications on this desktop, including
  `Spotify (Launcher)` from `/usr/share/applications/spotify-launcher.desktop`.
- In the running editor, Applications opened a searchable modal. Typing
  `spotify` left that result; Enter selected it, and the existing Open action
  saved the desktop-file path and application name in JSON. The inspector
  showed the chosen name. No real Spotify process was launched by this check.
- The original full launcher name clipped on the small key. The generated
  title now omits a parenthesized qualifier; a custom title is preserved.
- A temporary desktop entry launched a stand-in program through GIO with a
  quoted argument intact and `%U` removed when no URLs were supplied.
- Unit tests cover locale fallback, hidden entries, desktop environment and
  TryExec filtering, user overrides (including Hidden tombstones), nested
  desktop IDs, symlinked exports, file-size limits, bundle traversal, search,
  keyboard selection, cancellation, discovery failure and saved settings.

## Inferred

- Exported Flatpak/Snap desktop entries should launch with their installed
  arguments because Deckly stores the entry itself and delegates to GIO.
  No Flatpak/Snap installation was launched in these checks.
- Windows AppsFolder should cover both desktop and packaged applications;
  the picker stores a Shell AppsFolder target instead of an executable path.
- macOS bundles in the standard application directories should open through
  the existing native `open` path, including apps in Utilities subfolders.

## Unknown

- Windows and macOS at runtime. The changed editor and its test targets were
  compile-checked for x86_64 Windows GNU and aarch64 macOS, using build-std
  on Linux. This does not verify discovery or launching on those systems.
- Launching a real terminal, D-Bus-only, Store, Flatpak or Snap application
  through the picker, and application bundles outside the scanned macOS roots.
- Hardware key presses; no device support claims are added by this change.

## Deckly decisions

- Installed applications, files, folders and manually entered commands share
  the existing serializable Open action and execution path. No new action UUID
  or profile migration is needed. Discovery never executes an application.
- Scan on demand in a worker; use bounded traversal, depth and file sizes,
  nonblocking reads for Linux desktop entries, and an explicit list-limit notice.
- Preserve XDG root precedence and visibility rules. Do not interpret Exec
  command lines or field codes in the picker; GIO owns those semantics.
- Use Deckly's existing Phosphor icon set for picker rows, without copying
  application artwork. Native Windows COM is isolated to its platform module.
- If GIO cannot start, report the missing launcher instead of opening the
  desktop entry as a text document.

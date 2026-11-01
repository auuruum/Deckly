# Installed icons in the application picker

- Date: 2026-11-01
- Sources/method: the owner's annotated screenshot of Deckly; a separate
  running Deckly instance and installed Linux desktop entries/icon themes.
  Public documentation: [Desktop Entry Specification](https://specifications.freedesktop.org/desktop-entry/latest-single/),
  [Icon Theme Specification](https://specifications.freedesktop.org/icon-theme/),
  [Windows IShellItemImageFactory::GetImage](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellitemimagefactory-getimage),
  [Apple NSWorkspace icon(forFile:)](https://developer.apple.com/documentation/appkit/nsworkspace/icon%28forfile%3A%29).
  No other Stream Deck implementation or proprietary source was read. No
  application icons or screenshots were added to the repository.
- System: CachyOS/Linux, nested Hyprland, Wayland. Temporary Deckly profile,
  `DECKLY_NO_DEVICES=1`; offline MK.2 layout, no hardware or firmware accessed.
- Checks: `app_picker::icons`, `os::applications::icons`,
  `os::applications::linux_icons`; explicitly run
  `installed_applications_have_real_icons`; visual checks below.

## Observed

- The picker previously drew the same Phosphor symbol for every application.
- The new loader read real icons for Spotify, 1Password and Alacritty from
  this desktop's installations. Each decoded preview was 64 x 64 with
  nontransparent content. The test starts none of these applications.
- In the running picker, 1Password, Advanced Network Configuration, Alacritty,
  AnyDesk and ArtCraft Launcher displayed distinct icons aligned with their
  rows. Search and selection use the existing Open action.
- Tests check theme inheritance and cycles, user overrides, hicolor fallback,
  absolute icon paths, malformed/missing/oversized images, disabled external
  SVG images, aspect ratio, transparency, native BGRA conversion and bounded
  caching of failed lookups.

## Inferred

- Flatpak/Snap exported icon themes should resolve through the same data
  directories. Their application launches were not part of these checks.
- Windows Shell's icon factory should supply both desktop and packaged app
  artwork. macOS NSWorkspace should supply the app bundle's system icon.

## Unknown

- Windows and macOS icons at runtime. The editor and its tests were only
  compile-checked for x86_64 Windows GNU and aarch64 macOS on Linux.
- Every installed third-party icon format. Linux previews decode PNG and SVG
  themes and supported raster files at absolute paths; XPM and SVG embedded
  raster images are not decoded and may use the generic fallback.
- Device key images: this change affects the picker rows only, and adds no
  device support claims.

## Deckly decisions

- Keep application artwork in its installation. Only an icon name or native
  application target is carried by the discovery result; neither the decoded
  icon nor an image path is written to a profile by this change.
- Load only requested rows on one worker, cache 256 previews/misses, bound
  both work/result queues to 32 items, and repaint when a result arrives.
- Bound theme metadata, inheritance, directory counts, file bytes and raster
  dimensions. SVG previews disable external and embedded bitmap resolution.
  Preserve aspect ratio and alpha in a 64-pixel preview drawn at 28 points.
- Linux follows the configured GTK/KDE theme when available, inherited themes,
  hicolor and unthemed pixmaps. Windows uses owned Shell/GDI objects, released
  on every return path; macOS uses an autorelease pool on the icon worker.

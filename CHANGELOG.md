# Changelog

All notable changes to Deckly. Newest first.

## 0.0.4 — Make it yours

Wallpapers that follow you everywhere, feedback marks in your own style, and
video import that no longer sends you hunting for FFmpeg.

### Look and style

- **Global wallpaper.** Set one picture for every profile, page and folder in
  Preferences > Style. A profile, page or folder can still use its own image or
  opt out; the nearest setting wins.
- **Feedback marks in your style.** The failure mark, the success check and the
  folder auto-exit ring can be hidden or restyled: icon, color, size and
  duration, with live previews and a Reset button. The device and the editor
  both draw them your way.
- **Backups carry it all.** The global wallpaper and the mark styles travel
  with a full backup.
- **Cleaner previews.** The editor shows one preview entry per key layout.

### Video import

- **No more FFmpeg hunt.** When FFmpeg 7+ is missing, a "Turn on video import"
  window opens (also under Preferences > General > Video Import). It installs
  through your system's package manager, copies the command for you, downloads
  a pinned build into Deckly's own folder, or uses an `ffmpeg` you pick.

### Fixes

- The update banner no longer covers the installed version with its buttons.

### Notes

- macOS has no pinned FFmpeg download yet; use Homebrew or pick your own.
- Builds are not code-signed, so Gatekeeper and SmartScreen may warn on first
  start.

## 0.0.3 and earlier

See the [GitHub releases](https://github.com/deckly-app/Deckly/releases).

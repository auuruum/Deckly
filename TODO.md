# Deckly TODO

Planned features, chosen by the project owner on 2026-11-02.

## First run

- [x] Find the official app on first start and offer a one-click import.
      Done: while Deckly holds nothing of the user's, a welcome window offers
      everything it found. "Import Everything…" in Preferences stays for later.

## Profiles

- [x] Switch profiles automatically by the active window.
      Done: Preferences > Profiles lists the applications of a profile with
      their logos, and Preferences > General > Active Window turns it off. On
      Wayland it needs the wlr foreign toplevel protocol; tested on Hyprland.
- [x] Export a single profile, so people can share one without a full backup.
      Done: Preferences > Profiles > Export saves a `.decklyProfile`.
- [x] Lock the keys while the computer is locked, so Text and Hotkey cannot
      fire on the lock screen.
      Done: Preferences > General > Lock Screen, with Sleep or Standby Screen
      on the device while locked.

## Actions

- [ ] Close
- [ ] Hotkey Switch
- [ ] Timer
- [ ] Play Audio
- [ ] Soundboard
- [ ] Hotkey with up to four combinations in a row. Deckly presses one, and
      imported keys lose the rest.

## Key look

- [ ] Key title fonts: font family, bold, italic, underline. There is one font
      now.
- [ ] Animated keys: GIF and video. Only the first frame of a GIF shows now.
- [ ] Wallpaper across all keys.

## Plugin SDK

Extends the Stream Deck plugin protocol instead of replacing it, so one plugin
runs in both apps.

- [ ] `deckly-sdk`: a Rust crate for writing plugins.
- [ ] `deckly-cli`: `new`, `dev` (restart on changes, live log), `validate`,
      `pack`, `publish`.
- [ ] An optional `"Deckly"` block in the manifest, and the host telling plugins
      what it can do.
- [ ] Declarative plugin settings: the plugin describes its fields and Deckly
      draws the form in egui, with no HTML or web view.

## Marketplace

A static registry, with no server or accounts.

- [ ] Registry repository: one JSON file per plugin with version, release link,
      SHA-256 per system and license. Plugins are added by pull request and
      checked by CI; a signed index is published on GitHub Pages.
- [ ] Browse, search, install and update from the Plugins tab, using the
      existing install path.
- [ ] `deckly://install/<id>` links for one-click install from a website.
- [ ] An install dialog showing the plugin's author and source. Plugins run with
      the user's rights, so "verified" means reviewed, not safe.
- [ ] Deckly's own plugins in Rust, so the catalog does not start empty: audio
      devices and mixer, microphone, OBS, Discord, now playing, timer, system
      stats, Home Assistant, soundboard.
- [ ] List only what the license or the author allows. Take nothing from
      Elgato's marketplace.

## Lower priority

- [ ] Conditions in Multi Actions: if window X is open, do Y.

### Translations

- [ ] Move the UI strings (about 270) out of the code.
- [ ] Add a language choice, and tell plugins the language instead of the fixed
      `en`.

## Undecided

- Local API and command-line control (`deckly press`, `deckly profile`), plus an
  MCP server.

## Not planned for now

- Virtual Deck.

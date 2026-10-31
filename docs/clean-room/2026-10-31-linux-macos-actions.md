# Linux and macOS: actions and plugins

- Date: 2026-10-31
- Sources: public documentation only. The Wayland `virtual-keyboard-unstable-v1`
  protocol, the X11 XTEST extension, the Linux `uinput` interface, the MPRIS
  D-Bus specification, the XDG `RemoteDesktop` portal, Apple's Quartz Event
  Services and HIToolbox key codes, Wine's user documentation. The owner's
  screenshot and profile showed what Deckly did on their Linux desktop. No
  other Stream Deck reimplementation or driver library was read.
- System: CachyOS, kernel 7.3.0, Hyprland 0.56.2, Xwayland, PipeWire, Node 26.
- Device: Stream Deck MK.2 on the owner's desktop. Key presses on the device
  were **not** part of these checks.
- Checked by: tests in `crates/deckly/src/os`, `crates/deckly-plugin-host` and
  `crates/deckly-plugin-host/tests/windows_plugin.rs`, and the runs below.

## Observed

- The owner's Open keys hold a whole command line: an interpreter, a script
  and arguments, one of them quoted with a space. `xdg-open` cannot run that,
  and neither can `ShellExecuteW`.
- A second Hyprland, nested and hidden, took keys from a virtual keyboard with
  a private layout. A Wayland and an XWayland terminal both received `Deckly
  123 Abc !@# € я`, Enter and Ctrl+A (0x01) exactly. The compositor's own binds
  fired for Super+Shift+1, Ctrl+Alt+F9 and XF86AudioPlay.
- Hyprland sends a virtual keyboard's layout to the focused client on the first
  key, not on upload. Without an empty key first, the first letters could be
  lost (one run of 6). With it, 10 of 10 runs to a Wayland window were exact
  across focus changes.
- XTEST under XWayland lost Shift on the very first key of a session ("deckly"
  for "Deckly"). The same empty first key fixed it: 6 of 6 runs exact.
- An XWayland window could lose keys for a moment after taking focus. It
  depended on the time since the focus change, not on Deckly's delays: after
  0.3 s, results at 25, 60 and 120 ms did not improve in order; after 1 s, 6 of
  6 were exact. The hidden nested compositor was not drawing frames at the
  time, which may have made this worse.
- On a private D-Bus, Deckly chose the playing MPRIS player over a stopped one
  and called Next, PlayPause, Previous and Stop on it. With no player it fell
  through to the media key.
- `wpctl` raised and lowered the real volume and left it where it was.
- A stand-in portal on a private D-Bus saw the expected order (CreateSession,
  SelectDevices, Start, then keysyms), and the saved restore token on the next
  run. This checks Deckly's side of the protocol against the public
  documentation, not a real desktop.
- In Deckly with `XDG_CONFIG_HOME` pointing at a test folder, a Node plugin
  whose manifest spelled `bin/PLUGIN.js` for `Bin/plugin.js`, an HTML plugin in
  WebKitGTK and a Unix program without an executable bit all started. The two
  web plugins registered and logged through the protocol. A Windows `.exe`
  showed "needs Wine". Deckly ran under both Wayland and X11.

## Inferred

- A hotkey sent through a virtual keyboard should reach any client that reads
  keysyms, since the keymap carries them; one using raw key positions sees the
  real key number too, except for what a US keyboard lacks.

## Unknown

- Real GNOME and KDE portals. Whether both accept every keysym.
- uinput end to end: on this machine the running kernel's modules were missing
  after a kernel update, so `/dev/uinput` reported "no such device". The
  module error is reported; the event layout has unit tests only.
- A plain X11 session on real X.Org; XTEST was run through XWayland.
- Real Wine: it is not installed here. A stand-in showed the command, working
  folder, prefix and the reported platform. Which plugins work under it is
  unknown, and Wine Mono may be needed for .NET plugins.
- Everything on macOS at run time; it is compile-checked for both
  architectures. The media key event layout comes from public `ev_keymap.h`
  values and has not been seen working.

## Deckly decisions

- `open_url` never runs anything; plugins and the Website action use it, so
  text from a plugin cannot become a command. Only Open's own text may run.
- Commands in Open run without a shell. Pipes and variables would turn a typo
  into something else, and `sh -c` remains available by writing it.
- Plugins under Wine are told their platform is `windows`, because they are
  Windows programs and their settings pages must hear the same.
- Plugins built on BarRaider's library stay refused: Wine does not change the
  access check recorded in the plugin host entry.
- Multimedia asks players and the sound server directly, since a bare
  compositor binds no action to media keys.

# Clean-room behavior and evidence log

This folder records what we learned about the official Stream Deck app, Stream
Deck hardware, and the plugin protocol, written in our own words. Deckly is
implemented from these notes, from public documentation, and from observations
of our own hardware, not from Elgato code.

Rules (see `AGENTS.md` for the full version):

- Record the target (product, version, file hashes), the method (public doc,
  REA command, runtime observation, USB capture, hardware experiment), the date,
  and the device model and firmware when known.
- Keep **observed** facts, **inferences** and **unknowns** apart. Never present
  an inference as verified behavior.
- Do not paste proprietary code, decompiled output, assets, documentation or
  long strings. Short identifiers may be cited as pointers to evidence.
- Link each observation to the Deckly test or tool that checks it.
- A clean-room process lowers copying risk; it is not legal clearance.

## Entries

| Date | Entry | Scope |
| --- | --- | --- |
| 2026-10-25 | [Stream Deck MK.2 HID protocol](2026-10-25-mk2-hid.md) | Enumeration, key input, key images, brightness, reset |
| 2026-10-25 | [Official app 7.6: main window](2026-10-25-main-window.md) | Window layout, sizes and colours, key grid, page strip, action list |
| 2026-10-26 | [Elgato's published HID documentation](2026-10-26-elgato-hid-docs.md) | Main and Mini protocols, every documented model, rotations |
| 2026-10-26 | [Official app 7.6: Preferences window](2026-10-26-preferences.md) | Tabs, layout and colours, device, profile and title style settings |
| 2026-10-26 | [Official app 7.6: orientation, screensaver, standby screen](2026-10-26-orientation-screensaver-standby.md) | Rotation choices, sleep timer and screensaver, Customize Image window, standby image upload |
| 2026-10-26 | [Official app 7.6: Navigation actions](2026-10-26-navigation-actions.md) | Action list order and icons, folders and Parent Folder, Page Indicator |
| 2026-10-28 | [Key press feedback](2026-10-28-key-press-feedback.md) | Held keys drawn smaller on the device |
| 2026-10-31 | [Profile list order, pages in the list, import and restore](2026-10-31-profile-list-and-pages.md) | Stored profile order, handles, page names and icons, what import and restore do to existing profiles |
| 2026-10-28 | [Plugin host](2026-10-28-plugin-host.md) | Plugin launch arguments, registration, info object, events and commands, manifests |
| 2026-10-28 | [Action copy and paste](2026-10-28-action-clipboard.md) | Keyboard shortcuts, replacement confirmation, complete key and folder snapshots |
| 2026-10-28 | [Editor undo and redo](2026-10-28-editor-history.md) | Profile edit history, action and page restoration, shortcuts and memory limits |
| 2026-10-30 | [Official app 7.6: tray menu](2026-10-30-tray-menu.md) | Menu groups, device heading, profile list with the active one checked |
| 2026-10-30 | [Official app 7.6: Multi Action and Key Logic](2026-10-30-multi-actions.md) | Sequence timing and cancelling, Switch, Random, custom delays, press detection, editor layout |
| 2026-10-31 | [Linux and macOS actions and plugins](2026-10-31-linux-macos-actions.md) | Simulated input per session, media control, command lines in Open, Windows plugins through Wine |
| 2026-10-31 | [Official profiles, backups and plugin folders](2026-10-31-official-profiles.md) | Backup and profile files, page and key layout, action settings, model codes, where plugins and their settings live |

## Open questions

Tracked at the end of each entry under "Unknowns and next steps".

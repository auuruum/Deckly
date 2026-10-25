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

## Open questions

Tracked at the end of each entry under "Unknowns and next steps".

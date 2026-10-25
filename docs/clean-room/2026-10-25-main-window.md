# The official app's main window

- **Date:** 2026-10-25
- **Target:** Stream Deck app for Windows 7.6.0.23012, `StreamDeck.exe`
  SHA-256 `2963DF27B13F97E5736E3F931348D89D2F693109F3CB9DC3C9899300E35EA9E3`
- **Host:** Windows 10 Pro 22H2, display at 100% scale
- **Methods:**
  1. Runtime observation: a screenshot of the main window taken by the owner
     (852 × 772 px including the 30 px title bar, Stream Deck MK.2 connected,
     a profile with 15 keys, nothing selected). Colours and sizes below were
     measured on that screenshot with a small pixel-sampling script; the
     screenshot is not stored in the repository.
  2. Static inventory of the install folder and of printable identifiers in
     `StreamDeck.exe` (class and method names only). REA was not usable in
     this session: it is not registered for this client and the installed
     Node.js (22.22.3) is older than REA requires. No code was decompiled.
- **Implemented in:** `crates/deckly/src/app.rs`, `crates/deckly/src/theme.rs`

## Observed: structure

- The install folder ships Qt 6 Widgets, Quick Widgets and WebEngine
  libraries. No `.qml` names were found in the executable, while many
  `Q_OBJECT`-style view classes were, so the main window is built with Qt
  Widgets.
- Identifier names point to these parts of the window:
  - a canvas view holding a device view, with a splitter between the canvas
    and the property inspector (`ESDCanvasView`, `ESDDeviceView`,
    `ESDCanvasSplitter`);
  - a page navigation strip (`ESDPagesNavigationView`);
  - an action source panel with a filter field and a tree of actions
    (`ESDActionsSourceView`, `ESDFilterWidget`, `ESDActionsTreeView`);
  - a property inspector that swaps in one detail view per action type
    (`ESDPropertyInspector::createDetailView`, and one
    `ESD<Action>ActionSpecificView` class each for Website, Hotkey, Open,
    Open Application, Text, Multimedia, Go to Page, key brightness, and more);
  - a canvas context menu with copy and paste of keys (`CanvasContextMenu`,
    `CopyActionListItem`, `PasteActionListItem`).

## Observed: layout and measurements (at 100% scale)

Window client area 852 × 742. Two columns:

- **Left column** (x 0–553): background `#2D2D2D`.
  - Device header: the device name in bold, about 17 px, `#E6E6E6`, with a
    drop-down chevron; under it the profile name in regular text with a
    chevron. The header starts 24 px left of the key grid. Its right end, 24 px
    past the grid, holds round icon buttons (notifications, store,
    preferences, account).
  - Key grid: keys are 70 × 70 px squares with a 3 px `#474747` border and
    rounded corners; the key image inside is 64 × 64 px. Pitch is 84 px
    (14 px gap). Empty keys show `#222222` inside the border. The grid is
    horizontally centred and starts 104 px below the top of the client area.
  - Page strip, 32 px under the grid and centred on it: a separate small
    rounded button with a pin icon, then one rounded `#373737` pill holding
    the page numbers and a "+" button. The current page is a light `#C8C8C8`
    rounded chip with dark text; other numbers are `#969696`.
  - A 2 px `#222222` splitter separates the canvas (428 px tall here) from
    the property inspector below (308 px tall), also `#2D2D2D`.
  - With no key selected the inspector shows a single centred hint line in
    `#969696`.
- **Right column** (x 554–851, about 298 px wide): background `#222222`.
  - Header area `#292929`: a search field (25 px tall, `#3B3B3B`, rounded,
    placeholder `#686868`) with a list-view toggle button to its right; under
    it a segmented control with two tabs, "Keys" (selected, `#343434` pill)
    and "Plugins".
  - Category rows: 48 px tall, `#292929`, separated by 2 px `#222222`; a
    chevron at the left (`#929292`), a 24 px outline icon, then the category
    name in bold `#E6E6E6`. Categories seen: Multi Action, Navigation,
    Soundboard, a device category, System, Custom (plugin actions).
  - An expanded category lists its actions as 40 px rows on `#222222`: a
    20 px outline icon (`#A6A6A6`) indented under the category icon, and the
    action name in `#969696`. System lists Website, Hotkey Switch, Hotkey,
    Open, Open Application, Close, Text, Multimedia.
  - A thin rounded scrollbar thumb (`#666666`) at the right edge.

## Inferred

- The property inspector is a vertical splitter pane, so its height is user
  adjustable (splitter class name, and the 2 px divider).
- Key images are drawn at 64 px on screen even though the MK.2 uses 72 px
  images, so the editor scales key images.

## Design choices in Deckly (not observed)

- Deckly draws the same layout in egui with its own code and icons
  (Phosphor). Notification, store and account buttons are left out until the
  features behind them exist; Deckly keeps the preferences button.
- The selected key gets a light border; the official selection style was not
  captured in this screenshot.
- Categories Deckly does not implement yet (Multi Action, Soundboard) are not
  shown. The device category is called "Device".
- Only System starts expanded, as in the screenshot; whether the official app
  remembers expanded categories is not known.
- Built-in action icons are drawn white on black, like the keys in the
  screenshot.

## Checked

Deckly's window was captured with `DECKLY_NO_DEVICES=1` at 852 × 742 and
compared side by side with the reference screenshot: column widths, header,
key grid position and pitch, page strip, category and action rows match the
measurements above.

## Unknowns and next steps

1. Capture screenshots of a selected key with its property inspector for
   Hotkey, Website, Open, Text, Multimedia and Go to Page, and of the device
   and profile drop-down menus, to match their layout.
2. Capture hover, selected and drag-over states of keys and action rows.
3. Check how the key grid scales when the window is resized.

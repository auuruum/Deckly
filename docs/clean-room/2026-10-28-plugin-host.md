# Plugin host: launch, registration and messages

- Date: 2026-10-28
- Sources:
  - Elgato's public SDK documentation (docs.elgato.com/streamdeck/sdk): plugin
    environment, manifest reference, plugin WebSocket reference, property
    inspector WebSocket reference.
  - Runtime observation: the official app 7.6.0.23012 on Windows 10 22H2 was
    started with the owner's installed plugins, and the command lines of its
    plugin processes were read from the process list (`Win32_Process`).
  - Plugin folders installed on the owner's machine (manifests and file
    layout only).
- Device: Stream Deck MK.2 (`0x0080`), firmware 1.02.000.
- Checked by: `crates/deckly-plugin-host` tests (manifest, protocol, server,
  install) and a run of a real Node plugin (see the end of this entry).

## Observed

- Every plugin process gets the same four arguments, after the program path:
  `-port <port> -pluginUUID <32 lowercase hex> -registerEvent registerPlugin
  -info <JSON>`. All plugins share one port; each gets its own random
  registration id.
- Node plugins run as `node --no-global-search-paths [--enable-source-maps]
  [--inspect=127.0.0.1:<port>] <plugin dir>\<CodePath> <arguments>`. The app
  uses its own Node build, picking the major version the manifest's
  `Nodejs.Version` asks for (20 and 24 were seen side by side). The inspect
  flag was present only for a plugin whose manifest has `Nodejs.Debug:
  "enabled"`.
- Native plugins run their executable directly with the same arguments.
  `CodePathWin` wins over `CodePath` on Windows; a `CodePath` without an
  extension (`Autoclicker`) runs `Autoclicker.exe`.
- Plugins whose `CodePath` is an HTML page (about a quarter of the installed
  ones) get no process of their own: the page runs inside the app's web
  engine.
- The `-info` object holds `application` (`font`, `language`, `platform`,
  `platformVersion`, `version`, here `"7.6.0.23012"`), `colors` (five
  `#RRGGBBAA` strings), `devicePixelRatio` (2), `devices` (`id` as 32 hex
  digits, `name`, `size.columns`, `size.rows`, `type`, 0 for the MK.2) and
  `plugin` (`uuid`, `version`).
- The `@elgato/streamdeck` library bundled in Node plugins compares
  `info.application.version` with the version a feature needs and refuses
  features when it is lower. A host must therefore report a version at least
  as high as the protocol level it implements.
- The same library reads `manifest.json` from the working directory, so the
  plugin folder must be the working directory.
- Plugins installed through the official marketplace store `manifest.json`
  (and other files) in an encrypted form that starts with the bytes
  `ELGATO`. Deckly does not decrypt them; it reports them as unreadable.
- Images in the manifest are written without an extension; folders hold
  `name.png` and `name@2x.png` (or `.svg`).

## From the public documentation

- Registration: the plugin connects to `ws://127.0.0.1:<port>` and sends
  `{"event": <registerEvent>, "uuid": <pluginUUID>}`.
- A property inspector page defines a global
  `connectElgatoStreamDeckSocket(port, uuid, event, info, actionInfo)` that the
  host calls; it then registers the same way.
- Events to the plugin: `keyDown`, `keyUp`, `willAppear`, `willDisappear`,
  `didReceiveSettings`, `didReceiveGlobalSettings`, `deviceDidConnect`,
  `deviceDidDisconnect`, `propertyInspectorDidAppear`,
  `propertyInspectorDidDisappear`, `sendToPlugin`, `titleParametersDidChange`,
  `systemDidWakeUp` and dial/touch events. Key and appear events carry
  `action`, `context`, `device` and a `payload` with `settings`,
  `coordinates` (`column`, `row`), `controller`, `isInMultiAction` and an
  optional `state`.
- Commands from the plugin: `setTitle`, `setImage`, `setState`,
  `setSettings`, `getSettings`, `setGlobalSettings`, `getGlobalSettings`,
  `openUrl`, `logMessage`, `showAlert`, `showOk`,
  `sendToPropertyInspector`, `switchToProfile`, and feedback commands for
  dials.
- `setTitle`/`setImage` take optional `state` and `target`; without `state`
  they apply to every state.
- Manifest fields used by Deckly: `UUID`, `Name`, `Version`, `Author`,
  `Icon`, `Category`, `CategoryIcon`, `CodePath`, `CodePathWin`,
  `CodePathMac`, `PropertyInspectorPath`, `Nodejs.Version`, `Nodejs.Debug`,
  `OS`, `SDKVersion`, and per action `UUID`, `Name`, `Icon`, `Tooltip`,
  `States` (`Image`, `Title`, `ShowTitle`), `Controllers`,
  `PropertyInspectorPath`, `DisableAutomaticStates`, `VisibleInActionsList`,
  `UserTitleEnabled`.

## Inferred (not checked yet)

- Device `type` numbers follow the public `DeviceType` values of the SDK
  library: 0 Stream Deck (15 keys), 1 Mini, 2 XL, 7 +, 9 Neo, 13 + XL. Deckly
  gives the 6-, 15- and 32-key modules the type of the model with the same
  layout.
- Device ids look like a hash of the serial; Deckly derives its own stable
  32-digit id from the serial instead.
- An action with two states flips state when the key is released, after the
  `keyUp` event was sent.
- A user-entered title or image wins over the plugin's `setTitle`/`setImage`.

## Unknown

- When the official app adds `--enable-source-maps`.
- What the official app sends right after registration besides the `-info`
  object (Deckly sends `deviceDidConnect` for each device, then
  `willAppear` for each visible key).
- Whether the official app persists the state of two-state actions.

## Deckly decisions

- Deckly runs each Node plugin on the Node major its manifest asks for:
  a matching `node` on `PATH`, else the official build for that major,
  downloaded once from nodejs.org and checked against the release's
  SHA-256 list. When that fails it falls back to any `node` on `PATH`.
- HTML plugins and property inspectors run in `deckly-web`, a separate
  helper process with the system web view, never inside the editor.
- `info.application.version` is reported as `7.6.0`, the protocol level
  Deckly follows; `platform` is `windows`, `mac` or `linux`.
- Node and native plugins run through a guard (`deckly --plugin-guard
  <plugin command>`). It stops the plugin and its child processes when
  Deckly's pipe to it closes, which also happens when Deckly crashes. The
  same code runs on every platform.
- The server listens on IPv4 and IPv6 loopback, so libraries that connect to
  `ws://localhost` work whichever address it resolves to.
- Registration ids are random 128-bit values; a connection that does not
  register with an id Deckly handed out is closed.

## Run with real plugins (2026-10-28, Windows 10 22H2, MK.2)

- `com.singerous.ai-limits` 0.2.6 (Node, SDK 3, Node 22 from `PATH`) and
  `com.elgato.cpu` (native `cpu.exe`) started, registered and received
  `deviceDidConnect` and `willAppear`. The CPU plugin's `setTitle` and
  ai-limits' SVG `setImage` showed on the keys in the editor. A key dragged
  from the action list got `willAppear` and its plugin's image at once.
- Settings that reached Deckly for an ai-limits key were saved into the
  key's profile.
- Found: a plugin that sent nothing for 15 seconds was disconnected and
  restarted, because the registration timeout stayed set on the reader's
  socket handle (Windows keeps socket options per handle). Fixed; checked
  by `server::tests::quiet_peers_stay_connected_after_registering`.
- `deckly-web` showed the ai-limits property inspector page (WebView2) and
  exited when its standard input closed.
- Found: a plugin (ai-limits) kept running after the Deckly that started
  it was gone, so two copies polled the same web services and showed errors
  until the old one was stopped by hand. Fixed with the guard; checked by
  `guard::tests` and by killing the guard's parent: the plugin process went
  with it.
- Found: ai-limits 0.2.6 (manifest asks for Node 20) reports its Codex
  usage as "auth_required" on the first request under Node 22.23.4 and
  "ok" under Node 20.20.2, with everything else equal (three runs each,
  2026-10-28, clean starts against the same profile). The official app
  runs it on Node 20. Fixed by honouring `Nodejs.Version`; with Node 20
  downloaded by Deckly the first request was "ok", also right after the
  official app had run and been closed (it left no processes behind).
- The ai-limits Claude key shows "Login Required" in both apps: the
  official app's own log for that plugin at the same time reports the same
  status (its credentials file holds no token).
- Not checked yet: opening the property inspector from the editor and its
  settings round trip, key presses on the device reaching a plugin, HTML
  plugins, macOS and Linux.

## Inspector follow-up (2026-10-28)

Source: the owner's Windows screenshots and independently written Deckly
source. The screenshots show the ai-limits settings page loading, a visible
strip at its left edge, and the plugin attribution sitting above the centre
of its Settings row. No new device or firmware observations were made.

Implementation:

- Plugin pages are served from a custom origin rooted at the plugin folder,
  with JavaScript MIME types, so ES modules can load. Canonical path checks
  reject traversal and links pointing outside that folder.
- On Windows the optional helper creates a borderless child of the editor's
  native window. Placement uses physical client coordinates; the child is
  clipped by its parent and follows it when it moves. This removes the
  separate top-level frame instead of compensating for its eight-pixel inset.
- The attribution icon and label share a horizontally arranged, vertically
  centred row. The host distinguishes both the action and the key when
  switching inspectors, and ignores stale close decisions while the new
  inspector is being requested. Collapsed panels are hidden.
- Standalone settings windows on Linux and macOS receive no placement/hide
  commands. The helper also ignores those commands without a Windows parent.
- Removed the temporary inspector-placement stderr trace. Existing opt-in
  plugin protocol logging remains available.
- The GNU loader-copy step compares DLL bytes before reusing a locked copy.

Validation on Windows: `cargo test --workspace` (111 tests),
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`,
and `cargo build --release --workspace` passed. The placement tests in
`app::plugin_page_tests` cover client coordinates, fractional scaling,
clipping, and collapsed panels. `deckly-web::tests` covers file serving,
encoded traversal, command parsing, and safe argument quoting; its symlink
case is Unix-only and was not executed on Windows.

Manual verification of the updated frame and label appearance, settings
round trips, resizing and popup interactions was handed to the owner.
These visual changes have not yet been verified in the updated running app.
Cross-target checks of `deckly` and `deckly-plugin-host` passed for
`x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`. The helper was
not cross-checked: its GTK/WebKitGTK and Apple SDK requirements are not
available in this Windows environment. Linux and macOS runtime behaviour
and their system webviews remain untested.

## Inspector frame regression (2026-10-28)

Observed from the owner's screenshots: the settings page retained side and
bottom strips after rebuilding both programs, including at the minimum
editor window size. The earlier child-window change did not remove them.

Reproduced without Computer Use or user input by
`crates/deckly-web/tests/inspector_geometry.rs`. It creates its own hidden
Tao parent, starts the real `deckly-web` executable with an independently
written HTML fixture, sends placement commands, and reads Win32 outer,
client and WebView container rectangles. No camera is used.

Before the fix, `place 150 600 400 140` produced an outer rectangle of
`[150, 600, 416, 148]` and a client rectangle of `[158, 600, 400, 140]`.
The regression test failed. The owning Tao source initializes undecorated
shadows to true and reserves non-client insets for that shadow.

Fix: explicitly disable the undecorated shadow for the Windows inspector
child. This uses Tao's existing builder option, with no production FFI or
new dependency. The test now confirms identical outer, client and WebView
rectangles at `[150, 600, 400, 140]`, `[25, 30, 720, 300]` and
`[150, 600, 320, 120]` in one helper process, covering repeated resizing.

Validation: 113 workspace tests and clippy passed on Windows; the developer
launcher rebuilt the debug artifacts. This verifies live native geometry,
not a captured image of the owner's updated window. The owner subsequently
ran the updated app and confirmed that the fix works (2026-10-28).
Computer Use was discontinued at the owner's request. No new Linux/macOS
runtime or hardware claim is made.

## Related plugin configuration windows (2026-10-28)

Sources: the owner's comparison screenshots and static REA inspection of
installed Multi OBS Controller 0.8.1 (`dev.theca11.multiobs.sdPlugin`).
The selected files were `propertyInspector/inspector.js`,
`propertyInspector/configuration.js`, and `manifest.json`. REA did not execute
those sources. Other plugin sources were outside the selected analysis.

- Artifact SHA-256: `773c00b1e35c13fbb1e061b59f0f6da34b4641143eb2582df99e6c947400ec1b`.
- REA parent Evidence: `ev_0aeeaa8c76d93afbbfc9f9026e0215809d8aef7d957aa1b8b6aee25d2f14adbb`.
- The inspector opens its local configuration page with `window.open`.
  That page reads global settings from its opener, submits form changes to
  the opener, and calls it to reconnect. This is a static observation;
  the source alone does not establish successful execution.
- The earlier Deckly handler opened every requested HTTP URL in the default
  browser, including its synthetic plugin origin. That browser cannot serve
  the helper's private custom-protocol files.

Implementation: local plugin pages now open as real related webviews in
`deckly-web`. The platform's opener environment/configuration is reused, so
`window.opener` is provided by the engine. External HTTP(S) popup links keep
opening in the browser. Missing or escaping local files and other schemes
are denied; at most eight popup windows may be open per helper. Closing a
popup drops its webview and native window; closing the inspector stops the
helper and its popup windows. A Windows WebView2 close callback also removes
the native popup shell on `window.close()`. Shared callbacks hold weak
references to the popup owner, avoiding an ownership cycle.

Validation: the Windows runtime test in `popups::tests` uses independently
written temporary HTML pages and real WebView2 instances in hidden test
windows. It opens a page with `window.open`, mutates the opener's settings,
checks the response, closes it with `window.close()`, and repeats three
times. Every iteration leaves zero retained popup windows and one owner
reference. This checks explicit view ownership, not total browser-process
memory or allocator caches. URL routing and requested window-size bounds
have unit tests. All 116 workspace tests and clippy passed.

The installed multiobs popup still needs the owner's end-to-end visual and
settings-persistence check. Linux and macOS adapters use the system related
webview APIs but have not been runtime-tested here. No hardware or firmware
behaviour was tested. No proprietary plugin sources or assets were copied
into the repository. HTML plugins continue to need their background webview
while enabled; the configuration popup's lifetime is separate.

## BarRaider plugins refuse non-official hosts (2026-10-28)

Sources: the owner's screenshots, the plugin's own `pluginlog.log`, and
static REA inspection (metadata and `#US` strings only, nothing executed) of
`BRUtils.dll` 2.1.0.0 shipped in Advanced Launcher 2.0
(`com.barraider.advancedlauncher.sdPlugin`), Windows 10 22H2, MK.2.

- Artifact SHA-256: `a6ab2615ac60cc95501667ea4b2977432b590b6b683e431824d859f9d2ccc899`.
- REA parent Evidence: `ev_d0d691780d4e8bd6e8755771277849fea525c7d6426eb585f0e68baa1c0dd1c2`.
- Observed: its `CodePath` is `com.barraider.advancedlauncher`. Deckly took
  `.advancedlauncher` for an extension and never tried `.exe`. Fixed;
  checked by `manifest::tests::entries_follow_the_code_path`.
- Observed: once started, the plugin registers, logs the `-info` Deckly sent,
  and about ten seconds later logs "Unidentified Elgato Stream Deck device"
  and "Critical update needed", then draws a "Device Not Supported" image.
- Observed (static): the library reads its parent process's module name,
  compares it with `streamdeck.exe`, reads a certificate subject (`CN=`)
  and calls `Verify`. Inferred: it accepts only a parent signed as the
  official app.
- Deckly decision: this is an access check, so Deckly does not imitate the
  official executable or its signature. Plugins built on this library do
  not work with Deckly. A plugin folder holding `BRUtils.dll` is marked
  "Works only in the official app" in the plugin list, the install notice
  and the key editor; checked by `manifest::tests::entries_follow_the_code_path`.

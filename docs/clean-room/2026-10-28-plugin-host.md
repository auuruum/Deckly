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

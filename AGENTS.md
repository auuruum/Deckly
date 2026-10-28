# Deckly — agent instructions

Read this file first. It applies to every human and AI contributor.

## 1. Project goal

Build an independent, cross-platform desktop application for Elgato Stream Deck hardware on Windows, Linux, and macOS. Aim for 1:1 functional and workflow parity with the official Stream Deck app (profiles, pages, folders, multi-actions, built-in actions, device settings, plugins) while creating an independently written application.

Deckly is unofficial. Never imply that it is made, approved, or supported by Elgato or Corsair. Do not put "Stream Deck" or "Elgato" in the product name, binary names, crate names, or icons. Nominative use in text ("works with Stream Deck MK.2", "runs Stream Deck SDK plugins") is fine.

The product name is written `Deckly` in user-facing text; crates, binaries, and identifiers stay lowercase (`deckly-*`).

## 2. Core requirements

- Write all first-party application, device-protocol, plugin-host, CLI, build, and test code in Rust. Do not add JavaScript, TypeScript, Python, C, or C++ application code, Electron, or a webview to the main application UI.
- Use egui for UI, eframe for the native shell, and wgpu as the renderer. Use the framework renderer; avoid custom low-level wgpu code unless a measured requirement calls for it.
- Prefer pure-Rust dependencies and keep the tree small. Use OS libraries or FFI only for device access or desktop integration (HID, input injection, tray, autostart); isolate that code in small platform modules and explain why it is needed.
- Windows, Linux, and macOS are first-class. Keep shared logic portable. Do not silently make a feature Windows-only; a platform that lacks a feature returns a clear "unsupported" error.
- Keep the MIT license. Do not change it without the project owner's approval. Keep third-party notices intact.
- Use SVG icons or one consistent Rust icon library (egui-phosphor). Never use Unicode characters or emoji as substitute UI icons.

### Plugin compatibility

Running existing Stream Deck SDK plugins unchanged is a goal. Plugins are third-party programs, not Deckly code:

- Deckly implements the **host side** of the publicly documented plugin protocol (manifest + WebSocket JSON events) in Rust.
- Plugins run as separate child processes with their own runtime: the plugin's native executable, or a Node.js runtime for Node-based plugins. Deckly never embeds a JavaScript engine in its own process and never ships first-party JavaScript.
- Property Inspectors are HTML pages written by plugin authors. Rendering them needs a web engine; this is the only allowed place for one, and it must live in a separate, optional helper process (system webview), never inside the egui app. Until that helper exists, plugin settings are edited through Deckly's own UI where possible.
- A misbehaving plugin must never crash, hang, or block Deckly. Treat every message from a plugin as untrusted input.

## 3. Workspace map

Crates form layers. A crate may depend only on crates in lower layers.

| Layer | Crate | Purpose |
| --- | --- | --- |
| L0 | `deckly-core` | Device model table, profile/page/key/action data model, serialization, validation. No I/O except profile load/save. |
| L1 | `deckly-device` | Stream Deck HID protocol: report encoding/decoding (pure, unit-tested) and the transport that talks to real devices. |
| L2 | `deckly-plugin-host` | Host side of the SDK plugin protocol: manifests, plugin installs, the local WebSocket server, plugin processes and their restarts. |
| L3 | `deckly` | The desktop app: egui UI, key image rendering, built-in action execution, runtime wiring. |
| helper | `deckly-web` | Separate process with the system web view for HTML plugins and Property Inspectors. Never linked into the app. |

Planned, add when the work starts (not before): `deckly-cli` (headless control and inspection).

## 4. Golden rules

### Never crash

- Non-test code must not panic. No `unwrap`, `expect`, unchecked indexing, or `panic!` on data that comes from a device, a file, a plugin, or the user. Return errors and show them.
- Treat device reports, profile files, plugin manifests, plugin messages, and images as hostile: bound lengths, sizes, counts, and allocations.
- A device disconnect, a corrupt profile, or a dead plugin is a normal event, not a crash. Recover and keep the rest of the app working.
- No `unsafe` outside small, documented platform modules. Each `unsafe` block states its safety requirements.
- Every crash fix comes with a regression test.

### Everything is an action

- User-visible key behavior is an action with a stable UUID (`com.deckly.*` for built-ins, the plugin's UUID for plugin actions), a name, a category, settings stored as JSON, and an execution path. The UI, the device runtime, and future CLI/plugin-host code all go through the same action model.
- Profile data stays serializable. The UI is a view over that data; do not hide state in widgets.

### Keep it fast

- Low startup time, idle CPU, memory, and key-press latency on all platforms. No busy polling; block on device reads or use events.
- Re-render and re-send a key image only when it changed.
- Measure before optimizing and do not claim numbers you did not measure.

## 5. Clean-room implementation

Deckly is a clean-room reimplementation: study features and behavior, document findings in our own words, implement from scratch in Rust.

- Allowed sources: public documentation (the Stream Deck SDK docs, USB HID specs, OS API docs), observation of the official app's UI and behavior, USB captures of the owner's own devices, and analysis of artifacts the owner is authorized to inspect.
- When investigating a shipped application or binary, follow the [reverse-engineer-anything (REA) skill](https://github.com/morluto/rea/blob/main/.agents/skills/reverse-engineer-anything/SKILL.md) and its target guides. Decompiled output and captures are evidence about behavior, never code to reuse.
- Do not copy proprietary code, firmware, UI assets, icons, artwork, sounds, profile templates, screenshots, or substantial documentation text into the repository. Do not commit third-party plugins or their assets; test against plugins installed on the developer's machine.
- Do not read or copy source code of other Stream Deck reimplementations or driver libraries into Deckly. Implement from our own notes and hardware observations.
- Do not extract credentials, signing keys, marketplace tokens, or license material, and do not bypass access controls.
- A familiar layout and visually similar interface are explicit goals; draw everything in Deckly's own code.
- Keep the behavior and evidence log in `docs/clean-room/`. For each entry record the source or method, date, device model and firmware where known, what was observed, and the test that checks it. Keep **observed**, **inferred**, and **unknown** apart.
- A clean-room process lowers copying risk; it is not legal clearance. Never describe it as such.

## 6. Implementation workflow

- Read the relevant code and docs before changing files. Keep changes small; no speculative architecture.
- Every change includes tests where the logic is testable without hardware (protocol encoding, model validation, serialization round-trips, rendering sizes).
- Visual UI changes are checked by running the app and looking at it.
- Before finishing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- For platform-dependent changes, compile-check Windows, Linux, and macOS targets when the environment allows. Report compile checks separately from runtime and hardware testing; never imply a platform or device was tested when it was not.
- Do not claim support for a device model or OS until it was tested and recorded in `docs/devices.md`.

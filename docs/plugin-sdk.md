# Deckly plugin SDK: design

Status: draft, 2026-11-11. Nothing here is implemented yet. "Decided" lists the
project owner's decisions; everything under "Proposed" waits for the owner's
confirmation and may change.

## Decided (owner, 2026-11-11)

- The SDK is for Deckly only. Plugins written with it do not have to run in the
  official Stream Deck app, so the SDK does not use the Stream Deck plugin
  protocol.
- Pure Rust, like Deckly, with performance as a first goal.
- The SDK lives in this repository, in the same workspace as the plugin host.
- A GitHub organization named `deckly-app` is created now and Deckly moves
  into it. Deckly's own plugins are published there, one repository per
  plugin.
- The first plugins, which decide what version 1 must hold: Timer, System
  stats and OBS.
- The marketplace is part of this work, not a later project. Its index is
  signed; the signing key is a secret of the organization's CI.
- Plugin secrets (an OBS password) are stored in the system's own secret
  store.
- All of it is built on the branch `feature/plugin-sdk`, cut from `dev`. Each
  piece gets its own task branch cut from `feature/plugin-sdk` and is merged
  back into it. `feature/plugin-sdk` goes into `dev` only when the SDK and the
  first plugins work, on the owner's word, and from there into `main`. This
  replaces "branch from `origin/dev`" in AGENTS.md for this work only; the
  rules for `dev` and `main` are unchanged.

Running existing Stream Deck SDK plugins unchanged stays a goal (AGENTS.md).
The host keeps that path as it is. Deckly plugins are a second kind of plugin
beside it.

## Goals

- Writing a plugin takes one Rust crate and no HTML, JavaScript or manifest
  written by hand.
- One plugin source builds for Windows, Linux and macOS. On Linux the same
  binary works on X11 and Wayland without the author knowing which one runs.
- A plugin starts fast, idles at zero CPU and never blocks or crashes Deckly.
- The host treats everything a plugin sends as untrusted, as it already does.

Not goals for version 1: sandboxing, dials and touch strips (the protocol
leaves room for them), async runtimes inside the SDK.

## Proposed

### Two kinds of plugins in one host

| | Stream Deck SDK plugin | Deckly plugin |
| --- | --- | --- |
| Manifest | `manifest.json` (theirs) | `deckly-plugin.json` (generated) |
| Transport | WebSocket, JSON text | pipes, length-prefixed frames |
| Images | base64 inside JSON | raw bytes, or drawn by the host |
| Settings UI | HTML page in `deckly-web` | form drawn by Deckly in egui |
| Lifetime | guard process | the pipe itself |

Both end in the same `HostEvent`/`Command` model, so the app, the runtime and
the action model do not learn about a second kind of plugin
("everything is an action").

### Crates

| Crate | Purpose | Depends on |
| --- | --- | --- |
| `deckly-plugin-protocol` | Message types, framing and limits shared by host and SDK. Pure, no I/O. | `serde`, `serde_json` |
| `deckly-sdk` | What a plugin author uses: actions, settings, key faces, host services, the run loop. | `deckly-plugin-protocol` |
| `deckly-cli` | `new`, `dev`, `validate`, `pack`; later `publish` and headless control. | protocol, plugin host |

`deckly-plugin-host` gains a dependency on `deckly-plugin-protocol`. The SDK
never depends on `deckly-core`, `deckly-device` or the app. The protocol and
SDK crates carry their own versions, not the workspace version, and are
published to crates.io; plugin authors never clone this repository. AGENTS.md
gets the new rows when the work starts.

### Transport

The host starts the plugin with pipes on standard input and output.

- A frame is a 4-byte little-endian length, a 1-byte kind and the body. Kind 0
  is a JSON message; kind 1 is a binary blob (image bytes) that the next JSON
  message refers to. No WebSocket handshake, masking, ports or base64.
- The pipe is the identity: no port, no registration token, nothing another
  local process can connect to.
- When Deckly exits or crashes the pipe closes, the SDK's reader thread sees
  end of file and exits the process. No guard process per plugin. The host
  still kills a plugin that does not go away in time.
- Standard error is the plugin's log; the host writes it to the plugin's log
  file. Standard output belongs to the protocol: the SDK takes it over at
  start, and a stray `println!` is a protocol error that the host reports in
  plain words and answers with a restart.
- The host opens with `hello` (protocol number, app version, platform, Linux
  session type, language, devices, available host services). The plugin
  answers `ready` (protocol number, SDK version). Unknown message types and
  fields are ignored on both sides; a new feature is a new host service name
  or message type, never a changed one.
- Every length, count and string is bounded in the protocol crate, and the
  host and SDK share those limits.

To measure before it is called faster: time from key press to plugin handler,
and host plus plugin CPU for 32 keys updated once a second, against the
WebSocket path with the same plugin logic.

### Manifest

`deckly-plugin.json`, in JSON because the host already parses JSON and needs no
new dependency. It is generated, not written: the plugin binary prints it when
run with `--describe`, from the actions and settings registered in Rust, and
`deckly-cli` writes it into the package.

It holds: id, name, version, author, licence, description, icon, the protocol
number it needs, the host services it uses (shown to the user at install),
one program path per target (`x86_64-unknown-linux-gnu`,
`aarch64-apple-darwin`, ...), and per action its id, name, icon, tooltip,
states and settings form.

Package: a folder named by the plugin id, shipped as a `.decklyPlugin` zip
archive through the existing install path.

### SDK shape

A sketch, not a final API:

```rust
use deckly_sdk::prelude::*;

#[derive(Default, Serialize, Deserialize)]
struct CounterSettings {
    step: u32,
}

#[derive(Default)]
struct Counter {
    count: u32,
}

impl Action for Counter {
    const ID: &'static str = "com.example.counter.count";
    const NAME: &'static str = "Counter";
    type Settings = CounterSettings;

    fn form(form: &mut Form) {
        form.number("step", "Step").range(1..=100).default(1);
    }

    fn key_down(&mut self, key: &Key<Self>) {
        self.count += key.settings().step;
        key.face(Face::value(self.count.to_string()).label("Count"));
    }
}

fn main() -> deckly_sdk::Result<()> {
    Plugin::new("com.example.counter", "Counter")
        .action::<Counter>()
        .run()
}
```

- One value of the action type per key on a device, created when the key
  appears and dropped when it goes. Per-key state is a plain field.
- Settings are a typed `serde` struct; the SDK loads and saves them.
- `Key` handles are cheap to clone and `Send`, so a worker thread, a timer or
  the author's own async runtime can update a key.
- Built in: `key.every(duration, ...)` timers and plugin-wide state shared by
  all keys (one OBS connection for many keys).
- Threads and blocking I/O inside; no async runtime in the dependency tree.
  Dependencies: `serde`, `serde_json`, the protocol crate. No proc macros in
  version 1.
- The SDK sends a key update only when it differs from the last one sent.

### Drawing a key

Three levels, cheapest first:

1. Static: icon, title and states from the manifest.
2. Face: the plugin sends a small description (icon, label, value, a second
   line, progress bar, colours) and the host draws it with its own renderer,
   fonts and key style. No image bytes cross the pipe. Timer and System stats
   need nothing more.
3. Image: PNG, JPEG or SVG bytes, or raw RGBA, in a binary frame.

### Settings forms

The plugin declares a form per action and one for the whole plugin (OBS
address and password); Deckly draws both in egui.

Fields: text, secret, number, slider, checkbox, choice, file, folder, colour,
hotkey, button, section and note. A choice lists fixed options or asks the
plugin for them when the form opens (OBS scenes). Fields may depend on other
fields (shown only when another has a value) and carry simple validation.

Secrets are stored in the system's own store: Credential Manager on Windows,
Keychain on macOS, the Secret Service on Linux (decided on 2026-11-11). They
never go into a profile, a profile export or a backup. A system without such a
store, as some minimal Linux sessions are, gets a clear "unsupported" message
on the field; what else to offer there is open.

### Host services

What makes one plugin binary work on X11, Wayland, Windows and macOS: the
plugin asks Deckly to do the platform work that Deckly already does for its
built-in actions.

Version 1: press a hotkey, type text, read and follow the active window, open
a URL, file or application, switch profile, show the success or failure mark.
Later: clipboard, notifications, media keys, audio.

A service missing on a platform returns a clear "unsupported" error and is
absent from `hello`, so a plugin can check before offering an action. A
plugin lists the services it uses in its manifest.

### CLI

- `deckly-cli new`: a plugin crate with one action and a CI workflow that
  builds and packs for the three systems.
- `deckly-cli dev`: builds, shows the plugin to the running Deckly from its
  folder, restarts it on changes and follows its log.
- `deckly-cli validate`, `deckly-cli pack`.

Building for three systems happens in CI on each system; no cross-compiling
from one machine is promised.

### Tests

- Protocol crate: framing round trips, limits, hostile frames.
- Workspace: an example plugin built with the SDK is started by the real host
  in a test; a key press reaches it and its face update comes back. Runs on
  Linux, Windows and macOS in CI.
- SDK: a test harness that plays the host, so plugin authors test without
  Deckly.

### Plugin repositories

Each plugin is its own repository in the organization, with its own issues,
releases and version. To keep many small repositories alike:

- `deckly-cli new` writes the whole repository: the crate, the licence and a
  CI workflow that builds on Linux, Windows and macOS and attaches one
  `.decklyPlugin` per target to a GitHub release when a version tag is
  pushed.
- A plugin depends on `deckly-sdk` from crates.io. Until the SDK is published,
  the first plugins use a git dependency on `feature/plugin-sdk`.

### Marketplace

A static registry, with no server and no accounts, as TODO.md already plans.

- A registry repository in the organization holds one JSON file per plugin:
  id, name, author, licence, source repository, and per version the release
  link and SHA-256 for each target.
- A plugin is added or updated by pull request. CI checks the entry: the
  schema, that the links answer, that the SHA-256 matches, that the manifest
  inside the package agrees with the entry.
- CI builds one index file from all entries, signs it with the key kept in
  the organization's CI secrets and publishes both on GitHub Pages. Deckly
  carries the public key and refuses an index whose signature does not match.
  Deckly downloads that one file, bounded in size, and never more often than
  the user opens the Plugins tab or the update check runs.
- The Plugins tab gets browse, search, install and update. A download is
  checked against the SHA-256 from the index and then goes through the
  existing install path.
- The install window names the author, the source repository and the host
  services the plugin uses. Plugins run with the user's rights, so "verified"
  means reviewed, not safe.
- `deckly-cli publish` writes the registry entry from a plugin's release and
  opens the pull request.

Later, not in this work: `deckly://install/<id>` links and a website.

### Order of work

1. Organization and repository move (below). It does not block the rest.
2. Protocol crate, the pipe transport in the host, a minimal SDK (key events,
   titles, faces, settings), the example plugin and its test.
3. Settings forms in egui.
4. Host services.
5. `deckly-cli new`, `dev`, `validate`, `pack`.
6. Timer and System stats, each in its own repository.
7. OBS.
8. Marketplace: registry repository and its CI, the index, the Plugins tab,
   `deckly-cli publish`. It comes last because it needs plugins to list.

## Organization and repository move

Checked on 2026-11-11: the name `deckly` is taken on GitHub; `deckly-app`,
`decklyapp`, `getdeckly`, `deckly-dev` and `deckly-rs` were free.

- Done on 2026-11-11: the owner created the organization `deckly-app` and
  transferred the repository to `deckly-app/Deckly`.
- The repository path changes in `crates/deckly/src/update.rs`,
  `install.sh`, `install.ps1`, `README.md`, `CHANGELOG.md`, the Homebrew
  formula and its tap, the AUR package, the Windows installer and
  `docs/releasing.md`.
- Checked with `curl -L` on 2026-11-11, after the transfer: the old
  `api.github.com/repos/auuruum/Deckly/releases/latest` answers 200 through
  one redirect, a v0.0.4 release file under the old
  `github.com/auuruum/Deckly/releases/download/` path downloads through two
  redirects, and the old `raw.githubusercontent.com/auuruum/Deckly/main/install.sh`
  still answers 200. These are the requests an installed 0.0.4 makes; an
  installed 0.0.4 itself was not run through an update.
- The Homebrew tap is still `auuruum/homebrew-tap`; it was not moved.
- The macOS bundle id `io.github.auuruum.deckly` stays, so the system keeps
  treating new versions as the same application.

## Open questions

- Crate names: `cargo search deckly` on 2026-11-11 listed no crate whose name
  starts with `deckly` among its first results. The exact names
  `deckly-sdk`, `deckly-cli` and `deckly-plugin-protocol` were not looked up
  one by one.
- The signature scheme for the registry index and how the public key in
  Deckly is replaced if the CI key has to change.
- What a secret field does on a system without a secret store.
- How `deckly-cli dev` reaches the running app.
- The rule for the oldest Rust version the SDK supports.

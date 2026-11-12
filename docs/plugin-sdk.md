# Deckly plugin SDK: design

Status: draft, 2026-11-12. Stage 1 (step 2 of "Order of work") is built on
the task branch `feature/sdk-protocol-transport`; what it settled or changed
is under "Settled by stage 1". Settings forms (step 3) are built on the task
branch `feature/sdk-settings-forms`; see "Settled by the settings forms".
"Decided" lists the project owner's decisions; everything under "Proposed"
waits for the owner's confirmation and may change.

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
  store when there is one; without one Deckly falls back to something of its
  own (the owner left the choice of fallback to the design).
- All of it is built on the branch `feature/plugin-sdk`, cut from `dev`. Each
  piece gets its own task branch cut from `feature/plugin-sdk` and is merged
  back into it. `feature/plugin-sdk` goes into `dev` only when the SDK and the
  first plugins work, on the owner's word, and from there into `main`. This
  replaces "branch from `origin/dev`" in AGENTS.md for this work only; the
  rules for `dev` and `main` are unchanged.

Running existing Stream Deck SDK plugins unchanged stays a goal (AGENTS.md).
The host keeps that path as it is. Deckly plugins are a second kind of plugin
beside it.

## Settled by stage 1 (implementation, 2026-11-11)

Built: `crates/deckly-plugin-protocol`, the pipe transport in
`crates/deckly-plugin-host` (`src/pipe.rs`), `crates/deckly-sdk`, key faces in
the app's renderer, `examples/counter-plugin` with an integration test that
runs the real host, and `examples/transport-bench` for the measurements below.
These are facts of the code now; the owner may still change them before
`feature/plugin-sdk` goes into `dev`.

Wire format:

- The 4-byte little-endian length counts the body only, not the kind byte.
  Limits: 1 MiB for a JSON message, 4 MiB for a blob; a larger length is
  refused before anything is allocated. All limits are constants in
  `deckly_plugin_protocol::limits`.
- Kind 0 is a JSON message, kind 1 a blob for the message right after it.
  Frames of other kinds are skipped, so a later protocol can add kinds. Two
  blobs in a row are a protocol error; a blob followed by a message that does
  not use it is dropped.
- Messages are JSON objects tagged by `type` in snake case. Host to plugin:
  `hello`, `device_connected`, `device_disconnected`, `key_appear`,
  `key_disappear`, `key_down`, `key_up`, `settings`, `global_settings`,
  `system_wake`. Plugin to host: `ready`, `set_title`, `set_state`,
  `set_face`, `set_image`, `set_settings`, `set_global_settings`,
  `get_global_settings`, `show_ok`, `show_alert`. Unknown types and fields are
  ignored; a known type with a missing or wrong field, or one over a limit,
  is a protocol error.
- Protocol number 1. `hello` carries it; `ready` must answer with a number
  between the host's oldest and newest, and must be the first message. A
  manifest that needs a newer number is not started ("needs a newer Deckly").
  `hello.services` is empty until host services exist.

Host:

- A Deckly plugin is a folder named by its id holding `deckly-plugin.json`,
  next to the `.sdPlugin` folders. Plain folders install and uninstall through
  the existing install path; `.decklyPlugin` archives do not yet.
- The program is picked by target triple; when the exact triple is missing,
  one for the same processor and system is used (a `musl` build on a `gnu`
  system).
- A writer thread with a queue of 1024 frames and a reader thread per plugin;
  `Host::send` never blocks. A full queue, a malformed frame, a broken rule, or
  no `ready` within 10 s is a protocol error: one plain line in the plugin's
  log ("protocol error: ...") and the usual restart backoff. A restarted or
  stopped plugin gets its input closed and 3 s to exit before it is killed,
  in the background, so a plugin that ignores the closed pipe holds nothing
  up.
- Text printed to standard output is recognised (a length made of printable
  bytes) and logged as such, with the start of the text. The SDK cannot move
  standard output out of the way: that needs `dup2`, which is FFI, and the SDK
  may depend only on `serde`, `serde_json` and the protocol crate. So a stray
  `println!` restarts the plugin, as the draft said.
- The app is unchanged in shape: the events it builds for Stream Deck plugins
  are translated into protocol messages inside the host, and plugin messages
  become the existing `Command`s, plus `SetFace` and `SetImageData`. A typed
  event model in the app could replace the translation later.
- Deckly actions never flip states on their own: the plugin sets the state.
- Like Stream Deck plugins, a Deckly plugin hears of a key only while a
  device shows it. In the editor without a device the key shows the action's
  icon from the manifest, not its face.

SDK (`deckly-sdk` 0.1.0), where it departs from the sketch below:

- `Face::new().value("3").label("Count")` and `key.set_face(face)`; the
  sketch's `Face::value(...)` and `key.face(...)` clash as builder and
  constructor names.
- The timer is `key.every(period)`, which calls `Action::tick` on the key's
  own value; `key.stop_timer()` ends it. No closure, so the timer can use
  `&mut self` without locks.
- `Action` needs `Default` (one value per key, made by the SDK) and
  `Settings: Clone`. Settings that do not fit the type read as `Default` with
  a line on standard error.
- Not yet in stage 1: `form()` and global settings in the API (both came
  with the settings forms, below), plugin-wide state, dials.
- `--describe` names the running program for the current target only;
  `deckly-cli pack` is meant to merge the targets.
- `deckly_sdk::testing::Harness` plays the host over in-process pipes.

Faces, as drawn by the app: a label at the top, an icon from the plugin
folder, the value, a detail line and a progress bar, in the key's title font
and colour unless the face sets colours. A title on the key (the user's or
the plugin's) takes the label's place; the user's own picture wins over the
face's background. Text is cut to 32 characters and shrunk to fit the key.

## Settled by the settings forms (implementation, 2026-11-12)

Built: forms in `crates/deckly-plugin-protocol/src/form.rs`, the builder in
`crates/deckly-sdk/src/form.rs`, global settings in
`crates/deckly-sdk/src/globals.rs`, drawing in `crates/deckly/src/forms.rs`,
secrets in `crates/deckly/src/secrets.rs` and `crates/deckly/src/os/secrets/`,
and forms in `examples/counter-plugin`. Facts of the code; the owner may
still change them.

Forms in the manifest:

- `deckly-plugin.json` holds `form` per action (the key's settings) and one
  `form` for the plugin (its global settings): `{"fields": [...]}`.
- A field is a flat object: `kind`, `key` (the settings property), `label`,
  `help`, `placeholder`, `default`, `required`, `min`, `max`, `step`,
  `integer`, `min_length`, `max_length`, `multiline`, `options` (each
  `{value, label}`), `dynamic`, `extensions` and `visible_when`
  (`{field, equals}`). Properties that do not apply to the kind are ignored.
- Kinds: `text`, `secret`, `number`, `slider`, `checkbox`, `choice`, `file`,
  `folder`, `color`, `hotkey`, `button`, `section`, `note`. An unknown kind
  reads as unknown and is not shown; unknown properties are ignored. A known
  property of the wrong type, or a form over a limit, makes the manifest
  invalid, like any other manifest error.
- Values: strings for text, secret, choice, file and folder; numbers for
  number and slider; `true`/`false`; `"#rrggbb"`; a hotkey as `{"key",
  "ctrl", "shift", "alt", "meta"}`, the shape built-in Hotkey keys use.
- Limits in `deckly_plugin_protocol::limits`: 64 fields, keys of 64 bytes
  (letters, digits, `_`, `-`, `.`, unique in the form), labels of 100
  characters, help lines and notes of 500, 256 options of 256 bytes, 16
  extensions, 4 KiB for a default or a condition value, 8192 characters for
  a text, 64 KiB for the whole form. A default must be a value its field
  accepts.
- Checks shared by host and SDK (`Field::check`) give plain messages: "This
  field is required.", "Use at most 12 characters.", "Enter a number from 1
  to 100.". A dynamic choice accepts any string.
- `visible_when` uses the setting or the field's default. A field whose
  condition names a hidden field is hidden too; a loop of conditions hides
  its fields. A condition on a property no field shows uses the setting
  alone. Hidden fields keep their values and are not checked.

Messages (protocol number unchanged, 1; older hosts ignore them):

- Host to plugin: `get_choices {request, field, key?}` and `button {field,
  key?}`. Without `key` they belong to the plugin-wide form. The key need not
  be on a device.
- Plugin to host: `choices {request, options, error?}`, at most 256 options
  and an error of 200 characters.
- The host gives up on a choice after 5 seconds. The form shows "Loading
  options…", then "The plugin did not answer in time." or the plugin's
  error, with Retry; a plugin that is not running fails at once. A late
  answer is dropped.
- The SDK asks for the global settings (`get_global_settings`) right after
  `ready`.

SDK:

- `Action::form(form: &mut Form)` and `Plugin::form(|form| ...)`, a builder:
  `form.number("step", "Step").range(1..=100).default(1)`. `range` with
  whole-number types sets `integer`. Text is cut to the limits, fields past
  64 are left out with a line on standard error, and `--describe` refuses a
  manifest the host would refuse (`Plugin::check`).
- `Action::choices(field, &settings, &globals)` is an associated function,
  not a method: Deckly asks for options of keys no device shows, which have
  no action value. It runs on a thread of its own, so a slow lookup does not
  hold key events up; a panic in it is answered as an error.
- `Action::button(&mut self, key, button)`. For a key no device shows, the
  SDK makes a value just for the press and drops it after. Such a key stores
  settings but draws nothing (titles, faces, images and timers are dropped),
  because nothing would update them.
- `Globals` (from `key.globals()` and the plugin handlers): `get::<G>()`,
  `set(&g)`, `update::<G>(...)`, `value()`, `is_loaded()`.
  `Action::global_settings_changed` and `Plugin::on_global_settings` hear of
  changes; `Plugin::on_choices` and `Plugin::on_button` answer the plugin-wide
  form. Storing a typed value drops properties the type does not hold, so
  the type should hold every field of the form (the counter keeps the rest
  in a `#[serde(flatten)]` map).
- The harness answers `get_global_settings` itself and adds `place` (a key
  in the editor only), `choices`, `plugin_choices`, `button`,
  `plugin_button`, `set_globals`, `globals`, `stored_globals`, `form`,
  `plugin_form` and `manifest`.

App:

- A Deckly action with a form shows it in the rows of the key form, with
  the same grid, label column, widths and widgets as the built-in actions
  (DragValue for numbers, ComboBox for choices, the built-in hotkey
  recorder). The rows scroll when the form is taller than the panel.
- Edits go into the key's settings in the profile copy the editor works on,
  so they are saved and undone like any other edit; the plugin gets
  `settings` after each change. Undo and redo do not tell the plugin yet; it
  hears of the restored settings when the key appears again.
- Input that fails its checks stays in the field with the message under it
  and is not saved; the last value that passed stays in the profile.
- A plugin with a plugin-wide form gets a gear button in the Plugins tab
  (and "Settings…" in its menu), which opens a window with the form. It
  edits the plugin's global settings file and sends them to the plugin; it
  reads the file again when the plugin stores global settings itself.

Secrets:

- The settings (a key's, or the global settings file in `plugin-data`, which
  backups include) hold a reference `{"deckly_secret": "<id>"}`, never the
  value. A key's id is a random UUID; a plugin-wide secret's id is
  `global.<field>`. The value is stored under the plugin's id and that id.
- On the way to a Deckly plugin the host passes every settings object through
  a filter that puts the values back (`Host::set_settings_filter`); a secret
  that cannot be read reaches the plugin as an empty string. Settings the
  plugin sends back lose their secret values again before they are saved; a
  value equal to the stored one, or one sent while the store could not be
  read, does not overwrite the store.
- A secret is stored when its field loses focus or Enter is pressed, not on
  every key.
- Stores: Credential Manager (generic credential `Deckly/<plugin>/<id>`, kept
  on this computer), the Keychain (generic password, service `Deckly`,
  account `<plugin>/<id>`) and the Secret Service (attributes `xdg:schema`
  `app.deckly.PluginSecret`, `deckly-plugin`, `deckly-secret`, default
  collection, a `plain` session over the user's own bus, 3 s timeout). A
  locked keyring is reported; Deckly does not ask to unlock it.
- The fallback is `secrets.json` in Deckly's data folder, mode 0600 on Linux
  and macOS, written through a temporary file. It is used when the system's
  store does not answer, or with `DECKLY_SECRET_STORE=file`; the field then
  says "Kept in a file on this computer, without the system's protection."
  with the reason on hover. At start, when the store answers, the file's
  secrets move into it and the file is removed; if one cannot move, the file
  stays in use.
- Tested: the file and the reference handling in unit tests; the Secret
  Service by a test against a private GNOME Keyring in its own D-Bus session
  (`secret_service_round_trip`, ignored by default) and by running the app
  against such a keyring. Credential Manager and the Keychain are
  compile-checked only; they have not run.
- A copied key shares its secret with the original until either is given a
  new one through Deckly; secrets of deleted keys stay in the store.

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
WebSocket path with the same plugin logic. Measured; see "Measurements".

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

Built; see "Settled by the settings forms". Storing secrets in the system's
own store was decided on 2026-11-11.

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

- Protocol crate: framing round trips, limits, hostile frames. Built
  (`crates/deckly-plugin-protocol/src/tests.rs`).
- Workspace: an example plugin built with the SDK is started by the real host
  in a test; a key press reaches it and its face update comes back. Runs on
  Linux, Windows and macOS in CI. Built (`examples/counter-plugin/tests/host.rs`,
  with a plugin that prints to standard output and one that must exit when its
  input closes); it runs in `cargo test --workspace`, so in CI on all three
  systems. Only Linux has run it so far.
- SDK: a test harness that plays the host, so plugin authors test without
  Deckly. Built (`deckly_sdk::testing::Harness`).

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
   titles, faces, settings), the example plugin and its test. Built (see
   "Settled by stage 1").
3. Settings forms in egui. Built (see "Settled by the settings forms").
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
- How `deckly-cli dev` reaches the running app.
- The rule for the oldest Rust version the SDK supports.
- Whether undo and redo should send the restored settings to a Deckly plugin
  right away.
- Whether copying a key should give its secrets new references (and copy the
  values), and whether removing a key should remove its secrets from the
  store.
- Whether the Secret Service should use an encrypted session
  (`dh-ietf1024-sha256-aes128-cbc-pkcs7`) instead of `plain`; that needs a
  Diffie-Hellman and AES implementation in the tree.

## Measurements (2026-11-11)

Method: `examples/transport-bench`, run with `cargo test -p transport-bench
--release -- --ignored --nocapture --test-threads 1`. The real plugin host runs
one of two plugins with the same logic and threading: `bench-pipe` (Deckly
SDK, pipes) and `bench-ws` (a native Stream Deck style plugin using the same
WebSocket library as the host). The app's own work (rendering, devices) is not
included. Machine: AMD Ryzen 7 7700X, 16 threads, 30 GB, Linux
7.3.3-1-cachyos, governor `performance`, release build; the desktop and an
installed Deckly were running, so the machine was not idle.

Key-down latency, 2000 presses after 100 warm-up presses, 2 ms apart:

| Path | Send to handler, median / p95 / p99 / max | Round trip to the title command, median / p95 / p99 / max |
| --- | --- | --- |
| Pipe | 17 / 24 / 29 / 76 µs | 31.6 / 43.1 / 52.5 / 103.2 µs |
| WebSocket | 24 / 35 / 41 / 76 µs | 44.8 / 59.4 / 68.7 / 112.8 µs |

"Send to handler" compares wall-clock microseconds across two processes, so
its resolution is 1 µs. One run each; differences of a few microseconds are
within run-to-run noise not yet measured.

CPU for 32 keys updated once a second (60 s): the test is
`cpu_pipe`/`cpu_ws` in the same file. Pipe: host process 0 ms and plugin
0 ms of CPU over 60 s with 2016 titles received, that is below the 10 ms
resolution of `/proc/<pid>/stat`. WebSocket: host process 10 ms (one tick) and
plugin 0 ms over 60 s, 2000 titles received. Both paths are at or below the
resolution of this method, so no difference between them is claimed; a finer
method (`getrusage` or `perf stat`) is needed to tell them apart.

# Contributing to Deckly

Short guide. The full project rules are in [AGENTS.md](AGENTS.md): read them
before your first change.

## Setup

- Install the stable Rust toolchain (`rustup`).
- Close the official Stream Deck app before running Deckly: both apps cannot
  drive the same device at once.

## Run

Development build (fast to compile, not optimized):

```sh
cargo dev
```

Release build (slower to compile, runs like the shipped app; use it to check
speed, since egui can feel slow in a debug build):

```sh
cargo dev --release
```

Run these commands from the repository root. The Rust `xtask` launcher first
builds the whole workspace, including `deckly` and its separate `deckly-web`
helper, and starts the editor only if the build succeeds. Plain `cargo run`
and `cargo run --release` from the root use the same launcher.

Close existing Deckly instances before rebuilding. Windows locks running
executables, so an old editor or helper can prevent a successful build.

`cargo run -p deckly` is a direct editor launch: Cargo builds only that
package and its dependencies, so it can leave `deckly-web` out of date.
Use the developer launcher for normal development. To build everything
without opening windows or starting plugins:

```sh
cargo dev --no-run
```

The launcher does not run the test suite automatically; use the checks below
before pushing. The web engine stays in the helper process; the launcher
adds no web dependencies to the editor.

Development switches:

- `DECKLY_NO_DEVICES=1`: open the editor without touching any device.
- `DECKLY_DEBUG=1`: print device events and action results.

Set them per shell. PowerShell:

```powershell
$env:DECKLY_NO_DEVICES = '1'
cargo dev
```

Bash: `DECKLY_NO_DEVICES=1 cargo dev`.

## Check before you push

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must pass. For UI changes, also run the app and look at the result.
For platform-specific code, compile-check Windows, Linux, and macOS when you
can, and say which platforms you did not test.

## Making a change

1. Read the relevant code and docs first. Keep the change small and focused.
2. Add tests for anything testable without hardware (protocol encoding, model
   validation, serialization round-trips).
3. Fix a crash together with a regression test.
4. Write one commit per logical change, with a short imperative message
   ("Add folders to Navigation actions").

## Rules to remember

- Rust only; UI with egui. No JavaScript, webview, or Electron in the app.
- No `unwrap`, `expect`, or `panic!` on data from devices, files, plugins, or
  users. A disconnected device or a corrupt profile must not crash the app.
- Everything user-visible is an action with a stable UUID.
- Icons: SVG or egui-phosphor only, never Unicode or emoji.
- Clean-room: do not copy code, assets, or text from the official app or other
  Stream Deck libraries. Log observations in `docs/clean-room/`.
- Do not claim support for a device or OS until it is tested and listed in
  [docs/devices.md](docs/devices.md).
- Do not imply Deckly is made or endorsed by Elgato or Corsair.

## License

By contributing you agree that your work is released under the MIT license.

<img src="assets/logo/deckly.svg" width="96" alt="Deckly logo">

# Deckly

An independent, open-source app for Stream Deck hardware, written in pure Rust.

> Deckly is unofficial and not made, approved, or supported by Elgato or Corsair.

**Status:** early alpha.

## Build

```sh
cargo run --release
```

Close the official Stream Deck app first: both apps cannot drive the same device at once.

Development switches: `DECKLY_NO_DEVICES=1` opens the editor without touching
any device, `DECKLY_DEBUG=1` prints device events and action results.

## Plugins

Deckly runs plugins written for the Stream Deck SDK unchanged, each as its
own process. Install one from the Plugins tab ("Install…" for a
`.streamDeckPlugin` file, "Add Folder…" for a `<plugin id>.sdPlugin`
folder) or drop the file on the window. Plugin actions then appear in the
Keys tab under the plugin's name.

- Node.js plugins need Node.js 20 or newer on `PATH` (or `DECKLY_NODE` set
  to the `node` program).
- HTML plugins and action settings ("Open Settings") run in `deckly-web`,
  a helper next to the Deckly program that uses the system web view
  (WebView2 on Windows, WebKitGTK on Linux, WKWebView on macOS). Windows
  builds made with the GNU toolchain also need `WebView2Loader.dll` next to
  it; the build copies it there.
- Native plugins run only on the system they were built for.
- Copies installed through the official marketplace are encrypted and
  cannot be used; get the plugin from its author's own download instead.

Plugins live in Deckly's config folder under `plugins`, their logs under
`logs/plugins`. For plugin authors, `DECKLY_PLUGIN_DEBUG=1` writes every
message to and from a plugin into its log, shows HTML plugin pages, and
enables the web view's developer tools.

## Standby screen

The standby screen is the picture a device shows while it has power but no
app is driving it. Deckly can change it (Preferences > Devices >
Advanced…): "Set Image…" uploads your own picture and "Reset" uploads
Deckly's default. The picture is stored on the device itself, so it stays
after you close Deckly. Deckly only changes it when you press one of these
buttons.

To get the original picture back, open the official app, go to its
Preferences > Devices > Advanced…, and press "Reset" under "Standby screen".
If you have a picture of your own, "Set Image…" in either app uploads it.

## Tested devices

See [docs/devices.md](docs/devices.md).

## License

MIT

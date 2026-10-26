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

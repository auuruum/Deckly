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

## Tested devices

See [docs/devices.md](docs/devices.md).

## License

MIT

# Releasing Deckly

A release is a `vX.Y.Z` tag on `main`. GitHub Actions
(`.github/workflows/release.yml`) builds every archive and drafts the
release; a person checks the draft and publishes it.

## What a release holds

| Asset | Contents |
| --- | --- |
| `deckly-x86_64-unknown-linux-gnu.tar.gz` | `deckly`, `deckly-web`, menu entry, icon, udev rules |
| `deckly-x86_64-pc-windows-msvc.zip` | `deckly.exe`, `deckly-web.exe` |
| `deckly-setup-windows-x64.exe` | Inno Setup installer (per user, no admin); used by `install.ps1` and the updater |
| `deckly-aarch64-apple-darwin.tar.gz`, `deckly-x86_64-apple-darwin.tar.gz` | `Deckly.app` (ad-hoc signed) |
| `SHA256SUMS` | Checksums of the archives, read by the install scripts |
| `deckly.rb` | Homebrew formula for the `auuruum/homebrew-tap` repository |
| `aur-deckly-bin.tar.gz` | `PKGBUILD`, `.SRCINFO` and install file for the `deckly-bin` AUR package |

Every archive also carries `LICENSE`, `README.md` and
`THIRD-PARTY-LICENSES.txt`, the license texts of the crates linked into
`deckly` and `deckly-web` for that target. Archive names carry no version,
so `releases/latest/download/<name>` always gives the newest release; the
install scripts rely on that.

Builds are not code-signed (no paid Apple Developer ID or Windows
certificate), so Gatekeeper and SmartScreen may warn on first start.

## Local commands

```sh
cargo dev dist                       # archive for this computer, in dist/
cargo dev dist --target <triple>     # archive for another installed target
cargo dev release-files --tag v0.0.1 --checksums dist/SHA256SUMS
```

`release-files` refuses a tag that does not match the workspace version in
`Cargo.toml`. Templates live in `packaging/`; edit those, never the
generated files.

To try an archive without publishing it:

```sh
DECKLY_ARCHIVE=dist/deckly-x86_64-unknown-linux-gnu.tar.gz DECKLY_PREFIX=/tmp/deckly-test DECKLY_NO_UDEV=1 sh install.sh
```

## Steps

1. Set the new version in `[workspace.package]` in `Cargo.toml` and run
   `cargo check` so `Cargo.lock` follows. Merge to `dev`, then `dev` to
   `main` (both need the owner's approval).
2. Tag `main` and push the tag: `git tag v0.0.1 origin/main` and
   `git push origin v0.0.1`.
3. Wait for the Release workflow. It leaves a draft release with all assets.
4. Download the archives for the systems at hand and check that Deckly
   starts and drives a device. Record what was tested in `docs/devices.md`.
5. Publish the draft on GitHub as a normal release, not a pre-release: the
   install scripts and Deckly's update check follow GitHub's latest
   release, which skips pre-releases. Running copies offer the update within
   a day.
6. Homebrew: copy the `deckly.rb` asset to `Formula/deckly.rb` in
   `auuruum/homebrew-tap`, commit and push.
7. AUR: unpack `aur-deckly-bin.tar.gz` into a clone of
   `ssh://aur@aur.archlinux.org/deckly-bin.git`, commit and push.

## winget (not set up yet)

Submit a manifest for the Windows zip to `microsoft/winget-pkgs`, for
example with `komac new` the first time and `komac update` for later
releases. winget installs zips as portable apps; Deckly's zip keeps both
programs in one folder, as `deckly.exe` expects.

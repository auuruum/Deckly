# FFmpeg downloads

"Download for Deckly" in the **Turn on video import** window fetches a
prebuilt FFmpeg into Deckly's data folder. Deckly downloads only the builds
pinned in `crates/deckly/src/ffmpeg/pins.rs`, checks the exact size and
SHA-256 before using one, and unpacks only the `ffmpeg` program. No FFmpeg
binary is ever committed to this repository or shipped with Deckly.

## Sources

| System | Build | Why |
| --- | --- | --- |
| Windows x86_64, arm64 | [FFmpeg-Builds](https://github.com/FFmpeg-Builds/FFmpeg-Builds) `winXX-lgpl-<branch>` `.zip` | Static, single `ffmpeg.exe`, LGPL variant, built daily from an FFmpeg release branch, published on GitHub releases with SHA-256 digests. |
| Linux x86_64, arm64 | FFmpeg-Builds `linuxXX-lgpl-<branch>` `.tar.xz` | Same project and variant; a static program that runs without the distribution's FFmpeg libraries. |
| macOS | none yet | FFmpeg-Builds makes no macOS builds, and no LGPL macOS build with a dependable, pinnable source was found. macOS uses Homebrew or a chosen FFmpeg. |

The static builds are larger than the shared ones (about 115–170 MB), but
they hold one file and need no library search path, so nothing else has to
be unpacked or set up.

FFmpeg-Builds keeps the last build of each month for two years and only the
last 14 daily builds, so the pins name a **month-end** build. Today's pins
come from `autobuild-2026-10-31-12-59` (FFmpeg n9.0.2) and stay downloadable
until about October 2028.

## Updating the pins

Do this at least once a year, or when a pinned build is about to expire.

1. Pick the newest month-end release (`autobuild-YYYY-MM-<last day>-…`) on
   the FFmpeg-Builds releases page.
2. List its static LGPL files of the newest FFmpeg release branch, with their
   size and GitHub's SHA-256 digest:

   ```bash
   gh api repos/FFmpeg-Builds/FFmpeg-Builds/releases/tags/<tag> --jq '.assets[] | select(.name | test("-lgpl-[0-9.]+\\.(zip|tar\\.xz)$")) | "\(.name) \(.size) \(.digest)"'
   ```

3. Download each file and confirm the digest yourself:

   ```bash
   curl -fLO --proto '=https' <url> && sha256sum <file>
   ```

4. Check that the archive holds `<top>/bin/ffmpeg` (`tar -tJf`) or
   `<top>/bin/ffmpeg.exe` (`unzip -l`), then update `RELEASE`, `VERSION`, the
   `url!` macro's release folder and every entry of `PINS` in `pins.rs`.
5. Run the checks. The ignored test compares every pin with the digests
   GitHub reports:

   ```bash
   cargo test -p deckly pins -- --include-ignored
   ```

6. Download through the window on each system you can, and note what was
   tested in the pull request.

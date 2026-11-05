#!/bin/sh
# Installs Deckly from its GitHub releases on Linux and macOS:
#
#   curl -fsSL https://raw.githubusercontent.com/auuruum/Deckly/main/install.sh | sh
#
# Uninstall:   curl -fsSL .../install.sh | sh -s -- --uninstall
#
# Settings (environment variables):
#   DECKLY_VERSION   a release tag such as v0.0.1 (default: the latest release)
#   DECKLY_PREFIX    Linux install prefix (default: ~/.local)
#   DECKLY_NO_UDEV=1 skip installing the Linux device rules (needs sudo)
#   DECKLY_ARCHIVE   install from a local archive instead of downloading
#
# Linux: programs go to $DECKLY_PREFIX/lib/deckly, with a `deckly` link in
# $DECKLY_PREFIX/bin, a menu entry and an icon. macOS: Deckly.app goes to
# ~/Applications. Your profiles and settings are never touched.

set -eu

REPO="auuruum/Deckly"

say() { printf 'deckly: %s\n' "$*"; }
fail() { printf 'deckly: error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

uninstall=0
for arg in "$@"; do
    case "$arg" in
        --uninstall) uninstall=1 ;;
        *) fail "unknown option $arg" ;;
    esac
done

os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Linux)
        case "$arch" in
            x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
            *) fail "no Linux build for $arch yet; build from source (see README.md)" ;;
        esac
        ;;
    Darwin)
        case "$arch" in
            arm64 | aarch64) target=aarch64-apple-darwin ;;
            x86_64) target=x86_64-apple-darwin ;;
            *) fail "no macOS build for $arch" ;;
        esac
        ;;
    *) fail "this script supports Linux and macOS; on Windows use install.ps1" ;;
esac

prefix=${DECKLY_PREFIX:-$HOME/.local}
apps="$HOME/Applications"
udev_rules=/etc/udev/rules.d/60-deckly.rules

if [ "$uninstall" = 1 ]; then
    if [ "$os" = Darwin ]; then
        rm -rf "$apps/Deckly.app"
    else
        rm -rf "$prefix/lib/deckly"
        rm -f "$prefix/bin/deckly" \
            "$prefix/share/applications/deckly.desktop" \
            "$prefix/share/icons/hicolor/scalable/apps/deckly.svg"
        if [ -f "$udev_rules" ]; then
            say "removing $udev_rules (asks for your password)"
            sudo rm -f "$udev_rules" || say "could not remove $udev_rules; remove it by hand"
        fi
    fi
    say "Deckly removed. Your profiles and settings are still in place."
    exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

download() {
    if has curl; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
    elif has wget; then
        wget -q -O "$2" "$1"
    else
        fail "needs curl or wget"
    fi
}

sha256() {
    if has sha256sum; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif has shasum; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

name="deckly-$target.tar.gz"
if [ -n "${DECKLY_ARCHIVE:-}" ]; then
    archive=$DECKLY_ARCHIVE
    say "installing from $archive"
else
    if [ -n "${DECKLY_VERSION:-}" ]; then
        base="https://github.com/$REPO/releases/download/$DECKLY_VERSION"
    else
        base="https://github.com/$REPO/releases/latest/download"
    fi
    archive="$tmp/$name"
    say "downloading $base/$name"
    download "$base/$name" "$archive" || fail "download failed; is there a release for $target?"
    download "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "cannot download the checksums"
    expected=$(grep " \*\{0,1\}$name\$" "$tmp/SHA256SUMS" | cut -d ' ' -f 1)
    actual=$(sha256 "$archive")
    if [ -z "$actual" ]; then
        say "no sha256sum or shasum; skipping the checksum check"
    elif [ "$expected" != "$actual" ]; then
        fail "checksum mismatch for $name; not installing"
    fi
fi

tar -xzf "$archive" -C "$tmp"
src="$tmp/deckly-$target"
[ -d "$src" ] || fail "the archive does not hold deckly-$target"

if [ "$os" = Darwin ]; then
    mkdir -p "$apps"
    rm -rf "$apps/Deckly.app"
    cp -R "$src/Deckly.app" "$apps/Deckly.app"
    # Files fetched by curl carry no quarantine flag; clear it anyway in case
    # the archive came from a browser download.
    xattr -dr com.apple.quarantine "$apps/Deckly.app" 2>/dev/null || true
    say "installed $apps/Deckly.app"
    say "open it from Launchpad or Spotlight. macOS asks for Accessibility access"
    say "the first time a key types text or presses shortcuts."
    exit 0
fi

lib="$prefix/lib/deckly"
mkdir -p "$lib" "$prefix/bin" "$prefix/share/applications" \
    "$prefix/share/icons/hicolor/scalable/apps"
# Replace rather than overwrite, so a running Deckly keeps its old files.
for exe in deckly deckly-web; do
    cp "$src/$exe" "$lib/$exe.new"
    chmod 755 "$lib/$exe.new"
    mv -f "$lib/$exe.new" "$lib/$exe"
done
ln -sf "$lib/deckly" "$prefix/bin/deckly"
# Desktop sessions often lack ~/.local/bin on PATH: point the menu entry at
# the program itself.
sed "s|^Exec=deckly\$|Exec=\"$lib/deckly\"|" "$src/share/applications/deckly.desktop" \
    > "$prefix/share/applications/deckly.desktop"
cp "$src/share/icons/hicolor/scalable/apps/deckly.svg" \
    "$prefix/share/icons/hicolor/scalable/apps/deckly.svg"
has update-desktop-database && update-desktop-database "$prefix/share/applications" 2>/dev/null || true
say "installed $lib"

if [ "${DECKLY_NO_UDEV:-}" = 1 ]; then
    say "skipped the device rules (DECKLY_NO_UDEV=1)"
elif [ -f "$udev_rules" ] && cmp -s "$src/udev/60-deckly.rules" "$udev_rules"; then
    :
elif [ -f /usr/lib/udev/rules.d/60-deckly.rules ]; then
    # A system package (such as deckly-bin) already installed them.
    :
else
    say "installing device rules to $udev_rules so Deckly can reach the Stream Deck"
    say "(asks for your password)"
    if sudo install -m 644 "$src/udev/60-deckly.rules" "$udev_rules" </dev/tty; then
        sudo udevadm control --reload </dev/tty || true
        sudo udevadm trigger </dev/tty || true
        say "device rules installed; replug the Stream Deck if Deckly does not see it"
    else
        say "could not install the device rules; see README.md, Linux"
    fi
fi

if has ldconfig && ! ldconfig -p 2>/dev/null | grep -q 'libwebkit2gtk-4.1'; then
    say "note: WebKitGTK 4.1 is missing; plugins with web pages need it"
    say "  Arch: sudo pacman -S webkit2gtk-4.1   Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-0"
    say "  Fedora: sudo dnf install webkit2gtk4.1"
fi

case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) say "note: $prefix/bin is not on PATH; start Deckly from the app menu" ;;
esac
say "done. Start Deckly from the app menu or run: deckly"

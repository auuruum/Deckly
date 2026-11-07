<img src="assets/logo/deckly.svg" width="96" alt="Deckly logo">

# Deckly

An independent, open-source app for Stream Deck hardware, written in pure Rust.

> Deckly is unofficial and not made, approved, or supported by Elgato or Corsair.

**Status:** early alpha.

## Supported devices

Deckly drives keys only for now: dials and touch screens do nothing yet.
Only the MK.2 has been run on real hardware, and Deckly has not been run on
macOS.

✅ Tested &nbsp;·&nbsp; 🟡 Should work, not tested &nbsp;·&nbsp; 🟠 Keys only,
not tested &nbsp;·&nbsp; ❌ Not supported

| Device | Status | Notes |
| --- | :-: | --- |
| Stream Deck MK.2 | ✅ | Windows 10, Linux (CachyOS, Hyprland) |
| Stream Deck (v2) | 🟡 | |
| Stream Deck MK.2 (scissor keys) | 🟡 | |
| Stream Deck 15-key module | 🟡 | |
| Stream Deck XL | 🟡 | |
| Stream Deck XL (v2) | 🟡 | |
| Stream Deck 32-key module | 🟡 | |
| Stream Deck Mini | 🟡 | Older protocol |
| Stream Deck Mini (v2) | 🟡 | Older protocol |
| Stream Deck Mini Discord | 🟡 | Older protocol |
| Stream Deck 6-key module | 🟡 | Older protocol |
| Stream Deck Neo | 🟠 | No touch sensors or info window |
| Stream Deck + | 🟠 | No dials or touch strip |
| Stream Deck + XL | 🟠 | No dials or touch strip |
| First Stream Deck (`0x0060`) | ❌ | Its protocol is not documented |
| Stream Deck Pedal | ❌ | |
| Stream Deck Studio | ❌ | Not recognized |

Untested models are built from Elgato's published HID documentation.
[docs/devices.md](docs/devices.md) records what was checked and how to test a
new model.

## Install

Ready-made builds are on the
[releases page](https://github.com/auuruum/Deckly/releases). The commands
below download the latest one, check its checksum and install it for your
user. Close the official Stream Deck app first.

Linux (x86_64) and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/auuruum/Deckly/main/install.sh | sh
```

On Linux this installs to `~/.local`, adds Deckly to the app menu and, with
your password, the device rules from [Linux and macOS](#linux-and-macos).
Plugins with web pages also need WebKitGTK 4.1. On macOS it puts
`Deckly.app` in `~/Applications`. To remove Deckly (your profiles stay):

```sh
curl -fsSL https://raw.githubusercontent.com/auuruum/Deckly/main/install.sh | sh -s -- --uninstall
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/auuruum/Deckly/main/install.ps1 | iex
```

This installs to `%LOCALAPPDATA%\Programs\Deckly` with a Start menu entry,
without administrator rights. The builds are not code-signed, so Windows
SmartScreen and macOS Gatekeeper may warn before the first start.

Deckly asks GitHub once a day whether a new version is out (Preferences >
General, next to the version; it can be turned off there). A new version is
only offered: choose Update Now to download, install and restart in one
click, or Skip This Version. Copies installed through a package manager (AUR,
Homebrew, winget) say which command updates them instead.

Homebrew (macOS):

```sh
brew install auuruum/tap/deckly
```

Arch Linux: the `deckly-bin` AUR package is coming; until then use the
script above, or build the package from the `aur-deckly-bin.tar.gz` release
asset with `makepkg -si`.

## Build

Run from the repository root:

```sh
cargo dev
```

This builds the whole workspace, including the separate `deckly-web` helper,
then starts the editor. A failed build stops the launch. For an optimized
build, use `cargo dev --release`. Plain `cargo run` and `cargo run --release`
from the repository root use the same launcher.

Close existing Deckly instances before rebuilding, especially on Windows,
where a running executable cannot be replaced: use Quit Deckly in the tray,
since closing the window leaves Deckly running. See
[CONTRIBUTING.md](CONTRIBUTING.md) for development switches and checks.

Close the official Stream Deck app first: both apps cannot drive the same device at once.

On a first start, Deckly looks for the official app on this computer (Windows and
macOS). If it finds your setup, one click brings over your profiles, pictures and
plugins, and the official app stays as it is. Preferences > General > Import
Everything… does the same later.

Development switches: `DECKLY_NO_DEVICES=1` opens the editor without touching
any device, `DECKLY_DEBUG=1` prints device events and action results.

## Tray

Closing the window keeps Deckly running in the tray, so keys and plugins keep
working. Click the tray icon or start Deckly again to bring the window back;
only one Deckly runs at a time. The icon's menu lists each connected device
with its profiles, so you can switch profiles without opening the window, and
offers Preferences and Quit Deckly, which ends the app.

Tested on Windows 10. On macOS and Linux this is compile-checked only so far.
A Wayland window cannot be hidden, so on Linux, under X11 and Wayland alike,
the window really closes and opens anew from the tray. Devices and plugins
keep running, but the editor starts over: its selection, copied action and
undo history are gone. The panel must support StatusNotifierItem; stock GNOME
needs the AppIndicator extension. Without a tray, closing the window quits
Deckly as before.

## Profiles

Preferences > Profiles offers Rename, Duplicate, Export, Add Page, Delete and
Import from a profile's context menu or the options button below the list.
Duplicate copies pages, folders, actions and pictures into an independent
profile. Add Page appends an empty page and selects it in the list.

Select a page to rename it or change its icon. Change Icon offers image upload,
the offline icon library and the image creator, with the same layers and
undo/redo as key images. Page icons default to a transparent background and are
saved with the profile. A page's context menu, the options button and the minus
button offer confirmed deletion; the last page is kept.

Export saves a portable `.decklyProfile` with the profile and its pictures.
Use Import in the same menu to bring it into Deckly on another computer; this
is Deckly's own format. The importer also accepts `.streamDeckProfile` and
`.streamDeckProfilesBackup`. If another profile for the same model has the
name, the importer chooses a free name using `copy`, `copy 1`, and so on.
Profiles with the same id are replaced, as shown before import; their links
from other profiles keep working.

## Folder Auto Exit

Open a folder and select its Parent Folder key to enable Auto Exit and set
the delay from 0 to 60 seconds. It returns one level after an action is
executed; 0 returns immediately. Opening the folder alone starts no timer.

The Folder Exit icon next to an action's title lets it follow the folder
rule, use its own delay, or stay in the folder. The reset icon beside the
folder rule restores all its actions to inheritance after confirmation.
These settings are saved with profiles and carried over when importing
original Stream Deck profiles and backups.

## Feedback marks

A key shows a failure mark when its action fails and a success mark after
an action ran (for example a finished Multi Action, or when a plugin asks
for one). While Auto Exit counts down, the folder's Exit key shows a ring
that fills up. Preferences > Style > Feedback Marks lets you hide each one,
or change the icon, colour, size and, for the failure and success marks,
how long they stay (0.25 to 10 seconds). Each has a preview and a Reset
button. The marks look the same on every profile and key; backups carry
them, profile exports do not. A key whose own picture cannot be read always
shows the warning sign.

## Linux and macOS

In an Open action, use **Applications…** to search installed applications by
name, then press Enter or double-click a result. Linux lists XDG desktop
entries, including Flatpak and Snap exports; Windows uses the system's
Applications folder (desktop and Store apps); macOS lists bundles in
`~/Applications`, `/Applications` and `/System/Applications`. Discovery runs
in the background. **File…** and **Folder…** select other targets, and the
text field still accepts commands. Choosing an app fills an empty key title
without replacing a custom title.

Linux desktop entries launch through `gio launch` (GIO must be installed),
so the desktop handles arguments, terminal apps and D-Bus activation.
The picker has been run on Linux; Windows and macOS are compile-checked.
The list shows the installed applications' icons, loaded in the background
for visible rows and cached while the picker is open. Missing or unreadable
icons use Deckly's generic application symbol.

All built-in actions run on Linux and macOS. Open takes a file, folder, URL,
program or `.desktop` file, or a full command with arguments, for example
`/path/to/python /path/to/script.py toggle "Desk Lamp"`. A command runs
directly, not in a shell: for pipes, use `sh -c "..."`.

On Linux, Hotkey and Text pick the way that works in your session:

| Session | Way | Setup |
| --- | --- | --- |
| Wayland: Hyprland, Sway and other wlroots compositors | virtual keyboard protocol | none |
| Wayland: GNOME, KDE | RemoteDesktop portal | the desktop asks once |
| X11 | XTEST | none |
| Fallback in any session | uinput | access to `/dev/uinput` |

The first three type any language, whatever layout is active. uinput presses
places on the keyboard, so it follows the active layout and types only what a
US keyboard can. The rules in `assets/linux/60-deckly.rules` give the
logged-in user access to Stream Deck devices and to uinput:

```sh
sudo cp assets/linux/60-deckly.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger
```

Multimedia talks to media players over MPRIS and to the volume of PipeWire,
PulseAudio or ALSA directly, so it works without any media key bindings. When
nothing answers, it presses the media key. Volume keys step by 5%.

The desktop keeps the Super key to itself, so hotkey recording cannot see it;
use the Super checkbox next to Record.

On macOS, Hotkey, Text and Multimedia need Deckly allowed under System
Settings > Privacy & Security > Accessibility. macOS has no Stop media key, so
that choice reports an error.

Tested on CachyOS with Hyprland: the virtual keyboard, XTEST through XWayland,
media players and volume. The portal path was tested against a stand-in
portal, not on a real GNOME or KDE. uinput is not tested yet. macOS is
compile-checked only.

## Copy and paste actions

Select a key and press Ctrl+C, then select a destination and press Ctrl+V
(Cmd+C / Cmd+V on macOS). Empty keys receive the action immediately;
occupied keys ask for confirmation before being replaced. Cancel or Esc
keeps the existing action. The key's context menu offers the same commands.

Copies include settings, title, title style, custom image path and a snapshot
of any nested folders. Pasted folders are independent of the original.
The Parent Folder key stays protected. Shortcuts in text fields continue
to copy and paste text, and action shortcuts are disabled during hotkey
recording and confirmation dialogs.

## Key images

Select a key, then use **Image…** to upload a picture, browse the offline
Phosphor icon library, or open **Create image…**. The library searches the
bundled icons by name and can show only logos. Change its background and
icon colors, then press **Set image**, or continue in the creator.

The creator combines up to eight text, icon and picture layers. **Add text**,
**Add icon** and **Add image…** create layers; the separate **Layers** list
selects an existing layer, and **Edit selected layer** changes its settings.
The top item in the list appears in front. Drag the
selected layer in the preview or edit its position and size; **Back** and
**Front** change the layer order. You can combine several uploaded logos
and add text on a background color. **Set image** applies the result to the
selected key; Cancel or Escape leaves it unchanged. **Reset** restores the
action's default image, including live plugin images. These key edits are
covered by Undo and Redo.

While the image dialog is open, its **Undo** and **Redo** buttons restore
draft edits, including layer additions/removals, order, text, colors,
position and size. Use Ctrl+Z/Ctrl+Y, or Cmd+Z/Cmd+Shift+Z on macOS. One
drag is one edit, and consecutive typing is grouped. Draft history keeps
up to 50 edits and 32 MiB of retained buffers; images share their pixels
between snapshots. It lasts until the dialog closes. Search fields keep
their own text undo. After **Set image**, the profile's Undo restores the
previous assigned image.

The creator accepts still images, including the first frame of animated
images, and saves a 288 × 288 PNG into the profile's image folder.
Uploading an animated GIF, WebP or APNG directly to a key keeps the
animation. Backups include the saved image.
Layer drafts last while the dialog is open. **Add current key image** starts
further work from the previously applied PNG as one picture layer.

The catalog, creator and renderer use shared Rust/egui code on Linux,
Windows and macOS; only file selection uses a native dialog. Windows and
macOS are compile-checked; visual acceptance of this workflow is pending.

### Video keys

Choose **Image… → Import video…** on a key to import a local MP4, M4V,
MOV, WebM or MKV file. This requires FFmpeg 7 or newer on the PATH inherited
by Deckly, with the source codec and the APNG encoder enabled. Deckly
center-crops the first ten seconds to a 96 × 96, 10 fps silent loop and saves
it with the profile. The loop plays on other installations without FFmpeg.
Input is limited to 64 MiB and 16,777,216 decoded pixels per frame.

### Key wallpaper

Open **Wallpaper** beside the page numbers. Choose **Profile** to set the
default, or use the **Page** or **Folder** tab to choose a local image.
**Use Profile** restores inheritance and **No Wallpaper** disables it for
the current page or folder. Zoom and drag adjust the image for the selected
scope. Deckly profiles and backups carry the wallpapers and overrides.

On a key press, the icon, title and any custom key image shrink together
over the fixed wallpaper. If an image already contains its own background,
that background moves with the image.

## Undo and redo

Ctrl+Z undoes the last edit in the current profile; Ctrl+Y redoes it.
On macOS use Cmd+Z and Cmd+Shift+Z. This covers placing, replacing,
moving and clearing key actions, built-in action settings, titles, images,
and adding or deleting pages, including their folders. Text fields keep
their own text undo while focused. New edits discard that profile's redo.

History lasts for the current session, with a shared limit of 100 edits and
32 MiB of snapshot buffers. Oldest entries are discarded at either limit.
It does not undo device preferences, profile creation/deletion, changes made
by plugins, or actions already executed on the computer. If a profile changes
outside the editor, its old history is discarded instead of overwriting it.

## Plugins

Deckly runs plugins written for the Stream Deck SDK unchanged, each as its
own process. Install one from the Plugins tab ("Install…" for a
`.streamDeckPlugin` file or a `.zip` download that holds one or a
`<plugin id>.sdPlugin` folder, "Add Folder…" for a `.sdPlugin` folder or
a folder holding one) or drop the file or folder on the window. Plugin actions then appear in the
Keys tab under the plugin's name.

- Node.js plugins run on the Node.js major their manifest asks for: a
  matching `node` on `PATH`, else Deckly downloads the official build from
  nodejs.org once (checked against its SHA-256 list) into `runtimes`.
  `DECKLY_NODE` forces one `node` for every plugin.
- HTML plugins and action settings run in `deckly-web`,
  a helper next to the Deckly program that uses the system web view
  (WebView2 on Windows, WebKitGTK on Linux, WKWebView on macOS). Windows
  builds made with the GNU toolchain also need `WebView2Loader.dll` next to
  it; the build copies it there.
- On Windows, selecting a plugin key opens its settings page in the lower
  panel, with a label identifying the plugin. On Linux and macOS, use
  "Open Settings" to open a separate window. The web engine stays in the
  helper process on every platform.
- Plugin settings pages can open related configuration windows, preserving
  their connection to the original page. Closing a configuration window
  releases its web view. These windows close with their inspector.
- HTML plugins use a background web view while enabled; closing their
  settings window does not stop the plugin itself.
- A plugin's code comes from `CodePathWin`, `CodePathMac` or `CodePathLin`
  for this system, then `CodePath`, then another system's path: JavaScript
  and HTML plugins run anywhere. Paths ignore case, as they do on Windows.
- Windows programs run through [Wine](https://www.winehq.org) on Linux and
  macOS, when it is installed, in a Wine prefix of Deckly's own in `runtimes`.
  `DECKLY_WINE` forces one Wine. This is best effort: a plugin that needs
  Windows itself fails on its own, with the reason in its log. Programs built
  only for macOS cannot run on Linux.
- Copies installed through the official marketplace are encrypted and
  cannot be used; get the plugin from its author's own download instead.
- Some plugins refuse to run outside the official app on purpose: they
  check that the process that started them is the official, signed
  executable. Deckly does not work around such checks, so these plugins
  start but stay disabled (BarRaider's plugins, for example, show "Device
  Not Supported"). Deckly marks the ones it recognizes as "Works only in
  the official app". Only the plugin's author can change that.

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

## License

MIT

# Profiles by active window

- Date: 2026-11-02
- Method: public OS documentation (Microsoft `SetWinEventHook`,
  `QueryFullProcessImageNameW`, `GetApplicationUserModelId`,
  `SHGetPropertyStoreForWindow`; freedesktop EWMH; Apple `NSWorkspace`), a
  read-only listing of the Start menu's applications and of top-level windows on
  the owner's Windows 10 22H2 machine, Deckly source inspection and Rust tests.
  The official application was not inspected for this entry; the only thing
  taken from it is an earlier observation of its Preferences window, which has
  an application picker for automatic switching on the Profiles tab
  (`2026-10-26-preferences.md`).
- Hardware: none opened. No window was moved or focused and no input was
  injected.

## Decisions

The feature follows the owner's brief, not observed behavior of the official
app. Whether that app does the same in each case below is not known.

Rules live on the profile (`Profile.applications`), not in the settings. A rule
then goes when its profile goes, so it cannot point at a deleted profile or
another model, and it travels in a `.decklyProfile` and in backups. A rule holds
a display name and an id: the last part of a path, or a package, bundle or class
name. A full path would break on the next update and on another computer.

A rule also keeps where its picture is on this computer, for the list in
Preferences: the picker's icon source, or the chosen file. It only draws the
row, and a source that is empty or gone leaves the plain window icon, so
nothing depends on it. An exported profile drops a source that is a full path,
which names the computer's folders and its user. Windows draws only
`shell:AppsFolder\` entries and paths on a drive: a shared profile must not make
Shell open a network location. On Linux a chosen program has no picture of its
own, only desktop entries do. Tests: `icons_come_from_this_computer_only`,
`a_chosen_program_gives_its_own_picture`,
`an_exported_profile_keeps_no_full_path_to_an_icon`.

A copy of a profile (Duplicate) does not take the rules along. Two profiles of a
model listing one application: the first in the profile list wins. Test:
`the_first_profile_in_the_list_wins_an_application_two_list`.

The setting `switch_by_window`, Preferences > General > Active Window, is on by
default and older settings files load as on. A profile lists nothing until the
user adds an application, so the feature does nothing unasked, and a second
switch would only make a new rule silently not work.

A rule's profile lies over the device's assignment in memory and is never saved
as an assignment. Going back is dropping it, and a restart finds the user's own
choice. Focus moves between devices of a model together; each device keeps its
own assignment. Tests: `a_rule_shows_its_profile_and_another_application_goes_back`,
`going_from_rule_to_rule_still_returns_to_the_chosen_profile`,
`devices_follow_on_their_own`,
`a_rule_shows_on_the_device_and_in_the_tray_without_being_saved`.

Every switch by hand, from the main window, the tray, a key or a plugin, goes
through `State::assign`, which replaces the assignment and drops the rule. It
stays until the next foreground change. Tests:
`a_switch_by_hand_stays_through_the_next_application`,
`a_switch_by_hand_wins_until_the_foreground_changes`,
`picking_the_shown_profile_by_hand_keeps_it_after_the_application`,
`a_switch_by_hand_ends_the_rule_and_stays`.

The foreground counts once it stayed for 250 ms, and the same application
reported again is no change. Tests: `rapid_flips_only_count_where_they_end`,
`returning_to_the_same_application_changes_nothing`.

Deckly's own windows and those of `deckly-web` are dropped before anything else
sees them, so opening Preferences or a plugin page switches nothing. Test:
`deckly_and_its_helper_are_its_own_windows`. A rule added while the same
application is still the last one seen takes effect when it is reported again.
Test: `edited_rules_count_on_the_next_report_of_the_same_application`.

A device that is off or asleep is left alone and catches up when it comes back,
with where the foreground ended and not with every step. Reports that arrive
while the computer is locked are dropped: the lock screen must not count as an
application without a rule, or unlocking would flip the device twice. Tests:
`a_sleeping_device_is_left_alone_and_catches_up_when_it_wakes`,
`a_locked_computer_holds_devices_where_they_are`,
`a_disabled_or_absent_device_is_left_alone`,
`the_lock_screen_does_not_count_as_an_application`,
`a_device_left_behind_catches_up_with_where_the_foreground_ended`.

## Platform evidence

An application is reported as a list of names; a rule matches any of them by
last path part, ignoring case. Test: `a_rule_matches_by_the_last_part_of_a_name_in_any_case`.

### Windows

- Documented: `EVENT_SYSTEM_FOREGROUND` through an out-of-context
  `SetWinEventHook` needs only a message loop on the thread that set it.
- Observed on Windows 10 22H2 by listing the Start menu's applications and the
  top-level windows, read only:
  - Most desktop entries carry a link target with the program. A self-updating
    app's target can be a stub in another folder than the program that runs:
    `Discord\Discord.exe` against `Discord\app-1.0.9266\Discord.exe`. Only the
    file name is shared, which decided the rule format.
  - Packaged apps have no link target; the entry's id is their app id. Hosted
    ones run in `ApplicationFrameHost.exe`, and their frame window carries the
    app id, which the entry's id matches. Packaged desktop apps carry it on
    the process and not on the window.
  - Steam games are `steam://` links with no program, so nothing ties them to a
    process. The picker says so, and Choose File covers them.
- Observed through tests on the same machine: the hook is set and reports the
  current foreground, with a packaged process's app id; the taskbar is named by
  its program; a synthetic foreground event sent to the taskbar window reaches
  the report; a stale window and a window of a protected process give no error.
  Tests: `the_taskbar_is_named_by_its_program`,
  `a_window_of_a_closed_process_gives_no_error`,
  `a_window_that_is_gone_has_no_names`, `a_foreground_event_reaches_the_report`
  (ignored by default: a running Deckly would hear it).
- Not observed: a real focus change, a real elevated window, a real alt-tab, a
  real lock, or the switch on a device. Elevated windows are expected to be
  readable with limited query rights; if not, they count as unnamed.
- Unknown: whether a foreground event always follows an unlock; with none, a
  changed foreground counts only from the next change. How long Windows keeps
  the alt-tab window itself in the foreground: a hold longer than 250 ms makes
  `explorer.exe` count as an application for a moment.

### Linux

- Documented (EWMH): the root window's `_NET_ACTIVE_WINDOW`, with
  `_NET_WM_PID` and `WM_CLASS` on the window. Changes arrive as property events.
- Wayland has no common way to learn the active window. A session with
  `WAYLAND_DISPLAY` set or `XDG_SESSION_TYPE=wayland` reports that as an error,
  shown next to the setting and under the rule list, and nothing is watched.
  XWayland would only see part of the windows.
- A desktop entry's id is `StartupWMClass`, else the desktop file's name.
  Inferred: most entries are named after their class. Those that are neither
  do not match; Choose File takes the program instead.
- Only compile-checked. Tests under `cfg(target_os = "linux")` were not run.
- Unknown: window managers that do not set `_NET_ACTIVE_WINDOW` stay silent,
  with no error.

### macOS

- Documented: `NSWorkspaceDidActivateApplicationNotification` on the workspace's
  own notification center, and `frontmostApplication`.
- Reported names: bundle id, executable path, bundle path. Rules from the picker
  match on the bundle's folder name.
- Only compile-checked. Nothing here has run on a Mac.
- Unknown: whether `frontmostApplication` is always up to date inside the
  notification. If not, the report names the application before it.

## Not known

Official imported profiles keep no rules: Deckly does not read whatever the
official format stores for this. How the official application goes back, picks
between profiles or treats its own window.

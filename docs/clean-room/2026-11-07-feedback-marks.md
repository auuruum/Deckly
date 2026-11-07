# Feedback marks

Follows [Plugin host](2026-10-28-plugin-host.md) (`showAlert`, `showOk`),
[Multi Action and Key Logic](2026-10-30-multi-actions.md) (the alert on a
failed or empty sequence) and [Folder Auto Exit](2026-11-02-folder-auto-exit.md)
(the countdown ring).

- Date: 2026-11-07.
- Method: Deckly's own settings, renderer, runtime, backup and Preferences
  code; the earlier notes linked above. The official app was not run and its
  documentation was not consulted again for this step. No proprietary code,
  profiles, icons or other assets were used.
- System: Linux/CachyOS (Hyprland, the app under XWayland). Devices were not
  opened (`DECKLY_NO_DEVICES`); no device or firmware was involved.

## Observed

- Earlier notes only. The official 7.6 Preferences > Style tab
  ([Preferences](2026-10-26-preferences.md)) has a Multi Action progress
  overlay check box, global title settings for keys and dials, and
  Apply/Revert. It has no controls for the alert mark, the OK mark or the
  Auto Exit ring.
- The official profile data recorded so far
  ([Official profiles](2026-10-31-official-profiles.md),
  [Folder Auto Exit](2026-11-02-folder-auto-exit.md)) stores no look for
  these marks on a profile, page or action. The action-level `AutoExit`
  object holds only an enabled flag and a delay.
- The SDK's `showAlert` and `showOk` commands take only a context: a plugin
  can ask for a mark but not choose its look or how long it stays.

## Inferred

- In the official app the marks look the same everywhere and the user cannot
  change them. This is drawn from the absence of controls and stored fields
  above, not from a statement by Elgato.

## Unknown

- The exact look and timing of the official marks: icon, colour, size, how
  long the alert and OK marks stay, and the ring's radius, width and colours.
  Deckly's defaults are its own.
- Whether a later official version adds settings for them.

## Deckly decisions

- **Global only.** The marks are styled in Preferences > Style and apply to
  every profile, page, folder and key. A profile or key cannot override them.
  Reasons: the official app shows no per-profile or per-key control or field
  for them (above), the marks tell the user what happened rather than
  decorate a key, so they should read the same on every profile, and one
  setting keeps the renderer, the runtime and the editor simple. Per-profile
  overrides can be added later in the same way as the wallpaper
  (global, then profile) if a need shows up.
- Settings hold `feedback_marks` with three entries:
  - `failure` and `success`: shown or hidden, an icon, a colour, a size
    (30 to 90% of the key) and a duration (0.25 to 10 s).
  - `folder_exit`, the Auto Exit ring: shown or hidden, a colour and a size
    (its diameter, 50 to 100% of the key). It has no icon, and no duration
    of its own: it lasts as long as the folder's Exit after delay.
- Icons are stored by name and chosen from a fixed list of Phosphor icons
  that ships with Deckly. A name not on the list, a missing field or a value
  out of range falls back to that mark's default or is clamped. Each mark
  has its own defaults, so a partial entry in a hand-edited file keeps the
  right icon. Older settings files load with the defaults, which look as
  before this change.
- A hidden mark is still recorded: a Multi Action that had a failing step
  still counts as failed. It is not drawn, does not change the key's image
  hash, so nothing is sent to the device for it, and does not wake the event
  loop. A hidden ring also stops the per-percent wakeups of a countdown; the
  folder still closes on time.
- A key whose own picture cannot be read keeps the fixed warning mark. That
  mark reports a broken file, not the result of an action, so the failure
  settings do not change or hide it.
- The duration applies to marks shown after the change.
- Backups carry the settings as part of `settings.json`. A profile export
  does not, like the global wallpaper.

## Validation

- Tests: `round_trip`, `missing_fields_take_each_marks_own_default`,
  `bad_values_are_clamped_or_replaced`, `sanitize_fixes_values_set_in_code`,
  `durations`, `defaults_use_listed_icons` (core feedback);
  `older_settings_files_show_the_default_feedback_marks`,
  `feedback_marks_are_brought_within_limits` and the extended `round_trip`
  (settings); `hidden_marks_are_not_drawn_and_send_nothing`,
  `marks_are_drawn_in_their_style`,
  `every_listed_mark_icon_has_its_own_glyph`,
  `a_broken_picture_keeps_its_warning_when_failure_marks_are_hidden`,
  `the_exit_ring_follows_its_style_and_can_be_hidden` (render);
  `marks_last_as_long_as_the_settings_say`,
  `a_hidden_failure_mark_still_ends_a_run_as_failed` (runtime);
  `a_hidden_ring_wakes_the_event_loop_only_to_close_the_folder`
  (folder exit); `feedback_marks_go_into_the_backup_and_come_back` and the
  extended `an_exported_profile_carries_no_global_wallpaper` (backup).
- Run on Linux with a throwaway configuration folder and
  `DECKLY_NO_DEVICES=1`: the Style tab scrolls with a visible bar and shows
  the Feedback Marks section with three preview keys. Hiding the failure
  mark removed it from its preview and disabled its controls; picking another
  icon, dragging the size and duration sliders, and choosing a ring colour in
  the colour picker changed the previews and were written to the settings
  file (slider drags only once released). Reset brought each mark back to
  its defaults. The font menu of the title style still opened inside the
  scrolling tab.
- Compile checks: Windows x86-64 (MSVC) and macOS arm64.
- Not verified: the marks on a real device, the marks on the editor canvas
  after a real action (no device or plugin was used; the editor and device
  share the renderer the tests cover), and Windows and macOS at runtime.

# Folder Auto Exit

- **Date:** 2026-11-02
- **Target:** Stream Deck for Windows 7.6.0.23012, installed `StreamDeck.exe`;
  SHA-256 `2963df27b13f97e5736e3f931348d89d2f693109f3cb9dc3c9899300e35ea9e3`.
- **Host:** Windows. The owner's profile data includes MK.2 layouts. Device
  firmware was not read; this investigation did not operate the hardware.
- **Sources/methods:** owner's screenshot of Exit's inspector; read-only
  inspection of `AutoExit` fields in the owner's installed profiles; Elgato's
  [Auto Folder Exit guide](https://help.elgato.com/hc/en-us/articles/22589649057933-Elgato-Stream-Deck-Auto-Folder-Exit);
  REA 6.7.0 / Ghidra 12.1.4 defined-string inventory of the installed binary.
  Ghidra's initial 330-second deadline expired; a separate CLI run with a
  2400-second startup allowance completed. No REA configuration was changed.
- **REA Evidence:**
  `ev_96bfa60a9d3c105b711712b5fd77d37f509d813093cf10e5718502e702647d2e`,
  `search_strings` with the expression
  `AutoExit|autoExit|Auto Exit|Exit after|auto_exit`. Complete Evidence and the
  immutable snapshot are retained outside the repository in the task artifacts.
  The inventory covers Ghidra-defined strings, not all possible artifact text.
  This is static evidence, not observed execution or recovered source code.

## Observed

- The supplied screenshot shows an Auto Exit switch, a circular reset control,
  and an Exit after field displaying 3 seconds on a folder's Exit key.
- The public guide defines a delay of 0–60 seconds after an action in the
  folder is executed. Zero returns immediately. Individual actions can inherit
  the folder's rule or explicitly enable/disable Auto Exit with their own delay.
- Installed profiles store the action-level object
  `AutoExit: { "Enabled": <bool>, "Delay": <integer> }` alongside `Settings`,
  not inside plugin settings. Both enabled and disabled objects retain their
  delay. Observed delays include 0, 1, 2, 3, 5 and 10; absent objects occur too.
- REA reports reset-related text at `0x141675d38` describing restoration of
  every action to the folder rule. Confirmation text at `0x14166bb00` describes
  overwriting custom action rules. Only these meanings were retained here;
  proprietary strings, code, images and profiles were not copied into the tree.

## Inferred

- A missing action-level `AutoExit` means inheritance; a present disabled
  object opts the action out. This combines the public guide with the stored
  shape. The ordinary-action shape is covered by invented import fixtures;
  the owner's current explicit objects were on parent-folder actions.
- The reset beside the parent rule clears individual overrides while retaining
  the parent's own enabled flag and delay. Its wording specifies actions in
  that folder; Deckly applies this to that folder's direct keys.

## Deckly implementation and verification

- `KeyConfig.auto_exit: Option<AutoExit>` stores the policy independently of
  plugin settings. `None` inherits for an ordinary key and disables Auto Exit
  for a parent key. Legacy profiles keep their behavior. Delays are bounded.
  Core tests cover sanitization, profile round trips and independent copies.
- The parent inspector edits the folder default. An action's Folder Exit popup
  edits inheritance or an override. Reset asks for confirmation, clears only
  the current folder's overrides, and supports Undo. The four
  `app::auto_exit_tests` exercise these controls, cancellation and reset scope.
- The runtime schedules one deadline after a normal key's release, a completed
  sequence, or the chosen Key Logic action. Entering a folder alone starts
  nothing. The deadline returns one level and does not cascade. Navigation and
  page indicators do not schedule an exit. These event-order choices keep a
  zero-delay exit from interrupting plugin releases or an unfinished sequence.
- Newer presses supersede old deadlines and unfinished-action completions.
  Navigation, replacement, disconnection and policy changes invalidate stale
  sources. Releases retain their original action/context after navigation.
  `runtime::folder_exit::tests` covers these cases with controlled timestamps,
  actual key-event routing, Multi Action completion/cancellation and Key Logic.
  The existing blocking event loop wakes at the deadline, including while the
  window is hidden; no polling thread was added.
- The official importer preserves parent rules and individual overrides from
  profile folders, `.streamDeckProfile` and `.streamDeckProfilesBackup`.
  `official::tests::*auto_exit*`,
  `folder_exit_and_action_overrides_are_imported_and_survive_saving` and
  `transfer::tests::auto_exit_survives_official_profile_backup_and_folder_imports`
  use invented fixtures. A temporary read-only check against the owner's data
  converted 7 supported profiles and compared all 57 retained Auto Exit
  controls without differences; the temporary test and real data were not kept.
- The Windows app was run with an isolated profile and `DECKLY_NO_DEVICES=1`.
  Parent controls and the action popup were visually checked at 852 × 742 using
  the app's own wgpu screenshot output after native Windows capture timed out.
  Temporary capture hooks were removed. Windows tests and clippy passed;
  Linux x86-64 and macOS arm64 app targets were compile-checked.

## Unknown / not verified

- The original application's exact key-down/key-up, overlapping-key and
  sequence-completion ordering was not observed. No function pseudocode was
  reused. Deckly's event-order rules above are deliberate implementation choices.
- This feature was not exercised on a physical device or in a Linux/macOS
  runtime. Compile checks do not establish runtime or hardware verification.

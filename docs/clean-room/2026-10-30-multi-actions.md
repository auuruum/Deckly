# The official app's Multi Action and Key Logic actions

- **Date:** 2026-10-30
- **Target:** Stream Deck app for Windows 7.6.0.23012, `StreamDeck.exe`
  SHA-256 `2963DF27B13F97E5736E3F931348D89D2F693109F3CB9DC3C9899300E35EA9E3`
- **Host:** Windows 10 Pro 22H2
- **Device:** Stream Deck MK.2 (15 keys)
- **Methods:**
  1. Runtime observation: five screenshots of the action list, the Multi
     Action, Multi Action Switch and Key Logic editors and the delays popup,
     taken by the owner. Not stored in the repository.
  2. Reading the manifests of the built-in action plugins in the install
     folder (`…multiactions…`, `…keys…` and all others): action names, UUIDs,
     support flags. No image was copied.
  3. Reading the owner's own profiles under `%APPDATA%\Elgato\StreamDeck\ProfilesV3`
     (structure, field names, delay values only).
  4. Static analysis with Ghidra 12.1.4 of the classes that run these actions,
     started through the REA workflow and finished in headless mode. Pseudocode
     stayed outside the repository and only behavior is recorded here. Nothing
     was executed in the target.
  5. Public documentation: Elgato's articles on Multi Actions and Key Logic.
- **Implemented in:** `crates/deckly-core/src/profile.rs`,
  `crates/deckly/src/sequence.rs`, `crates/deckly/src/runtime.rs`,
  `crates/deckly/src/actions.rs`, `crates/deckly/src/plugins.rs`,
  `crates/deckly/src/app.rs`, `crates/deckly-plugin-host/src/protocol.rs`

## Observed

### Action list and data

- The Multi Action category lists Multi Action, Multi Action Switch, Random
  Action, Key Logic. Delay is in that category only while a Multi Action is
  open. Checked by `actions::tests::lists_follow_the_official_support_flags`.
- Manifests flag what can go where (`SupportedInMultiActions`,
  `SupportedInKeyLogicActions`, both default to true). The holders themselves
  and Create Folder are in neither. Previous Page, Next Page, Go to Page and
  Page Indicator are not in Multi Actions but are in Key Logic. Delay is not in
  Key Logic.
- A Multi Action key stores two lists of actions and a Switch uses both. Key
  Logic stores three actions in the order single press, double press, hold.
  Each action in a list can carry two delays and, with more than one state, the
  state to use. Delay's own setting is a number of milliseconds.
- Custom delays are 1 to 100 ms in both the slider and the spin box, both
  100 ms by default. "Revert" drops the custom values. Public documentation
  describes the first as the time from virtual press to release and the second
  as the pause before the next action. Checked by
  `sequence::tests::delays_default_to_the_official_values` and
  `profile::tests::group_lists_are_repaired`.
- The editor replaces the key grid: a back arrow and title, a two-part 1/2
  control for a switch, rows with an icon, "Category: Name" and a drag handle,
  and the blue selected row. Key Logic shows three key-shaped slots labelled
  Single Press, Double Press, Hold. A selected row's inspector has a clock
  button and a bin.

### Running a sequence

- A Multi Action key does nothing on press and starts on release. Checked by
  `runtime::tests::a_release_starts_a_run_and_a_press_cancels_it`.
- Each action gets a virtual press, the first delay, a virtual release, then
  the second delay (at least 1 ms). Delay waits for its setting in the second
  place. Checked by `sequence::tests::a_run_presses_and_releases_each_action_in_order`.
- An action that cannot run at that moment is skipped, not treated as a failure.
- Pressing the key while it runs cancels the run, and the release of that press
  does not start another. Checked as above.
- Random Action picks one action of its list at random and runs it in the same
  way. Checked by `sequence::tests::a_random_action_runs_one_action_and_reaches_them_all`.
- A switch runs the list of its current state and moves to the next state only
  when the run finished; a cancelled run leaves it. Checked by
  `plugins::tests::a_finished_run_turns_a_switch_and_a_cancelled_one_does_not`.
- A finished run shows the OK mark, unless the Show progress overlay preference
  is on; then the key shows a percentage of actions done while it runs.
- An action in a list with several states is pressed with the state chosen for
  it. Actions in lists are told about before they are pressed, once the key
  holding them is set up.

### Key Logic

- Press starts two timers, 300 ms and 500 ms. Checked by the
  `sequence::tests` for `Press`.
- A second press while both run is a double press.
- The key being up when the 300 ms timer ends is a single press.
- The key still being down when the 500 ms timer ends is a hold.
- Single press taps its action for 60 ms. Double press and hold press their
  action, which is released when the key comes up.
- A slot with no action shows the alert mark. Checked by
  `runtime::tests::key_logic_runs_the_action_of_a_double_press` for the running
  path and by `sequence.rs` for the timing.

## Inferred

- The official app may cancel a running sequence when its key is torn down: the
  method that does so looks like the counterpart of the one called when a key
  is set up. Which page and profile changes reach it was not determined.
- A press released between 300 and 500 ms appears to trigger nothing in the
  official app: neither timer's condition is met.
- An empty Multi Action fails the validity check and probably shows the alert.

## Deckly's own choices

- `KeyConfig::groups` holds the lists: one for Multi Action and Random Action,
  two for a switch, three (of one action each) for Key Logic. Loading repairs
  the count, drops what cannot be held, and bounds a list at
  `MAX_GROUP_ACTIONS`.
- A press released between 300 and 500 ms is a single press, so no press is
  lost. Checked by `sequence::tests::a_press_released_between_the_two_times_is_a_single_press`.
- A run keeps going when the page or profile changes, so a Multi Action can
  switch profile and go on.
- A cancelled run still sends the release of the action it was holding, so a
  plugin never sees a key stuck down.
- A switch's side is the `side` setting and is drawn with a toggle icon; both
  sides share one title and image.
- Key Logic actions are not marked `isInMultiAction` and keep their key's
  coordinates, since no evidence says otherwise.
- The state of a many-state plugin action is picked with a row in its inspector.
- Copying a row and pasting it after the selected row, or into a Key Logic
  slot, goes through the existing key clipboard. A Delay can't be pasted on a
  key.

## Unknowns and next steps

- Not tested on hardware: the timing was checked by unit tests only.
- The Show progress overlay preference and the percentage on the key are not
  implemented.
- A switch's per-state title and image are not implemented.
- A manifest's own default for the two delays exists in the official app and
  is not read yet.
- The official app's Action Wheel and Action Trigger (dial actions) are not
  covered here.
- Importing official profiles with these actions is not implemented.

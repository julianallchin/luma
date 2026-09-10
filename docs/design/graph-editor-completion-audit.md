# Graph redesign completion audit

This audits the full goal. The library browser imports copies into canonical
scores, and the obsolete native editor has been deleted. The viewport controls, score lifecycle, library import and mapping inspector
have been verified in the native harness and the real desktop window. The full
redesign scope is complete; adjacent limitations are recorded below.

## Current evidence

| Requirement | Evidence inspected | Status |
| --- | --- | --- |
| One numerical evaluator; structured controls and tensor broadcasting | `backend/crates/patterns/src/runtime`, `tensor.rs`, `prepared.rs`; the old `backend/src/eval/ops` and category compiler files are deleted. The remaining host Plan delegates to PreparedGraph. All 162 core tests pass, including rejection of resolved head IDs in saved event literals. | Proved for computation |
| Compact mathematical operations; editable Envelope replacing ADSR | `envelope.rs`, `event_tensor.rs`, `recipes.json`; ADSR appears only in saved-format conversion and reference cases. Migration tests assert one Envelope sampler and no exposed stage controls. | Proved |
| Five compositions | `tests/foundation_compositions.rs` exercises independent Chase lifetimes, triangle/ramp mover remapping, per-head shimmer, phase-offset circles and a wrapped palette ribbon. It runs in the passing core suite. | Proved numerically |
| Immutable global/targeted events; deterministic selection and seeking; Max overlap | `event_targets.rs`, `event_tensor.rs`, and their tests cover event/head/seed identity, independent cohorts, reordered heads and nonsequential time batches. | Proved numerically |
| Chase travel independent of repeat; gaps, overlaps, shape and boundaries | `tests/event_tensors.rs`, `spatial_recipes.rs` and the foundation composition test exercise the short/equal/long cases and existing spatial baselines. | Proved numerically |
| Custom UVZ vectors plus presets; independent mirror plane and offset | `mapping.rs`, `tests/mapping.rs`, the native Input test and `gpui/crates/ui/src/arg/mapping.rs` cover these controls, invalid vectors and persistence. | Proved in core/native tests and the full physical mapping inspector |
| Reusable numerical operations for laser samples; identify extra laser work | The sixth foundation test transforms sampled path vectors with the shared math. The design ledger identifies missing scan-point domain and output adapters. No hardware output is claimed. | Proved for the requested mathematical reuse |
| Input cards, inference, sharing, safe renaming and inspector-only controls | Core Input tests and native `graph_inputs` tests pass. A native window capture shows the name-and-socket Input card. The old card-body widgets and the Legacy source branch are deleted. | Proved for the canonical editor and admitted entry points |
| Explicit capability Output; bundled lighting internal | Core `signal_output` tests and native `graph_output` tests pass. Old numerical graphs lower to capability ports and an Output terminal. Missing or unsupported implementations report an explicit error and retain their saved rows. | Proved for the canonical editor and admitted entry points |
| Wiring, compatibility, cursor search, Space, edge Delete and undo | All 23 serial native graph tests pass; closing/reopening and switching scores pass a strengthened lifecycle check. Physical X11 input exposed and then verified a fix for search autofocus. Every graph entry point uses the same canvas and score history. | Proved for the canonical editor and admitted entry points |
| Visualizer transport with real clip context and temporary adjustments | Native graph preview tests verify timing, selection, overrides, loop/stop, errors and unchanged persisted score. Real window captures show scrubbing, playback and fixture output. The old static strip and its image-preview/save/reload paths are deleted. | Proved for the canonical editor and admitted entry points |
| Migrate definitions, exposed bindings and overrides safely | Core versioned migration tests, 31 backend node migration tests and 16 authored-score persistence tests pass, including history/CAS, rollback, mixed row conversion and shared overrides. `upgrade_rows` preserves unsupported or missing implementations instead of publishing partial scores. | Proved; missing implementations retain their source data and report an error |
| Rendering and product review | Current real renderer integration passes and images were inspected. The native app was launched against a SQLite backup of a disposable harness fixture on its own Xvfb display; current graph/search/Input/preview screenshots were inspected. | Proved by renderer assertions, inspected images and physical desktop interaction |

## Verified fixes in this checkpoint

Python's separate PatternDraft builder and host creation/check commands were
deleted. Authoring uses `luma.track.edit().graph(...)`, keeps definitions in the
score, roundtrips, renders, applies through authored history and retries
idempotently. The real worker integration verifies no additional Pattern row is
created. Python gradient literals retain alpha in defaults and clip overrides;
unknown stop fields reach Rust validation instead of disappearing.

Sample Gradient returns `color` and `opacity` separately. Tests verify fixture
identity, frame dimensions, opacity values, unchanged RGB, explicit Dimmer wiring
and retaining auxiliary outputs when a numerical effect is placed on a clip.
Placement rejects mistyped capability outputs and name collisions. Existing
color-only connections do not acquire an implicit opacity multiplier.

Search had focused its field before the popup was mounted. The native window
ignored immediate typing until the user clicked the field. Focus now transfers
after the popup's first frame, provided the menu and owning tab remain active.
The interaction test now sends keystrokes directly after right-click and Space;
it no longer uses `app.type`, which clicks the field before typing.

## Final verification

The latest binary's viewport controls were exercised physically. `100%` centers
the selected node at readable size; Fit restores the full overview without
clearing selection. Expand / Show chat uses a visible control. Native tests verify
both paths. Closing a timeline or switching its score closes dependent graph views;
reopening restores persisted edits. Missing implementations remain preserved and
visibly unavailable. The strengthened lifecycle test verifies new-score switching.

Envelope and gradient sampling each have one search result. Built-in graphs and
new migrations use the shared sampler names. The two old field-specific names
remain readable for saved graphs; their kernels already use the same tensor
implementation. They are excluded from the authoring search, with a native check.

## Library import checkpoint

`get_pattern_score_template` resolves the venue-visible implementation through
the admitted graph read seam and converts it to a detached canonical score clip.
It does not write the library. `Score::import_clip` and `make_independent` share
one recursive copy operation: only reachable local definitions are copied,
built-ins stay shared, and collisions/invalid copies are atomic. The template's
selection and control defaults survive; each newly inserted clip gets its own
seed and actual musical timing from the destination timeline.

The canonical insertion picker now includes saved library patterns. The global
browser opens that preview at the current timeline range. Enter or a result click
performs the insertion through the score's existing history/CAS path. The new-tab
Pattern choice opens this browser without requiring a previously selected pattern.
Async results are addressed to the captured score and check that it is still open
and writable before applying.

The native test exposed a same-name selection bug: a programmatic search update
reset the selected library row to the score graph above it. Search now resets the
selection only when the query actually changes. The regression imports a library
pattern that is already used in the score, undoes/redoes it, opens the imported
clip in the canonical editor, and exercises edge deletion/undo. Persistence checks
verify distinct graph roots and unchanged source library JSON. Core coverage also
checks dependency isolation, retained selection/overrides/seeds and atomic failures.

Current logs: `/tmp/luma-library-import-all-core.log` (161 core tests),
`/tmp/luma-library-import-core-clippy.log` (strict core Clippy),
`/tmp/luma-library-import-migration-native.log` (four native migration tests),
`/tmp/luma-library-import-picker-regression.log` (six picker/editor tests),
`/tmp/luma-library-import-manifest.log` (regenerated IPC contract), and
`/tmp/luma-library-import-backend-check.log` (backend all-targets check).
The final strengthened source-JSON assertion passes in
`/tmp/luma-library-import-native-final.log`; eight tab-chrome unit tests pass in
`/tmp/luma-library-import-tabs.log`. The GPUI workspace/all-targets check is in
`/tmp/luma-library-import-final-check.log`. Physical window review of the new
library workflow passes in the September 10 consolidation checkpoint below. No live library database was written.

## Reproduction and artifacts

- Core: `/tmp/luma-gradient-core-final.log` (160 tests); strict Clippy:
  `/tmp/luma-gradient-core-clippy.log`.
- Python: `/tmp/luma-canonical-python-authoring-final.log` (real worker),
  `/tmp/luma-python-score-tests.log` (12 tests),
  `/tmp/luma-python-track-tests-final.log` (27 tests).
- Native: `/tmp/luma-gradient-focus-native-final.log` (18 serial tests).
- Persistence: `/tmp/luma-final-score-persistence.log` (16 tests).
- Renderer: `/tmp/luma-graph-final-render-test.log`; images in
  `/tmp/luma-graph-redesign-final-renders` show isolated blue output and the
  composite purple output, and the test compares private and published renders.
- Native window: `/tmp/luma-native-review-p6e1ix87/screenshots`.
  `14-reopened-query.png` shows the unfocused search before the fix;
  `17-fixed-immediate-typing.png` shows immediate physical typing after the fix;
  `19-input-card.png` shows the compact Input;
  `21-scrub-retry.png` shows a 1.68-second preview seek;
  `23-loop-enabled.png` captures active fixture output. Filenames identify
  capture attempts, not assertions about transient control states.
- Backend/all-targets and GPUI workspace/all-targets checks pass in
  `/tmp/luma-gradient-backend-check.log` and
  `/tmp/luma-gradient-focus-workspace-check.log`.

The private app and Xvfb processes were stopped. No live library database was
written. The copied fixture's normal desktop startup attempted audio analysis;
its n2n model load failed with `invalid load key, 'v'`, an adjacent local model
artifact issue. The earlier full backend run's six unrelated authentication and
venue-chain failures remain documented in the design ledger; this checkpoint
does not claim the full backend suite passes.

## September 10: one editor and native product review

Removed the Legacy source, detached graph document/history, old save/reload and
static image-preview calls, old parameter-card widgets, and the separate tab
kind. Replaced their obsolete tests with canonical score gestures. A physical
clip double-click exposed an inspector-layout shift between the two clicks;
opening now uses the clip selected by the first click.

The real application ran on its own Xvfb display against a SQLite backup of a
disposable library-import fixture. No live database was used. Captures under
`/tmp/luma-single-editor-review-pjv1japv/screenshots` show:

- `05-expanded.png`: the visible Expand action gives the editor room.
- `09-mapping-presets.png`, `10-custom-vector.png`: existing presets and a
  custom UVZ direction, with component labels.
- `11-mirror-presets.png`, `12-custom-plane.png`: mirror presets, an independent
  custom normal, and plane offset in meters from the selection center.
- `13-browser.png`, `14-import-preview.png`, `16-import-complete.png`: library
  selection, the selected Library result in the normal insertion preview, and
  three clips after confirming the copy. Saved library JSON is unchanged.

Validation: `/tmp/luma-single-vocabulary-core.log` (161 core tests),
`/tmp/luma-single-vocabulary-migration.log` (31 backend migration tests),
`/tmp/luma-single-editor-final-graphs.log` (22 native graph tests),
`/tmp/luma-score-graph-tabs.log` (three tab/expand tests),
`/tmp/luma-score-graph-chat-navigation.log` (score/graph chat continuity),
`/tmp/luma-score-graph-search.log` (search, wiring, Delete and undo), and
`/tmp/luma-single-editor-pixel-check.log` (optional pixel fixtures compile).
The pixel harness cannot capture on Linux; the real native window supplies the
physical visual evidence instead.

The broader chat suite has seven scripted-turn failures caused by a rejected
cloud execution token (HTTP 401), plus two obsolete model/welcome UI expectations.
The graph navigation check now opens the venue explicitly and waits for loaded
clips; it passes without sending messages. These chat failures are not repaired
by weakening authentication. The copied fixture also reproduces the previously
reported n2n model artifact load error. Neither is claimed fixed.

## Final gates and known limits

- `/tmp/luma-final-editor-graphs.log`: 23 native graph tests passed.
- `/tmp/luma-final-editor-score-switch.log`: strengthened close/reopen/switch
  regression passed after the final lifecycle change.
- `/tmp/luma-portable-events-core.log`: 162 core tests passed; strict core Clippy
  passed in `/tmp/luma-portable-events-core-clippy.log`.
- `/tmp/luma-portable-events-persistence.log`: all 16 persistence/migration/history
  tests passed after the final authored-event validation change.
- `/tmp/luma-final-editor-render-authoring.log`: real Python authoring and scene
  rendering passed, including exact private-preview/published-render equality.
- `/tmp/luma-final-editor-backend-check.log` and
  `/tmp/luma-final-editor-workspace.log`: backend and GPUI all-targets checks pass.
- `git diff --check` passes. No live database writes, commits, or staging occurred.

The final portability audit found that targeted event literals could serialize
resolved head IDs. `Score` already rejected resolved numerical/cell snapshots; it
now applies that rule to targeted events too. Recorded global event times remain
valid saved data. Graph selectors resolve group-selected head identities at runtime.
Tests cover both clip overrides and graph defaults at this serialization boundary.

Durable visual evidence is in `/home/julian/.codex/visualizations/2026/09/09/01a0881c-2671-75d0-bc8f-27d86b2d84e7/native-final`.
`19-viewport-controls.png` shows navigation in the fitted overview;
`20-readable-chase.png` shows the selected card after pressing 100%.
The import review's copied database has three clips with three independent roots,
and its entire patterns table exactly matches the untouched source fixture.
The private native app and Xvfb display were stopped after review.

Laser math is covered by the sampled-path composition. A laser implementation
still needs a separate ordered scan-point domain (independent of output time and
fixture identity), plus a device adapter for position, color/intensity and blanking
at the scanner's sample rate. Those domain/output additions and laser hardware are
outside this completed graph-editor goal.

Adjacent limitations: the host treats a union selection as one grouping domain,
so per-group mapping cannot split that union into its constituent groups; the
existing flag remains preserved. Graph deletion undo restores nodes/wires but does
not restore the deleted node selection. The broader chat/auth and venue-chain test
failures, and the local n2n model artifact problem, remain as described above and
in the chronological design ledger. The full application test suite is not claimed
green. None blocks the verified graph workflows or required effect compositions.


## Integration with dev

The completed redesign was committed as `6978a5a6` and integrated with dev's
`9c3cd008` visualizer work and `e23532dc` track playback/overlap changes.
Graph preview now uses the same session ownership checks as track playback;
late range, seek and pause commands cannot modify another song. Returning to
the timeline restores its own playhead. Fullscreen Space controls the active
preview, while Space in the graph canvas still opens node search.

The newer inspector layout exposed a double-click hit-test regression. The
first clip hit now survives the layout change until the second click, and
opening, dragging, undoing and reopening the graph pass native verification.

Integration validation: 23 native graph tests in `/tmp/luma-dev-merge-graphs.log`,
eight navigation/overlap/fullscreen tests in `/tmp/luma-dev-merge-navigation.log`,
four audio boundary/session tests in `/tmp/luma-dev-merge-audio-tests.log`, and
13 Python score tests in `/tmp/luma-dev-merge-python.log` pass. The IPC manifest
was regenerated for the combined command table and its check passes in
`/tmp/luma-dev-merge-manifest-final.log`. Backend all-targets validation passes
in `/tmp/luma-dev-merge-backend-check.log`.
GPUI workspace/all-targets validation also passes in
`/tmp/luma-dev-merge-workspace-check.log`. Both local SQLite databases were
backed up and passed `quick_check` before updating the watched dev checkout.

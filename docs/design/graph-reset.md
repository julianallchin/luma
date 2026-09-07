# Graph reset — execution contract and migration ledger

## Accepted model

A node has typed inputs and outputs. Its implementation is a native fundamental
operation or a graph of nodes. Built-in definitions are fixed. A score owns its
custom graph definitions; clips reference them and carry independent timing,
selection, z-order, blend mode, seed, and input overrides. Multiple clips may
share one definition. Making a clip independent clones the reachable local
definition graph. Local graph labels are optional. There is no separate Pattern
or Implementation identity in the destination model.

Human and Python edits must mutate this same document with the same validator.
Exposure is an input binding, not a synthetic node and matching bundle of wires.
Nested graph navigation follows a definition reference. Built-ins are inspected
read-only; customization copies them into the score.

The existing compositor owns clip overlap: color blend modes, dimmer/strobe
scalar blending, position last-write-wins and binary movement speed. Output
bindings preserve which capabilities a graph writes. RGB-to-color/dimmer
conversion retains current apply_color semantics. Masking within a graph is
explicit signal arithmetic; it must not silently become opacity over other clips.

Core operations retain units and domains. Scalars broadcast; fields align by
stable head identity. A field-domain mismatch is an error. Musical positions,
beat durations, normalized coverage, and normalized spatial positions are distinct.
Envelope sampling consumes an explicit phase/coordinate. Randomness is a pure
function of base seed, head identity and event/refresh index; seeking is repeatable.
No new mutable playback RNG or unspecified previous-frame state.

## Migration scope

EBF: 1,225 clips on 14 populated scores, using 22 Patterns. Local library: 43
Patterns total. Preserve the other venues and all original authored data until
their migration scope is decided. No leaderboard or laser-specific work.

Backup: `/home/julian/luma-backups/graph-reset-20260906`, verified SQLite integrity
plus SHA-256 for all copied assets; 2,888 files, 23,656,328,574 bytes. The manifest
is outside the repo. Never run a new binary against this immutable backup.
Working baseline: `/home/julian/luma-migration/graph-reset-20260906/baseline-library`.

## Work ledger

- [x] Full independent library backup and verification.
- [x] Audit saved graphs, including upstream paths reaching output sinks.
- [x] Capture EBF reference numeric outputs (22 effects, 65 frames each).
- [x] Capture representative stage renders (Chase, circle, gradient, random mask).
- [x] Unify core node contracts and graph-backed Chase/Dissolve.
- [x] Complete existing output capabilities through the normal compositor.
- [x] Remove the Pattern wrapper in the score document and support shared graph editing.
- [x] Replace Pattern/Implementation persistence for migrated scores; retain legacy consumers for other venues.
- [x] Native graph editing/navigation/exposure and matching Python operations.
- [x] Rebuild the required built-ins; translate EBF graphs and clip overrides.
- [x] Verify migrated output, then adopt the working library in GPUI.
- [ ] Finish remote sync after reconnecting the Supabase connector.

## Existing behavior requiring an explicit migration decision

- Legacy time_delay is a pass-through placeholder.
- Legacy Normalize and Invert use sampled, frozen clip-wide extrema; they are
  not equivalent to fixed-range remapping or `1 - x`.
- Legacy within-graph OpKind::Blend is a placeholder. The new output path shares
  the existing cross-clip blend math and preserves unwritten capabilities.
- Structured Mapping values validate but the native widget only edits simple choices.
- Unmigrated scores retain the legacy Python Pattern API. New scores expose
  graph definitions, clips and typed inputs directly; no Pattern/Implementation
  records are written by their native or Python editing paths.

## Verified implementation checkpoints

- Chase's pill, both mask combiners, and dissolve are graph recipes. Pill output
  matches the prior spatial kernel across clipping, wrapping, widths and shapes.
- Numeric EBF captures after the core change exactly match all original captures,
  including graph, overrides, sample times, per-head frames and capability masks.
  Comparison directory: `ebf-after-core` beside the baseline.
- The core score document now has direct graph references and independent copies;
  production Pattern/Implementation storage has **not** been replaced yet.
- Native graph navigation enters Chase → Chase Mask → Pill and returns through
  breadcrumbs. The headless test confirms the Inputs box is absent and built-in
  definitions cannot be deleted while inspecting them.
- Preview and score rendering share U right / V downstage / Z up. Fixed expressions
  are folded during preparation; this is constant folding, not op fusion.

- Version-2 score sources now use the same authored revision identity, CAS,
  idempotent edit path, restore, detached workspace merge and server-head
  projection as legacy scores. Eight focused tests cover atomic rollback,
  stale writes, shared graph edits, migration/restore, workspace merges and
  first sync materialization. Existing authored-history tests also pass.
- `scores.graph_document_json` is a local-only projection. It is excluded from
  metadata row sync; updates leave the metadata clock unchanged. New SQLite
  migrations guard against writing legacy clips into a migrated score. No
  Supabase schema change is needed for this payload: it travels in the existing
  `score.luma` revision file.

- Version-2 scores now render directly through the existing compositor. Timeline
  placement, clip previews, save/reopen and shared graph edits use that same
  document. A tempo-aware beat/seconds inverse preserves authored positions;
  active clips have half-open intervals. The 22 EBF numeric reference captures
  still match exactly (`ebf-after-native`, `ebf-native-comparison.json`).
- Native score graphs support adding and removing nodes, typed port wiring,
  output selection, input exposure/defaults, per-clip controls, node layout and
  undo/redo. Double-click follows graph definitions; fixed definitions are
  read-only. Incomplete wiring stays in an in-memory draft while the last valid
  show keeps playing. Completing a draft merges against other score edits.
- The four native graph tests cover insertion, the per-clip Envelope editor,
  nested inspection, composition, exposure, layout and undo/redo. Linux pixel
  mode verifies actual text geometry but this GPUI pin cannot capture screenshots
  through its headless renderer.

## Remaining integration risks

- New-score creation, perform playback and Python now consume graph documents.
  Other legacy-only surfaces still need review before removing old storage.
  Perform still combines all scores for a matched track/venue; selecting the
  intended score on a deck remains an unresolved product behavior.
- There is no incremental plan cache for version-2 scores yet. Timing relations
  are checked without geometry at save time; actual mapping-domain requirements
  are checked during host preparation. Dynamic expressions can still fail at
  another time, and not all such failures can be proven away statically.
- Incomplete native graph drafts live in memory; closing their tab can lose
  them. Published graph tabs now refresh after shared-definition edits, while
  drafts retain their merge base.
- Render-slot generations are now per engine and installed atomically. A late
  update cannot reopen a closed score/deck; deck compilation preserves the
  editor scene and empty results clear prior deck output. The legacy plan cache
  remains process-global until its consumers are retired.
- The full backend run had two rig-chain Python failures (joint direction
  mismatch) outside this graph work; their status on the base branch is not yet
  verified. Graph/storage/count regressions found in that run were fixed and
  their focused checks passed.

## Python and validation checkpoint

- `luma.track.edit()` owns both graphs and clips. `edit.graph(node="chase")`
  creates the same one-node wrapper as the native picker. `graph.node`, port
  binding, exposure, defaults, output selection and removal use core GraphEdit.
  `edit.make_independent` uses the same reachable-definition clone as GPUI.
- `edit.source()` is the canonical score.luma JSON. `replace_source` accepts it
  as a draft; check/apply use the shared validator and production compositor.
  Definitions are inspectable by reference through `definition(id)`; shipped
  nodes are fixed, local graphs can call other local graphs.
- A real Python-kernel test composes Chase and movement, exposes inputs, renders,
  saves/reopens, detaches a shared graph, retries a save, edits a detached
  workspace and merges it. It verifies no Pattern or old clip rows are created.
  A u64 seed at its maximum value survives the manifest and a second cell;
  the binding number codec previously converted large unsigned integers to f64.
- Core validation bounds local definitions (512), clips (2,048), nodes per
  graph (128), graph nesting (24), expanded nodes (8,192), execution dependency
  depth (96), interface inputs/outputs (64/32) and envelope knots (256). Identity
  keys contain 1–256 printable bytes; `@` prefixes are reserved for UI controls.
  Three tests reject compact exponential expansion, deep dependencies and
  invalid timing before scene preparation.

- Latest backend checkpoint: 921 tests passed; two rig-chain Python tests failed
  with joint-direction errors outside the changed graph code. The third failure
  regenerated the IPC manifest and passed on rerun. Workspace all-target checks,
  four native graph tests, 41 core tests and 96 Python unit tests pass.

## Signal and audio checkpoint

- Added one Gradient value (ordered RGB stops, including coincident stops
  for hard color changes), color fields, field masking, color output, scalar
  arithmetic/unit conversions, field floor/fraction/absolute/sine and coherent
  spatial noise. Resolved color fields cannot be stored as authored values.
- Wash, Pulse, dimmer-only Pulse, Color Fade, Spatial Gradient, Noise Wash,
  Frequency Pulse and Drum Pulse are ordinary graphs. Appearance and uniform
  dimmer output are now graphs too; their old dedicated kernels were removed.
- Clip Time reads the placed clip's duration. Gradient timing therefore follows
  clip resizing and variable tempo. Envelope controls temporal and spatial
  response. Native graph and clip controls edit Gradient values; graph stop
  dragging is verified through save. Color-only score graphs use RGB controls.
- A prepared graph reports its required audio stems, drum events and harmony.
  The authorized score preparation loads a shared immutable source. Band energy
  uses the existing causal FFT math; onset clocks use absolute analyzed events.
  Missing stems/analysis fail explicitly, with no full-mix fallback. Save-time
  timing validation still works when audio has not been bound yet.
- Verified: 46 core tests, 96 Python unit tests, four backend lighting tests,
  the existing four native graph tests and the new persisted-gradient test.
  The real Python-kernel check now also renders Drum Pulse and Frequency Pulse
  and rejects an unavailable bass stem. Workspace all-target check passes.
- No live in-app model runs are authorized: the user explicitly ruled those
  out because of cost. Test harness/Python execution does not call a model.
- EBF migration remains pending. The new noise kernel intentionally has a
  stable, documented seed contract rather than reproducing an undocumented
  legacy random stream. Audio range calibration and curved-envelope translation
  must be recorded in the migration report.

## Spatial recipes and complete-reference checkpoint

- Captured all 1,225 EBF clips, 65 frames each, with per-head geometry, the exact
  beat grid, output capabilities and frozen legacy range calibration. Files are
  in `ebf-all-clips` beside the baseline library (182,359,909 bytes). All clips
  resolve to 8 or 16 heads. The 72 strobe-only clips deliberately write no light;
  the other 1,153 captures contain nonzero dimmer output.
- Motion is now a graph of travel time, Envelope and scalar interpolation.
  Chase exposes its travel curve independently of its stroke shape. A triangle
  travel curve gives a bounce; outside start/end positions remain supported.
  Rhythm has a beat-valued phase delay, inherited by Chase, Pulse and Dissolve.
- Rank, selection reductions and raw stage coordinates are fundamental sources
  and operations. Radius, selection normalization and field profiles compose
  from them. Raw radial distance preserves the physical stage aspect ratio.
- Random Heads is a graph that either draws fresh sets or walks a seeded shuffled
  order. It floors and bounds requested counts and minimizes immediate repeats
  when walking. Stable head IDs break ordering ties; seeking is independent of
  previous frames and selection traversal order.
- Rainbow, Harmony Color and Strobe are graphs. Strobe-only output remains
  available separately, and uniform strobe now wraps the per-head writer.
- Fixed an adjacent integration gap: standalone composable previews now load
  the same authorized audio features as saved-score playback. The real Python
  kernel test also exercises direct Drum/Frequency preview and missing-stem
  diagnostics, without running a model.
- Verified: 52 core tests, five native graph tests, the real Python kernel and
  direct audio-preview test. No user-library migration has been applied yet.

### Additional migration findings

- Legacy color arguments lower to three-channel RGB, so their stored alpha is
  ignored by the current evaluator. Preserve actual output; do not reinterpret
  alpha-zero arguments as dimmer-only contributions during translation.
- The misspelled `soild_strobe` only writes strobe. It must migrate to strobe-only
  output, not a wash plus strobe.
- Gradient now shares the existing OKLab interpolation. Native bar painting and
  stop insertion use the same color space, so two-stop authored fades keep two
  stops. A regression compares canonical gradients against the legacy sampler.
- Legacy beat envelopes use the smallest analyzed pulse gap for their full duration, whereas
  new timing follows the current grid interval. Quantify these differences.
- Removed obsolete Tauri/TypeScript introductory comments from `agent_harness`;
  its dispatch seam is shared with GPUI. No web interface is involved.

- Synced compressed stems now rebuild their disposable PCM cache during preview
  and score preparation. An unavailable source remains an explicit error; it is
  never replaced with the full mix. The Python/preview regression passes with
  a saved WAV stem and no decoded cache, without running a model.
- During EBF capture, 26 of 40 bass-band clips silently used the mix because
  their bass PCM cache was absent; 14 used bass. The migration records this
  effective choice explicitly. The legacy loader's “evaluating as silence”
  warning is misleading: the old audio kernel falls back to the mix. That old
  path remains for unmigrated scores.
- Verification for shared OKLab: 53 core tests, six Gradient editor tests,
  the backend sampler comparison, native persisted-gradient test, core clippy
  and workspace all-target check pass.

## EBF adoption — September 6

The user explicitly approved fixing old flaws even when that changes output.
The reference captures are evidence for intent, not a requirement to reproduce
average-BPM drift, sampled Invert extrema, accidental stem fallback or pixel
merging. No paid in-app model turns were run.

- `scripts/library/ebf_graph_reset.py` contains the manually rebuilt recipes. It
  uses a whitelist of local dispatch calls. Import verifies reviewed source
  hashes and unchanged original sources, validates every score first, then
  imports through authored history and compare-and-swap. It never writes score
  JSON directly into SQLite. Live mode uses the regular stored identity, never
  fixture admission. The regular venue-open upgrade prepares old stage geometry
  before score validation acquires its read transaction.
- Complete review: 1,225 clips × 65 times, matching head IDs and output capability
  masks. Every light-producing clip has nonzero output; the 72 strobe-only clips
  remain strobe-only. All 22 effect families also generated saved-score preview
  strips, and four families were inspected with the production stage renderer.
- Your 38 EBF scores are now version 2: 14 populated and 24 empty, with all
  1,225 clips preserved. Ten empty EBF scores owned by other accounts were left
  unchanged, as were all 1,952 legacy clips in other venues. Database integrity
  and foreign keys pass after import. The regular GPUI app was launched against
  `~/.config/com.luma.luma`, not a test library.
- Full backup remains `~/luma-backups/graph-reset-20260906`. A fresh online
  database/history backup immediately before live import is in
  `~/luma-backups/graph-reset-pre-import-20260906` (both databases verified).
- Comparison sources, per-clip previews and changes are under
  `~/luma-migration/graph-reset-20260906/migration-review`. The working-copy
  import ledger is `import-working-copy-05/applied.json`; the regular library
  import ledger is `import-regular-library-02/applied.json` alongside it.

Intentional corrections:

- Rhythm follows the current beat grid, including clip-relative color/circle
  motion. Beat-envelope duration no longer uses the shortest gap in the song.
- Linear chases share one local graph with independent mapping, travel curve,
  repeat, travel time, stroke width, stroke Envelope and color inputs. Stroke
  profiles have lit centers and dark edges rather than frozen Invert extrema.
- Major-axis Chase uses the actual fitted axis. Circle alternation addresses
  heads individually rather than merging nearby pixels by a hidden distance.
- Random Heads and noise use authored, stable per-head seeds. Kick retriggers
  start a new envelope instead of inheriting the prior kick's tail.
- Bass-band clips read the saved bass source. Sensitivity is explicitly authored
  from its 95th-percentile sampled energy, with a floor to avoid amplifying
  silence. This replaces the full-mix fallback in 26 clips and its mismatched
  calibration.

Native/Python customization now uses `Score::customize_node`. “Edit a copy”
clones the called graph into the score and rebinds that call site; Python uses
`node.customize()`. Fundamental ops remain immutable. Shared graph tabs refresh
after publication, and undoing customization leaves a removed subgraph view.
Verification: 53 core tests, five native lighting tests (including customization
undo/redo), real Python editing/rendering test, core clippy, workspace check and
regular app build pass.

Startup exposed a remote sync blocker: `patterns.score_id` is absent in the
server schema, but the retained legacy sync registry requests it. The matching
remote migration is checked in but has not been applied by this run. Supabase
MCP is listed but fails OAuth refresh (“Failed to parse server response”), and
there is no configured CLI deployment credential. Reconnection was requested.
No remote DDL has been attempted. Inspect the actual remote schema before
applying the pending migration; do not assume its legacy `track_scores` table
still exists.

The regular app also exposed a local uploader bug: composite keys were decoded
as strings even when one component was an INTEGER (`parent_order`). Dirty scans
now project identity components as text; transmitted row values keep their SQL
types. A regression reproduces the original failure and verifies the numeric
payload, receipt, and empty second flush. All 65 sync tests pass.

After rebuilding and restarting the regular app, it uploaded 186 records and
cleared all 45 waiting revision-parent receipts. All 38 EBF migration proposals
have server sequence numbers and terminal integration records; no live authored
proposals remain pending. The legacy `patterns.score_id` pull error still needs
the remote schema fix. No remote DDL was attempted. App log:
`/tmp/luma-graph-reset-regular-app-sync-fixed.log`.

## Envelope curves — correction to the migration

The first migration baked curved responses into dense linear point sets. That
preserved samples but produced a poor editing model. Envelopes now store real
cubic Bézier segments between authored anchors. There is one optional `curves`
entry per segment; omitting the array retains straight segments in existing
revision history. Each entry is `{"kind":"linear"}` or, for example:

```json
{
  "points": [[0, 1], [1, 0]],
  "curves": [{"kind": "bezier", "control1": [0.3, 1], "control2": [0.7, 0]}]
}
```

Handles use the same normalized coordinates as anchors. Their x positions must
stay ordered between their segment endpoints; y stays in 0..1. Evaluation
solves the curve's x coordinate before sampling y. GPUI paints that same cubic
directly and edits its handles, with no authored polyline approximation.
Double-click inserts an anchor using de Casteljau subdivision without changing
the shape; moving/removing anchors maintains the shared value's invariants.
Straight/Curve controls and presets use the same value. Graph controls, clip
overrides, Python source, saved history and playback retain the curve metadata.

The migration tool now constructs one segment per phase and three anchors per
stroke. Its `--repair-curves` path compares against the previous reviewed import
and changes only still-unmodified sampled envelopes through authored CAS. The
regular library repair replaced 462 envelopes on 14 scores: 39,930 sample knots
became 1,207 anchors. The maximum measured normalized difference is 0.006943;
all other score content is unchanged. No user-edited envelopes were encountered.
No database schema migration is required.

Backup: `~/luma-backups/bezier-pre-repair-20260906`. Live repair sources, before
exports, and revision ledger: `~/luma-migration/graph-reset-20260906/bezier-regular-01`.
Verification includes analytical Bézier inversion, shape-preserving subdivision,
validation and atomic edit regressions, all core tests, core clippy, workspace
check, native handle dragging through persistence, the real Python editing/
rendering round trip, and saved nonempty previews for all 11 affected effect
families. The regular GPUI app was rebuilt and restarted with this repair.

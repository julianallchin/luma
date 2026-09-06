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
- [ ] Replace production Pattern/Implementation persistence and consumers.
- [x] Native graph editing/navigation/exposure and matching Python operations.
- [ ] Rebuild remaining built-ins; translate EBF graphs and clip overrides.
- [ ] Verify migrated output, then adopt the working library in GPUI.

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
  them. Other open graph tabs do not yet refresh after shared-definition edits.
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

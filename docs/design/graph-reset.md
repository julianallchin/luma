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
- [ ] Capture representative stage renders.
- [ ] Unify core node contracts and graph-backed Chase/Dissolve.
- [ ] Complete existing output capabilities through the normal compositor.
- [ ] Remove the Pattern wrapper in the score document and support shared graph editing.
- [ ] Replace production Pattern/Implementation persistence and consumers.
- [ ] Native graph editing/navigation/exposure and matching Python operations.
- [ ] Rebuild remaining built-ins; translate EBF graphs and clip overrides.
- [ ] Verify migrated output, then adopt the working library in GPUI.

## Existing behavior requiring an explicit migration decision

- Legacy time_delay is a pass-through placeholder.
- Legacy Normalize and Invert use sampled, frozen clip-wide extrema; they are
  not equivalent to fixed-range remapping or `1 - x`.
- Legacy within-graph OpKind::Blend is a placeholder. The new output path shares
  the existing cross-clip blend math and preserves unwritten capabilities.
- Structured Mapping values validate but the native widget only edits simple choices.
- The newly added Python Pattern API uses the old storage projection and will
  be replaced together with native authoring; it is not a second permanent model.

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

- New-score creation, perform playback and the Python authoring API still need
  the version-2 document path. Existing migrated scores must not fall back to
  legacy clip rows in these consumers.
- Graph size/expansion limits and validation of related runtime inputs are not
  complete. There is no incremental plan cache for version-2 scores yet.
- Incomplete native graph drafts live in memory; closing their tab can lose
  them. Other open graph tabs do not yet refresh after shared-definition edits.
- The legacy compositor has a process-global generation/cache and a stale
  install window between score resolution and its legacy install call.
- The full backend run had two rig-chain Python failures (joint direction
  mismatch) outside this graph work; their status on the base branch is not yet
  verified. Graph/storage/count regressions found in that run were fixed and
  their focused checks passed.

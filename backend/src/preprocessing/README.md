# Preprocessing DAG

Audio analysis runs as a layered DAG of typed nodes. Adding a new preprocessor
is one trait impl, one entry in `registry.rs`, and one migration. This doc
walks through the contract and the moving parts.

## The `Preprocessor` trait

See [`preprocessor.rs`](preprocessor.rs). Each impl declares four things and
optionally overrides one:

| Method            | What it does                                                          |
| ----------------- | --------------------------------------------------------------------- |
| `name`            | Stable wire name (logs, `preprocessing_failures`). Never rename.      |
| `version`         | Bumped when output schema or algorithm changes. Triggers backfill.    |
| `inputs`/`output` | Artifact dependency edges. Drives topo-sort.                          |
| `artifact_table`  | Local table the row(s) live in (must have `track_id`, `processor_version`). |
| `run`             | Per-track work — compute, persist, return `Result<(), String>`.       |

Default implementations of `is_complete` and `list_pending` are derived from
`artifact_table` + `rows_per_track` — workers don't write SQL for the happy
path. Both read one definition of a current artifact, so the run path and
startup reconcile agree on what is left to do. Override `verify_disk` if your
output includes side-effect files (see `workers/stems.rs`); only the run path
checks it, so reconcile doesn't notice deleted files. Declare
`first_bar` if your rows are indexed by bar (see "Shared bar axis").

## The `Artifact` enum

See [`artifact.rs`](artifact.rs). Every input/output is one of these typed
variants. Two preprocessors producing the same artifact panic at startup, so
naming collisions are caught immediately. The `as_str()` value is persisted —
never rename a variant.

`Artifact::Audio` is special: always available, never produced. List it as an
input on your root preprocessor.

## Registry

See [`registry.rs`](registry.rs). Adding a node is literally one line:

```rust
Arc::new(workers::n2n::N2NPreprocessor),
```

`registry::dag()` layers this list once, on first use (startup
reconciliation), and checks it there: two producers of one artifact, an input
nobody produces, or a cycle panics. The scheduler only runs the layers.

## Version-bump backfill

Every artifact row carries a `processor_version` column. Reconcile-on-startup
asks each preprocessor for tracks whose row is missing OR whose version is
below `self.version()`. Bumping `version` from 1 to 2 thus re-runs the
preprocessor across every existing track on next launch — no manual
migration step. Bump when:

- The output schema changes (column added, JSON shape edits).
- The algorithm changes meaningfully (new model weights, different
  thresholds — anything that would make old rows wrong).
- Bundled bytes (model weights via `include_bytes!`) change. Hash them into
  the version decision.

Don't bump for cosmetic refactors — version churn is expensive (full
re-analysis across the whole library).

## State (no separate state table)

Completion lives **on the artifact rows themselves** via `processor_version`.
Sync-pulling an artifact from another device counts as completion
automatically — no special hook. See [`mod.rs`](mod.rs) for the rationale.

Failures live in [`failures.rs`](failures.rs) — the local-only
`preprocessing_failures` table holds (track_id, preprocessor) PK with
exponential backoff (cap 24h). Records are written on `Err`, cleared on `Ok`.
While a failure's `next_retry_at` is in the future, the step is skipped on
that track, with its dependents: reconcile does not queue the track for it, and
a track queued for another step passes it by, unless one of the step's inputs
was rebuilt in the same run. Reconcile first forgets the failures recorded
before the Python environment was last installed, and those other versions of
each preprocessor recorded, so a repaired environment or a bumped `version`
retries every track the old one failed on, with a fresh backoff.

## Scheduler

See [`scheduler.rs`](scheduler.rs). Two-tier parallelism:

- **Intra-layer fan-out**: within one track, siblings in the same topo layer
  spawn into a `JoinSet` and run concurrently. Beats and stems both depend
  only on `Audio`, so they run in parallel for the same track.
- **Cross-track**: `run_bounded_track_jobs` (size = `analysis_worker_count()`,
  both in [`concurrency.rs`](concurrency.rs)) bounds how many tracks process
  at once. Big libraries don't OOM the GPU.

`InflightSet` deduplicates concurrent calls for the same `(track,
preprocessor)` so user-driven re-imports never race the startup reconcile.

The startup reconcile launches its workers below the UI
(`WorkerEnvironment::in_background`: `taskpolicy -b`, the Darwin background
band, on macOS). There a library-wide backfill mostly leaves the stage's frame
rate alone, and takes longer. Other platforms still run it at normal priority.
Imports run at normal priority everywhere: someone is waiting on them.

The desktop sets Python up in the background (`AppServices::start_python_setup`),
which takes minutes on a first launch or after an update changes the
requirements. Until the packages are in, every stage waits in `run_one`
(`WorkerEnvironment::ready`) and stays queued, and so does the startup
reconcile. An identity transition ends the wait. If setup fails, stages still
run on an environment an earlier setup finished installing, so an offline
launch after an update keeps working. With none, they fail without recording
backoff: the environment failed, not the track, so the next launch with a
working setup retries them.

On failure, the failed artifact's downstream preprocessors are skipped for
this run (the track's roots won't try to compute if stems blew up). The
backoff record carries the track to a later retry.

A batch (`run_for_tracks`: one import, or the startup reconcile) also has a
circuit breaker. Once a step has failed with the same error on three tracks in
a row, with no success between, the rest of the batch skips it and its
dependents, and each skipped track reports that error. A step failing that way
is broken itself, in its code or its Python environment, so this saves a
worker launch per track. The skipped tracks record no failure, so the next
batch tries the step again.

What runs is also reported, for the status bar, in
[`activity.rs`](activity.rs): each batch holds its tracks there until it is
done with them, each step marks itself running while its worker runs, and the
breaker's paused steps last as long as their batch. Guards let go on every
path out, cancellation included. The `background_work` command reads that,
the steps in backoff and Python setup in one snapshot.

Host events emitted: `Event::TrackImportState` throughout an import, one per
stage start and settle included. Consumers branch on the structured
phase/step/error fields, never human-readable status labels.

## Worked example: adding the n2n drum-onset node

Concrete walkthrough using the drum-onset preprocessor (model:
[`julianallchin/n2n`](https://github.com/julianallchin/n2n), a paper-aligned
reproduction of Yeung et al., Sony AI 2025). Five steps:

1. **Reserve the artifact.** Already done in `artifact.rs`:
   ```rust
   Artifact::DrumOnsets,  // wire name "drum_onsets"
   ```

2. **Add the migration.** Add a new file in `migrations/`. A new artifact table
   follows the row model. Use `track_drum_onsets` as the model:
   `track_id TEXT PRIMARY KEY`, `uid`, the JSON column (`onsets_json`),
   `processor_version`, `created_at`, `updated_at`, the `updated_at` trigger,
   and a generated `id` column (`GENERATED ALWAYS AS (track_id) VIRTUAL`) with a
   unique index. Do not add `synced_at`, `origin`, `version` or sync triggers.
   The upload triggers are TEMP triggers that
   `backend/src/sync/triggers.rs` generates from `backend/src/sync/schema.rs`.
   If the table syncs, add it to `schema.rs`. Also add a Supabase migration
   with the table, its row-level security policies and the `powersync`
   publication. A local-only table needs neither.

3. **Add the python worker.** `python/n2n_worker.py` takes the drum stem
   (`drums.ogg`) path plus `--ckpt <weights.pt>` and `--mert <cache.npy>`,
   and emits
   `{"onsets": {"kick": [t, ...], "snare": [...], "hat": [...], "cymbal": [...]}}`
   on stdout. The vendored model package lives next to it in `python/n2n/`,
   and the checkpoint (~190 MB, Git LFS) is `python/n2n/weights.pt`.

4. **Wire the trait impl.** `workers/n2n.rs`:
   ```rust
   impl Preprocessor for N2NPreprocessor {
       fn name(&self) -> &'static str { "n2n" }
       fn version(&self) -> u32 { 10 }
       fn inputs(&self) -> &'static [Artifact] { &[Artifact::Stems, Artifact::Mert] }
       fn output(&self) -> Artifact { Artifact::DrumOnsets }
       fn artifact_table(&self) -> &'static str { "track_drum_onsets" }
       async fn run(&self, ctx, track_id) -> Result<(), String> { ... }
   }
   ```
   The `run` body reads the drum MERT cache path from `track_mert`, runs the
   worker on the drum stem and that cache via `spawn_blocking`, and writes
   `track_drum_onsets`.

5. **Register.** One line in `registry.rs`:
   ```rust
   Arc::new(workers::n2n::N2NPreprocessor),
   ```

6. **Test.** `workers/n2n.rs::tests` builds an in-memory pool with the
   table, asserts `list_pending` returns the track until a current-version
   row exists, and that v1 (ADTOF-era) rows are flagged stale under the
   bumped version. The registry's own test already checks that every node
   runs after the producers of its inputs.

## Shared MERT cache

The bar classifier and the n2n drum-onset preprocessor both consume MERT-95M
layer-7 features. The `mert` preprocessor (`workers/mert.rs`,
`python/mert_worker.py`) computes two fp16 `.npy` caches per track in one
Python process: the full mix (`track_mert.file_path`) and the demucs drum stem
(`track_mert.drum_path`). The model loads once per track.

- `classifier`: slices the full-mix cache per bar, from `start_s × 75` to
  `end_s × 75`.
- `n2n`: runs sliding-window inference over the drum-stem cache. Its
  checkpoints were trained on drum stems, so both of its inputs come from
  `drums.ogg`.

The scheduler picks up the new node. Reconcile on startup queues every existing
track for it. Progress events reach the UI without UI changes.

## Shared bar axis

`classifier` and `genre` both key their rows by `bar_idx`, and both derive
bars from `workers::track_bar_boundaries` — consecutive downbeat pairs plus a
synthetic final bar. That shared definition is what lets the agent index
`features.bars` and `features.genres` with the same integer, so keep new
bar-indexed nodes on it rather than re-deriving boundaries.

Both also declare `first_bar`: where their row records bar 0 of the grid it
was computed against. A row whose bar 0 is no longer the current
`track_beats` row's first two downbeats is stale, so reconcile queues the
track and its run redoes the step. Any bar-indexed artifact needs this: a
later beat re-detection or sync pull silently invalidates every `bar_idx`
otherwise, and the drift compounds bar by bar.

## User-provisioned model weights

The `genre` node is the one preprocessor whose model Luma does not ship: the
Essentia Discogs-EffNet weights are CC BY-NC-ND 4.0, so they are never bundled
(`include_bytes!`) or committed. Instead `workers::genre::process::ensure_model` downloads
them on first use from MTG's own server into `<app config>/models/`,
checksum-pinned — Luma never redistributes the file, it only fetches from the
source. Acceptable while Luma is internal; a commercially distributed build
still needs MTG's proprietary license — see
[`docs/design/genre-model.md`](../../../docs/design/genre-model.md). Prefer bundling for
anything new; this is an exception forced by licensing, not a pattern to copy.

## Pointers

- Trait + context: [`preprocessor.rs`](preprocessor.rs)
- DAG construction + validation: [`registry.rs`](registry.rs)
- Scheduler + dedup: [`scheduler.rs`](scheduler.rs)
- Failure backoff: [`failures.rs`](failures.rs)
- Generic Kahn's: [`../topo.rs`](../topo.rs)
- Python bootstrap (venv, requirements, weight downloads): [`../runtime/python.rs`](../runtime/python.rs)

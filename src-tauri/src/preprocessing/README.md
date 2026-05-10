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
path. Override `verify_disk` if your output includes side-effect files (see
`workers/stems.rs`).

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

The scheduler topo-sorts this list at startup; cycles or unknown artifacts
panic at that point.

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
Reconcile filters out tracks whose `next_retry_at` is in the future.

## Scheduler

See [`scheduler.rs`](scheduler.rs). Two-tier parallelism:

- **Intra-layer fan-out**: within one track, siblings in the same topo layer
  spawn into a `JoinSet` and run concurrently. Beats and stems both depend
  only on `Audio`, so they run in parallel for the same track.
- **Cross-track**: a tokio `Semaphore` (size = `analysis_worker_count()`)
  bounds how many tracks process at once. Big libraries don't OOM the GPU.

`InflightSet` deduplicates concurrent calls for the same `(track,
preprocessor)` so user-driven re-imports never race the startup reconcile.

On failure, the failed artifact's downstream preprocessors are skipped for
this run (the track's roots won't try to compute if stems blew up). The
backoff record carries the track to a later retry.

Frontend events emitted: `track-import-progress` with `(track_id,
status_label)` per node, `track-status-changed` per completed node,
`track-import-complete` once all queued tracks finish. **Do not change these
event names.**

## Worked example: adding the n2n drum-onset node

Concrete walkthrough using the drum-onset preprocessor (model:
[`julianallchin/n2n`](https://github.com/julianallchin/n2n), a paper-aligned
reproduction of Yeung et al., Sony AI 2025). Five steps:

1. **Reserve the artifact.** Already done in `artifact.rs`:
   ```rust
   Artifact::DrumOnsets,  // wire name "drum_onsets"
   ```

2. **Add the migration.** `migrations/20260502000000_track_drum_onsets.sql`
   defines a `track_drum_onsets` table mirroring `track_roots`'s structure:
   `track_id TEXT PRIMARY KEY`, JSON blob (`onsets_json`), `processor_version`,
   `origin`, `synced_at`, the standard `updated_at` trigger, and the
   `sync_delete_track_drum_onsets` trigger.

3. **Add the python worker.** `python/n2n_worker.py` takes the full-mix
   audio path on argv plus `--ckpt <weights.pt>` and `--mert <cache.npy>`,
   and emits
   `{"onsets": {"kick": [t, ...], "snare": [...], "hat": [...], "cymbal": [...]}}`
   on stdout (4-class native to v6+ n2n checkpoints). The vendored model
   package + bundled weights live next to it in `python/n2n/`; a stripped
   EMA-only checkpoint (~190 MB) ships at `python/n2n/weights.pt`.

4. **Wire the trait impl.** `workers/n2n.rs`:
   ```rust
   impl Preprocessor for N2NPreprocessor {
       fn name(&self) -> &'static str { "n2n" }
       fn version(&self) -> u32 { 3 }
       fn inputs(&self) -> &'static [Artifact] { &[Artifact::Mert] }
       fn output(&self) -> Artifact { Artifact::DrumOnsets }
       fn artifact_table(&self) -> &'static str { "track_drum_onsets" }
       fn status_label(&self) -> &'static str { "Transcribing drums…" }
       async fn run(&self, ctx, track_id) -> Result<(), String> { ... }
   }
   ```
   The `run` body reads the cached MERT path from `track_mert`, shells out
   to the worker with the full-mix audio + the cache path via
   `spawn_blocking`, and `upsert_track_drum_onsets`.

5. **Register.** One line in `registry.rs`:
   ```rust
   Arc::new(workers::n2n::N2NPreprocessor),
   ```

6. **Test.** `workers/n2n.rs::tests` constructs an in-memory pool with the
   migration applied, asserts `is_complete` returns false initially / true
   after a manual insert, asserts that v1 (ADTOF-era) rows are flagged stale
   under the bumped version, and asserts the topo position lands strictly
   after `mert` (the new dependency).

## Shared MERT cache

The bar classifier and the n2n drum-onset preprocessor both consume MERT-95M
layer-7 features. They share a per-track cache (`track_mert.file_path` →
fp16 .npy on disk under `<app_config>/tracks/mert/<track_hash>.npy`) so MERT
extraction runs once per track, not twice. The cache is owned by the `mert`
preprocessor (`workers/mert.rs`, `python/mert_worker.py`); both consumers
slice their inputs out of the global stream:

- `classifier`: per-bar slice from `start_s × 75 → end_s × 75`.
- `n2n`: full-song stream piped into the sliding-window EDM sampler.

n2n's mel input also moves to the full mix so its two conditioning streams
describe the same audio. ⚠ The bundled v10 checkpoint was trained on drum
stems — running on full-mix audio is a distribution shift; verify event
quality on representative tracks when bumping the checkpoint.

That's it — the scheduler picks up the new node, reconcile-on-startup queues
every existing track for it, and progress events surface in the UI without
any frontend changes.

## Pointers

- Trait + context: [`preprocessor.rs`](preprocessor.rs)
- Scheduler + topo + dedup: [`scheduler.rs`](scheduler.rs)
- Failure backoff: [`failures.rs`](failures.rs)
- Generic Kahn's: [`../topo.rs`](../topo.rs)
- Python bootstrap (venv, requirements, weight downloads): [`../python_env.rs`](../python_env.rs)

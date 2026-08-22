# Track import dispatch seam

**Status:** dispatch/Library foundation implemented; GPUI T2–T5 UI remains the
next slice

The read and write sides are GPUI-callable through `Library`. Engine DJ and
Rekordbox share `TrackSource`, `SourceLibrary`, `SourcePlaylist`, `SourceTrack`
and one typed import request/result/progress contract. Analysis is owned by the
service task group rather than a dialog or command future.

This foundation does **not** implement the GPUI track-acquisition routes. The
Luma-wide browser (T2), source selection (T3), unified source browser UI (T4),
and background-import presentation/reconciliation (T5) remain a separate UI
slice in the renderer/dialog gauntlet.

## Implemented foundation

1. Change `services::tracks::{file_fast_import,dj_fast_import,
   engine_dj_fast_import}` from `&AppHandle` to `&StorageRoot`. Album-art and
   managed-track paths already belong to `StorageRoot`.
2. Replace `PreprocessorContext::app_handle` with explicit `StorageRoot`,
   cache/resource paths and `Events`. `scheduler` must emit only through
   `Events`.
3. Port the seven Python-backed workers together. Their `AppHandle` use is path
   resolution (`python_env`), not UI behavior. Introduce a path-based worker
   environment equivalent to `agent_execution::headless_env`; Tauri constructs
   it from its resolved cache/resource directories and GPUI reconstructs the
   same directories from environment/default roots.
4. Add dispatch commands for file, Engine DJ and Rekordbox import. Phase one
   returns inserted/deduplicated `TrackSummary` rows quickly. Phase two is
   owned by `AnalysisTaskGroup`, survives the command/dialog lifetime, and
   reports typed progress through `Events`.
5. Keep source peculiarities in handlers and expose one `Library::import_tracks`
   request/result vocabulary. The GPUI layer must never parse event prose.

## Acceptance tests

- a disposable GPUI Library imports a tiny fixture, returns before analysis
  completion, then observes typed progress and the completed enriched row;
- dropping the import future/dialog does not cancel the analysis task;
- importing the same file/source id twice returns one catalog row;
- one Engine DJ and one Rekordbox fixture normalize to the same source model;
- cancellation by identity epoch leaves no partial catalog/file ownership;
- the Tauri commands become thin adapters over the same handlers, not a second
  implementation.

These tests cover the host-neutral foundation only. They are not acceptance
evidence for the outstanding GPUI T2–T5 routes.

# Track import dispatch seam

**Status:** next owner task; prerequisite for T5

The read side is now GPUI-callable through `Library` and presents Engine DJ
and Rekordbox as `TrackSource`, `SourceLibrary`, `SourcePlaylist` and
`SourceTrack`. The write side must not be exposed by wrapping the existing
Tauri commands: doing so would either cancel analysis when the dialog closes
or silently omit it in the GPUI host.

## Required refactor

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

Until those tests pass, the add-track UI may browse sources and create durable
venue membership, but must not claim GPUI background import is complete.

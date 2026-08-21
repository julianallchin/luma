# IPC audit — 2026-08-19

The one-time audit of the Tauri command surface that motivated the dispatch seam: payload
conventions, the state/`AppHandle` inventory, the dead-command list and the known issues.

**A snapshot, not a live artifact.** Most of the surface has since moved onto
`src-tauri/src/dispatch`, so the file and line references below point at the pre-port layout and
several of the commands named here are gone. The current surface is generated in
[`ipc-manifest.md`](./ipc-manifest.md); this file is kept for the analysis, which still holds.

## Events

`close-requested` comes from Tauri's own window lifecycle, not from our emit sites. `universe-buffer` (`universe-state-store.ts:161`) and `dmx://update` (`dmx-store.ts:18`) are **dead listeners** — nothing in `src-tauri/` has ever emitted them; the visualizer gets its data from `universe-state-update` instead. `controller_port_change` (`controller_manager.rs:295`) is the mirror image: emitted, nobody listens.

Progress is *only* observable through events. `import_tracks`, `reprocess_track`, `sync_full` and the DJ-import commands all return `Ok` as soon as the work is spawned; there is no completion value and no cancellation handle in the command surface.

The one contract break here is `track-import-progress` vs `file-import-progress`: one concept, two names, and two payload shapes (a positional `(trackId, step)` tuple vs a structured `ImportProgressEvent`). See `import-progress-two-shapes` below.

## Payload conventions

### Argument shape: two competing forms

**142 commands** use flattened positional args that Tauri renames `snake_case` → `camelCase` on the wire (57 args carry a `rustName` in the JSON for exactly this reason). **22 commands** — all of `agent/authored-state`, most of `perform/midi`, and the track-score writes — take a **single `input` object** with a serde-camelCase struct. The remaining 33 take no arguments at all. The object form is the better one: it typechecks as a unit, versions cleanly, and carries `operationId` naturally. The split is historical, not designed.

The worst case of the flattened form is `save_pattern_graph_document`: `pattern-editor.tsx:1755` passes an opaque object straight into `invoke`, so the TS object keys *are* the Rust parameter names with nothing checking them. Renaming a Rust arg breaks that call at runtime with no compile error on either side.

### Identity

All ids are string UUIDs, passed by value; there are no handles or opaque cursors. `venueId` appears on **57 commands** — it is the implicit tenant key of the whole surface, and it is almost always the authorization subject rather than a lookup key (see below). `trackId` appears on 27. The only non-string id is `retry_pending_op`'s `opId: i64` (a local sync-queue rowid).

Ordering is sometimes load-bearing and undocumented at the type level: `midi_list_cues` returns rows ordered `display_y, display_x, name` and the perform canvas depends on it; `generate_annotation_previews` returns one entry per score row in `z_index` order.

### Paths

Filesystem paths cross as plain strings and are always host-absolute except one case. `import_track`/`import_tracks` take absolute paths from the OS file dialog; `engine_dj_*` take a caller-chosen `libraryPath`; `rekordbox_*` take **no** path at all because the DB is auto-discovered. `get_fixture_definition`'s `path` is the exception — it is relative to the fixtures root and is joined **without sanitization**, so `..` escapes the root. A dispatch layer should reject absolute and `..` paths there.

Large media goes the other way: paths come *out*. `list_tracks_enriched` returns `albumArtPath` and the frontend loads it via `convertFileSrc`, deliberately, so bulk responses stay small.

### Binary data — three conventions, no canonical one

| Form | Used by | Cost |
| --- | --- | --- |
| base64 `string` | `get_track_audio_base64` | whole-file; a 10 MB mp3 crosses as ~13 MB of string |
| `Vec<u8>` as a JSON number array | `generate_annotation_previews`, `preview_pattern_image`, `preview_graph_image`, `view_composite_image` (RGBA pixels) | ~4 bytes of JSON per byte of image |
| `Vec<f32>` as a JSON number array | `get_melspec`, `get_track_waveform`, `run_graph` view data | largest payloads on the surface |

`serde_bytes` is used nowhere. Any port should pick one binary encoding for all three and make it the only way.

### Casing and nullability

serde camelCase is universal on responses with exactly one exception: `get_fixture_definition` returns the raw XML mapping (`Manufacturer`, `Channel`, `"@Name"`, `"$value"`). That is the only place the XML convention leaks into TS.

`null` is a *value*, not an omission, in several places, and the distinction is only documented in prose: `remove_fixture_from_group`'s `headIndex: null` means "the whole fixture" (a number means "split the membership"); `composite_track`'s `annotations: null` means "fall back to persisted scores" while `[]` means "authoritative empty document, clear the scene".

### Writes, authorization, and idempotency

49 commands take a `VenueAccess<Read|Write>` lease. The lease *is* the authorization check and the transaction at once, and the resource is often finer than the venue (`VenueResource::Group(groupId)`, `::Score(scoreId)`). Writes must call `access.commit()` explicitly. An unauthorized id errors with a string; it does not return an empty result.

Idempotency is by `operationId` + request fingerprint, on 3 flattened commands (`save_pattern_graph_document`, `score_dsl_import`, `replace_track_scores`) and inside the `input` object of most `authored_state_*` commands — 17 commands document replay-safe behavior. The TS side depends on this: `src/lib/dsl/index.ts:51` blind-retries the identical request once on any failure, and `appendThreadMessages` reuses one in-flight `operationId` per thread. **The retry is only safe because the id is reused verbatim** — preserve that or the retries become double-writes.

### Errors

188 command signatures return `Result<_, String>`. Everything — authorization failure, not-found, optimistic-concurrency conflict, panic in a blocking task — arrives as an opaque string, and callers that need to branch parse it. `agent_thread_append_messages` is the clearest cost: the Rust service returns a typed `AgentThreadAppendOutcome::HeadMoved`, and the command flattens it into `"Agent transcript changed before append (expected X, found Y)"`.

## Dispatcher refactor notes

> **Status:** the seam described below is built — `src-tauri/src/dispatch/`, with
> `AppServices`, `Events`/`EventSink`, `HostControl`, `CommandError`, and a
> `commands!` table that generates both the Tauri adapter and a
> `dispatch(name, json)` entry point. 12 commands are ported and the agent
> harness is a thin adapter over it. The recipe for the remaining ~184 is
> [`dispatcher-port-guide.md`](./dispatcher-port-guide.md). The notes below are
> the analysis that motivated it, kept as the record of what the audit found.

What it would take to lift these bodies out of `#[tauri::command]` into a plain dispatch layer (`dispatch(name, serde_json::Value) -> Result<serde_json::Value, Error>`), based on what the audit actually found in the code.

### There is already a second dispatcher, and it duplicates command bodies

`src-tauri/src/bin/agent_harness.rs` hand-writes a `match name { … }` dispatcher covering **51 of the 196 commands**, with its own `arg()` / `opt_arg()` JSON extraction and its own re-implementation of each body (`get_pattern_args`, `save_pattern_graph_document`, `run_graph`, `replace_track_scores`, …). That is the duplication the refactor deletes, and the harness is its first consumer — not a hypothetical one. Any port should land the dispatch layer *and* delete those 51 arms in the same change, or the drift doubles.

### State: one services struct replaces 18 `State<T>` injections

Injection counts across the surface:

| Injected | Commands | | Injected | Commands |
| --- | ---: | --- | --- | ---: |
| `State<Db>` | 149 | | `State<FftService>` | 12 |
| `AppHandle` | 53 | | `State<ControllerManager>` | 10 |
| `State<AuthoredDocuments>` | 33 | | `State<MixerManager>` | 10 |
| `State<StateDb>` | 22 | | `State<AnalysisTaskGroup>` | 9 |
| `State<StemCache>` | 20 | | `State<PythonWorkspaceService>` | 6 |
| `State<RenderEngine>` | 18 | | `State<GraphRunStore>` | 6 |
| `State<SyncEngine>` | 17 | | `State<StageLinqManager>` / `<ProDJLinkManager>` / `<FixtureState>` | 2 each |
| `State<HostAudioState>` | 13 | | `State<ArtNetManager>` | 1 |

All 18 are process-global singletons registered once at startup, so they collapse cleanly into a single `&AppServices` parameter. Three wrinkles:

- **`fixtures.rs:80` uses `app.try_state::<ArtNetManager>()`** — a runtime service-locator lookup for an *optionally present* manager. `AppServices` has to model that as `Option<ArtNetManager>`, not a required field.

- **`State<'_, T>` forces the elided lifetime on every async command**, which is why the bodies are `async fn` with `Result<_, String>` and can't hold state across some awaits ergonomically. A plain `&AppServices` removes that constraint entirely — this is the single biggest ergonomic win of the refactor.

- Two injections are dead weight already: `run_graph` takes `_stem_cache` and `preview_pattern` takes `_stem_cache` + `_fft_service`. Drop them rather than threading them through.

### AppHandle: only three real uses, all replaceable

53 commands take `AppHandle`, but the whole surface uses it for exactly three things:

1. **Emitting events** — 11 emit sites in 4 files (`midi.rs`, `tracks.rs`, `rekordbox.rs`, `engine_dj.rs`), every one of them fire-and-forget (`let _ = app.emit(...)`). Replace with an `EventSink` trait on `AppServices`; the harness then gets a recording sink instead of no events, which is strictly better than today.

2. **Resolving paths** — fixtures resource root, app config dir. Replace with resolved `PathBuf`s on `AppServices`, computed once at startup.

3. **`app.exit(0)`** — `sync.rs:93` (`force_quit`). This one is genuinely host control and needs a `Host` capability on the services struct, or `force_quit` stays a thin Tauri-only shim outside the dispatcher.

`AppHandle` is also cloned into spawned tasks (`tracks.rs:156,246`, `rekordbox.rs:158`, `engine_dj.rs:165`) purely to emit progress from the background — so the `EventSink` must be `Clone + Send + 'static`.

### Async patterns to preserve

- **178 of 196 commands are `async fn`.** The 18 sync ones are the cheap in-memory reads (`get_node_types`, the `host_*` transport calls, `controller_list_ports`, `search_fixtures`). A `dispatch` fn should just be `async` uniformly; the sync bodies cost nothing to await.

- **7 `spawn_blocking` sites** wrap genuinely blocking FFI/IO: Rekordbox SQLCipher opens (`with_db` reopens the DB on *every* call — no cached handle), Engine DJ SQLite, fixture XML parsing. These must stay `spawn_blocking`; note that a panic inside surfaces as `"Task join error: …"`, another stringly-typed failure mode.

- **7 `tokio::spawn` fire-and-forget sites** are the import/reprocess pipelines. The command returns `Ok(())` once the task is *spawned*; progress and completion arrive as events. The dispatcher cannot make these synchronous without changing the contract, so it needs the `EventSink` wired before it can host them at all.

- **`AnalysisTaskGroup` epochs** gate the spawned analysis work (`reprocess_track` spawns onto the current epoch). Epoch invalidation is the only cancellation mechanism on the surface — there is no per-command cancel except `cancel_python_cell`.

### Wire decoding

The dispatcher has to reproduce Tauri's argument decoding exactly or the frontend breaks silently:

- flattened args are `camelCase` on the wire and `snake_case` in Rust — 57 args in this manifest carry a `rustName` recording that rename. Either apply `#[serde(rename_all = "camelCase")]` on a per-command args struct, or convert the whole surface to the single-`input`-object form (22 commands already use it, and it's the better shape).

- responses are already `camelCase` via serde on the model types, so `Result<serde_json::Value, _>` at the edge needs no extra work — except `get_fixture_definition`, whose raw-XML shape must not be normalized.

- errors are `String` today. The refactor is the moment to introduce a typed error enum (`NotFound`, `Unauthorized`, `Conflict{expected,found}`, `Invalid`, `Internal`) and serialize it at the edge, so `HeadMoved` and the optimistic-concurrency conflicts stop being parsed out of prose.

## Dead commands

Registered in `lib.rs` with zero callers anywhere in `src/` or `scripts/` (15 of 196). Delete them rather than porting them.

| Command | Domain | Why it's dead |
| --- | --- | --- |
| `controller_list_ports` | `perform/midi-controller` | the controller config dialog reads ports out of `ControllerStatus.availablePorts` instead |
| `create_pattern_category` | `patterns` | categories can be listed and assigned from the UI, but never created |
| `engine_dj_sync_library` | `dj-import/engine-dj` | unreachable from the UI — an unfinished feature; it only counts drift, it never imports or deletes |
| `get_group` | `universe/groups` | every frontend path reads groups via `list_groups` or `get_grouped_hierarchy` |
| `get_melspec` | `tracks/audio` | mel specs reach the UI only through `run_graph`'s `melSpecs` map; its `beatGrid` field is always null on this route |
| `get_patch_hierarchy` | `universe/fixtures` | overlaps `get_grouped_hierarchy`, which is what every frontend path actually calls |
| `get_pending_errors` | `sync` | no error-queue UI exists; it also hand-maps `pending::PendingOp` into a near-identical struct |
| `get_sync_status` | `sync` | no caller, yet it still generates an exported and unused ts-rs binding in `src/bindings/sync.ts` |
| `host_unload` | `audio-host` | no TS call site — segment memory is only reclaimed by being replaced via `host_load_track` / `host_load_segment` |
| `import_track` | `tracks` | the UI only ever uses batched `import_tracks`; the two are not wrappers of each other, and this one is the only emitter of the legacy `track-import-progress` tuple event |
| `log_session_from_state_db` | `auth` | debug-only, and it logs session material to stdout — drop it rather than port it |
| `midi_update_modifier` | `perform/midi` | modifiers can only be created and deleted in the UI; its `Option<Option<Vec<String>>>` groups arg is also serde-ambiguous |
| `retry_pending_op` | `sync` | no error-queue UI exists — but its `notify_one()` nudge is the canonical post-write push pattern and must survive elsewhere |
| `sync_files_v2` | `sync` | the `_v2` suffix is vestigial — there is no v1 command anymore |
| `sync_pull` | `sync` | the pull-only path is unreachable; the UI always goes through `sync_full`, which also drops the returned `PullStats` |

## Known issues

Carried over from the fragment audits; each is a thing a port must either fix or deliberately preserve.

| Issue | Severity | Scope | Summary |
| --- | --- | --- | --- |
| `dead-commands` | cleanup | `create_pattern_category`, `import_track`, `get_melspec` | Registered commands with zero TS callers. Categories can be listed and assigned from the UI but never created; mel specs only reach the UI via run_graph's melSpecs map. See deadCommands for the full registry-level list. |
| `import-progress-two-shapes` | contract | `import_track`, `import_tracks` | One concept, two event names and two payload shapes: import_track's pipeline emits `track-import-progress` as a positional tuple (trackId, step); import_tracks emits `file-import-progress` as a structured ImportProgressEvent. Unify on the structured shape. |
| `pattern-id-arg-drift` | contract | `set_pattern_category` | Parameter naming drift: set_pattern_category takes `patternId` while every other pattern command takes `id`. |
| `missing-push-notify` | correctness | `set_pattern_category`, `verify_pattern` | set_pattern_category is the only pattern mutation that does not call engine.push_notify.notify_one(), so a category change is not pushed until an unrelated write wakes the sync loop. Both it and verify_pattern return void, forcing a get_pattern read-after-write. |
| `update-pattern-full-replace` | correctness | `update_pattern` | update_pattern is a full replace, not a patch. Callers fill untouched fields from possibly-stale local state (`name: pattern?.name ?? ""`), so renaming with an unloaded pattern can blank the name. It also bypasses AuthoredDocuments and writes the SQLite projection directly, unlike create/delete. |
| `pattern-args-venue-divergence` | correctness | `get_pattern_args`, `get_pattern_graph_document` | get_pattern_args loads the whole graph document and discards everything but `.args` — same cost as get_pattern_graph_document. Worse, get_pattern_graph_document passes venue_id = None while get_pattern_args passes a real venue, so the two can resolve DIFFERENT implementations for the same pattern id. |
| `annotation-count-duplicate-sql` | duplication | `get_venue_annotation_counts`, `list_tracks_enriched` | get_venue_annotation_counts duplicates list_tracks_enriched's venueAnnotationCount as raw SQL embedded in the command file instead of living in database/local/tracks.rs. |
| `artifact-version-zero` | correctness | `list_tracks_enriched` | The has* flags fall back to artifact version 0 when the registry has no preprocessor for a table, and version 0 matches any row — so hasX can read true for a stale artifact. |
| `melspec-dead-field` | cleanup | `get_melspec` | get_melspec always returns beatGrid: null on that route; the field is only populated by run_graph's mel specs. Dead field on the command. |
| `dead-params` | cleanup | `run_graph`, `preview_pattern` | Unused injected State params: run_graph takes `_stem_cache`; preview_pattern takes both `_stem_cache` and `_fft_service`. |
| `preview-silent-clamp` | correctness | `preview_pattern` | Silently clamps fps into [10,30] and frame_count into [1,256], so long spans under-sample with no signal to the caller. It also forces every Selection arg to "all", so preview output can differ materially from a real run. |
| `undocumented-idempotency-retry` | contract | `score_dsl_import` | The TS wrapper (src/lib/dsl/index.ts:51) blind-retries the identical request exactly once on ANY failure. That is only safe because operationId makes the write idempotent — an invariant undocumented at the call site that any port must preserve. |
| `score-dsl-toctou` | correctness | `score_dsl_import` | Takes VenueAccess::<Write> for the scope check then drops it before the authored apply — a TOCTOU window between authorization and write. |
| `opaque-invoke-payload` | contract | `save_pattern_graph_document` | The caller (pattern-editor.tsx:1755) passes an opaque `input` object straight into invoke, so the TS object keys ARE the Rust arg names with no typecheck between them. Renaming a Rust arg breaks the call silently at runtime. |
| `duplicate-domain-naming` | cleanup | `universe/groups` | Fragment authors split src-tauri/src/commands/groups.rs across two domain labels (`stage/groups` and `universe/groups`). Canonicalized here to universe/groups; the underlying file is one module and should stay one domain. |
| `orphan-events` | cleanup | `controller_port_change`, `universe-buffer`, `dmx://update` | `controller_port_change` (controller_manager.rs:295) is emitted and never listened to. `universe-buffer` and `dmx://update` are the mirror image — the visualizer stores subscribe to them but nothing in src-tauri/ has ever emitted them; delete the listeners. (`close-requested` is not in this set: Tauri's own window lifecycle emits it.) |

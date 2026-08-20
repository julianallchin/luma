# Dispatcher port guide

How to move a command off `#[tauri::command]` and onto the dispatch seam. The
seam and 12 commands are already landed in `src-tauri/src/dispatch/`; the
remaining ~184 are a mechanical fan-out, and this is the recipe.

Read [`ipc-manifest.md`](./ipc-manifest.md) first for what each command actually
does — this document only covers the mechanics of moving it.

## Why the seam exists

The command surface used to have no existence apart from Tauri: a body needed an
`AppHandle` and a `State<T>` registry to be called at all. The headless agent
harness worked around that by hand-writing a second `match name { … }` with its
own re-implementation of 51 command bodies. One surface, two forks. The seam
deletes the fork and makes a future non-Tauri host (GPUI) a third adapter over
the same bodies rather than a fourth implementation.

## The interface

The whole seam, from outside:

```rust
let services = AppServices::headless(db, state_db, storage, fixtures_root, workspaces);
let result: Result<serde_json::Value, CommandError> =
    dispatch(&services, "get_pattern_args", &args).await;
```

Plus `Events`/`EventSink` and `HostControl`/`Host` for a host that wants to
observe events or own process termination — both default to a no-op, so a
minimal host implements neither — and `handles(name)` for a host that still
implements some commands itself. That is the public surface.

## Inside

```
src-tauri/src/dispatch/
  mod.rs                 the commands! table -> `adapter` (Tauri) + `dispatch` (JSON),
                         and the wire decoding, which knows the same schema
  handlers/<domain>.rs   behavior:  async fn(&AppServices, args…) -> Result<T, CommandError>
  services.rs            AppServices, Events/EventSink, Host/HostControl
  error.rs               CommandError
  tauri_host.rs          the Tauri-backed EventSink and Host
```

The Tauri adapter and the JSON dispatcher are both **generated from one table**,
so a command's wire name, argument names, argument types and return type are
declared exactly once.

## The recipe

### 1. Move the body into `dispatch/handlers/<domain>.rs`

Same name as the wire name. Signature transform:

| Old | New |
| --- | --- |
| `db: State<'_, Db>` | drop it; use `services.db.0` |
| `app: AppHandle` (emitting) | drop it; use `services.events.emit(name, payload)` |
| `app: AppHandle` (paths) | drop it; use `services.storage` / `services.fixtures_root` |
| `app: AppHandle` (`exit`) | drop it; use `services.host.exit(0)` |
| `let _ = app.emit(…)` | `services.events.emit(…)` — it returns `()`, there is nothing to drop |
| `State<'_, T>` for any other singleton | drop it; use `services.<field>` |
| `-> Result<T, String>` | `-> Result<T, CommandError>` |

`impl From<String> for CommandError` means every `?` on an existing
`Result<_, String>` service call keeps working; retype the interesting failures
(`NotFound`, `Unauthorized`, `Conflict`) as you go, and leave the rest as
`Internal`.

`CommandError`'s `Display` is a **verbatim passthrough** of the message. Never
add a prefix — the string is the wire contract. Structure goes in the variant,
not in the prose.

### 2. Add a row to the `commands!` table in `dispatch/mod.rs`

```
<domain>::<wire name>(<arg>: <Ty>, …) -> <ReturnTy>;
```

Argument names must be the handler's parameter names in `snake_case`; Tauri
already renames them to `camelCase` on the wire, and `args::decode` accepts both
spellings on the JSON path. Import the return type at the top of `mod.rs`.

### 3. Point the registration at the adapter

In `lib.rs`'s `invoke_handler`, change `commands::<domain>::<name>` to
`dispatch::adapter::<name>`. Delete the old `#[tauri::command]` fn.

Once every command is ported, that whole list collapses into a single generated
invocation and this step disappears.

### 4. Delete the duplicate in `agent_harness.rs`

If the command has an arm in the harness's legacy `match`, delete it. The
harness checks `dispatch::handles(cmd)` first, so the ported command resolves
through the shared body automatically. When the last arm goes, the `match`, the
`arg`/`opt_arg`/`ok` helpers, the transitional `AppServices` accessors they use,
and most of the harness's imports go with it.

### 5. Keep it green

```
cargo check --manifest-path src-tauri/Cargo.toml --all-targets
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets
cargo fmt --manifest-path src-tauri/Cargo.toml
bunx vitest run
```

The frontend must not change. If it does, the port is wrong.

## Worked example: `get_pattern_args`

Before (`src/commands/patterns.rs`):

```rust
#[tauri::command]
pub async fn get_pattern_args(
    db: State<'_, Db>,
    id: String,
    venue_id: Option<String>,
    implementation_id: Option<String>,
) -> Result<Vec<PatternArgDef>, String> {
    db::get_pattern_pool(&db.0, &id).await?;
    let document = load_visible_graph_document(
        &db.0, &id, venue_id.as_deref(), implementation_id.as_deref(),
    ).await.map_err(|error| error.to_string())?;
    Ok(document.graph.args)
}
```

After (`src/dispatch/handlers/patterns.rs`):

```rust
pub async fn get_pattern_args(
    services: &AppServices,
    id: String,
    venue_id: Option<String>,
    implementation_id: Option<String>,
) -> Result<Vec<PatternArgDef>, CommandError> {
    let pool = &services.db.0;
    db::get_pattern_pool(pool, &id).await?;
    let document =
        load_visible_graph_document(pool, &id, venue_id.as_deref(), implementation_id.as_deref())
            .await
            .map_err(|error| CommandError::Internal(error.to_string()))?;
    Ok(document.graph.args)
}
```

Table row (`src/dispatch/mod.rs`):

```
patterns::get_pattern_args(
    id: String,
    venue_id: Option<String>,
    implementation_id: Option<String>,
) -> Vec<PatternArgDef>;
```

`lib.rs`: `commands::patterns::get_pattern_args` → `dispatch::adapter::get_pattern_args`.
`agent_harness.rs`: the `"get_pattern_args" => { … }` arm is deleted.

Net effect: one implementation instead of two, the same bytes on the wire.

## Design it twice

Three shapes were sketched before this one was built.

**A. Trait-object registry.** `trait Command { fn name(&self) -> &str; async fn
run(&self, &AppServices, Value) -> Result<Value, CommandError>; }` with a
`HashMap<&str, Box<dyn Command>>` built at startup. Genuinely extensible — a
plugin could register a command — and dispatch is a map lookup rather than a
196-arm match.

Rejected because extensibility is not a requirement here: the command set is
closed and known at compile time, and paying a `Box<dyn>` + object-safety tax
(every command's args become `Value`, so the Tauri adapter loses its concrete
types and has to double-serialize) buys nothing. It also cannot generate the
`#[tauri::command]` wrappers, so the 196 pass-throughs would have to be
hand-written — exactly the smell we are removing.

**B. One request enum.** `enum Command { GetPatternArgs { id: String, … }, … }`
with `#[serde(tag = "cmd")]` and a `match` on it. Very Rust-idiomatic, gives
exhaustiveness checking, and the wire shape is derived by serde rather than by
hand.

Rejected on the wire contract. Tauri does not deliver `{cmd, args}` — it
delivers named arguments to a named function — so an enum would need a
hand-written `#[tauri::command]` per variant anyway, plus a variant-to-function
mapping. Two lists instead of one. It also makes every command's arguments a
public type in one namespace, which is a large interface for no gain.

**C. Macro-generated table (chosen).** One row per command generates the Tauri
wrapper, the JSON dispatch arm, and the name registry. The declaration is the
single source of truth for the wire name, the argument names, the argument types
and the return type — the four things that could drift between two hosts.

It won because it is the only one of the three where *the duplication cannot
come back*. The other two leave the `#[tauri::command]` wrappers to be written
by hand, which is where drift lives. The cost is real — macro-generated code is
harder to read and `cargo expand` is sometimes needed to understand a compile
error — and that cost is paid once, in one file, by the people maintaining the
seam rather than by the ~184 commands passing through it.

## What is already ported

Twelve commands, chosen to cover every hard shape rather than to make the count
look good:

| Command | Shape it proves |
| --- | --- |
| `get_node_types` | zero-arg, formerly sync |
| `list_patterns` | zero-arg async, db only |
| `get_pattern_args` | flattened args with `camelCase` renames + `Option`s |
| `save_pattern_graph_document` | idempotent write, `SyncEngine.push_notify` |
| `agent_thread_create` | single-`input`-object form |
| `agent_thread_append_messages` | typed `Conflict` from a typed service outcome |
| `agent_thread_rename` | `Option` argument |
| `get_patched_fixtures` | the `Option<ArtNetManager>` case |
| `get_track_waveform` | `AnalysisTaskGroup`, large `Vec<f32>` payload |
| `update_track_metadata` | event emission |
| `midi_release_cue` | conditional double emission + a manager singleton |
| `force_quit` | host control (`exit`) |

Nine of these had duplicate bodies in `agent_harness.rs`; all nine are gone.

## Special cases

### `AppServices` does not hold every singleton yet

Only the services the ported commands need are fields today. Adding a command
that needs `StemCache`, `FixtureState`, `HostAudioState`, `ControllerManager`,
`MixerManager`, `StageLinqManager` or `ProDJLinkManager` means adding the field,
constructing it in `AppServices::headless`, and passing it in
`dispatch::tauri_host::app_services`. Do that in the same change as the command;
do not add speculative fields.

Two singletons need care because their interior is **not** `Arc`-shared —
`PythonWorkspaceService` and `GraphRunStore` hold bare `Mutex<HashMap<…>>`, so a
clone forks the state. They are held as `Arc<T>` in both `AppServices` and
Tauri's managed state. `Db`, `StateDb` and `SyncEngine` are `Clone` instead,
because every field inside them is already a shared handle.

### `ArtNetManager` is genuinely optional

`ArtNetManager::new` takes an `AppHandle`, so no headless host can build one.
`AppServices::artnet` is `Option<Arc<ArtNetManager>>` and `headless` sets it to
`None`. This replaces the runtime `app.try_state::<ArtNetManager>()` lookup.

### The four spawned-progress commands are blocked

`import_tracks`, `reprocess_track`, `rekordbox_import_tracks` and
`engine_dj_import_tracks` clone an `AppHandle` into a background task purely to
emit progress. `Events` is `Clone + Send + 'static` and handles that fine — but
their bodies also call `services::tracks::{file_fast_import,
run_background_analysis}`, and `run_background_analysis` threads the `AppHandle`
down through `preprocessing::scheduler` into every preprocessor, using it for
storage paths as well as emits.

**Porting these four means first refactoring `services/tracks.rs` and
`preprocessing/` to take `(&StorageRoot, &Events)` instead of `AppHandle`.** That
is a real piece of work, not a mechanical rename, and it should land as its own
change before these four commands move. Everything they need from the seam
already exists.

### Two different notions of "current user"

`AppServices` exposes both, deliberately:

- `admitted_principal()` reads the app database's signed-write admission gate.
- `session_user_id()` reads the verified host session in the state database, and
  can refresh a Supabase token.

Which one a command uses is historical, not designed — `agent_thread_create`
uses the first, `save_pattern_graph_document` uses the second. **Preserve
whichever the command used**; unifying them is a separate decision with real
auth consequences. Both honour `fixture_principal`, which is how the harness's
`--fixture-principal` flag stays a one-line override instead of a fork.

### `get_fixture_definition` takes an unsanitized path

Its `path` argument is joined to the fixtures root without checking for `..`, so
it escapes the root. Fix that in the handler when porting it — the seam is the
right layer for the check.

### Dead commands: delete, don't port

15 commands have zero callers anywhere in `src/` or `scripts/`. They are listed
in the manifest's "Dead commands" table. Delete them rather than moving them.

### Behavior changes made deliberately so far

- `update_track_metadata` no longer fails when its `library-changed` event
  cannot be delivered. It used to return an error *after* the write committed,
  reporting a rollback that never happened. `Events::emit` returns `()`, so the
  error is gone from the API rather than dropped at each of 11 call sites.
- `midi_release_cue` releases the render-engine lock before emitting rather than
  emitting while holding it.
- `save_pattern_graph_document` now calls `push_notify` on the headless path too
  (the harness's forked body did not). Harmless there — nothing runs the sync
  loop — and it removes the divergence.

## Things worth fixing while you are in there

Flagged during the seam work, not fixed:

- `commands/tracks.rs::get_venue_annotation_counts` embeds raw SQL in the
  command file that duplicates `list_tracks_enriched`'s `venueAnnotationCount`.
  It belongs in `database/local/tracks.rs`.
- `database/local/agent_threads.rs::append_messages` is now only used by tests;
  the production path goes through `append_messages_at_head` and the typed
  `Conflict`. Consider folding it into the test helpers.
- `commands/node_graph.rs::run_graph` takes `_stem_cache` and `preview_pattern`
  takes `_stem_cache` + `_fft_service`, all unused. Drop them when porting.

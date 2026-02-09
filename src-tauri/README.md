# Rust Backend

## Overview

The backend is a Rust application built with Tauri that provides the core services for Luma. The entry point is `main.rs` which calls `luma_lib::run()` from `lib.rs`. The `lib.rs` file sets up the Tauri application, initializes the databases, registers command handlers, and starts background services.

### Database / Services / Commands split

- `models/` — data shapes only (no logic).
- `database/local/` — pure SQL helpers on `&SqlitePool` (CRUD, no filesystem or side effects).
- `services/` — business logic and orchestration (filesystem, workers, ArtNet, audio/DSP).
- `commands/` — Tauri wrappers that pull state (`State<'_, Db>`, `AppHandle`, caches) and delegate to services.

The local SQLite DB (`luma.db`) is initialized in `database::init_app_db()` and stored in the app config dir. Tables cover patterns, tracks (plus beats/roots/stems/waveforms), scores, fixtures, venues, and implementations.

### Tracks

Service: orchestrates imports (hash/copy, lofty metadata, album art), storage layout, and workers (beats, roots, stems, waveforms, mel spec) with mutex guards to avoid duplicate work. DB: `database/local/tracks.rs` holds only the queries/upserts. Commands: thin wrappers in `commands/tracks.rs` delegate to the service with `&db.0`/state.

### Patterns

The `patterns` module manages pattern definitions. Patterns are stored in the local database with their name and description. Graph implementations live alongside patterns in the same database, with `default_implementation_id` referencing the default graph for a pattern. When you call `get_pattern_graph`, it looks up the default implementation. When you call `save_pattern_graph`, it updates or creates the default implementation.

### Schema

The `schema` module defines all the types used for graph execution. It includes `NodeTypeDef` which describes what nodes are available (their inputs, outputs, parameters), `Graph` which is the serialized graph structure (nodes and edges), and the `run_graph` command which executes a graph. The graph execution uses `petgraph` to build a directed graph, then topologically sorts the nodes to determine execution order. It processes each node type (pattern_entry, view_channel, mel_spec_viewer, etc.) and passes data between nodes through their ports. The execution returns view data that gets displayed in the frontend.

### Playback

The unified `host_audio` module manages audio playback using the `rodio` library. It maintains a `HostAudioState` which holds the currently loaded audio segment (samples, sample rate, beat grid). The host audio system is shared by the pattern editor (segment preview with looping) and the track editor (full track playback). When you call `host_load_segment`, it loads a specific time range of a track for pattern preview. When you call `host_load_track`, it loads the full track for the track editor. The `host_play`, `host_pause`, and `host_seek` commands control playback, and `host_set_loop` enables segment looping for pattern preview. The state broadcasts updates via the `host-audio://state` event every 50 milliseconds. Playback runs in a separate thread using rodio's `Sink` with a custom `LoopingSamples` source that supports live loop toggling.

### Annotations

The `annotations` module manages track scores which are pattern placements on a track's timeline. Scores link a track to a pattern with start and end times and a z-index for layering. The scores are stored in the local database's `scores` and `track_scores` tables, with a default score created on demand.

### Waveforms

Service: decodes audio, computes preview/full buckets, band envelopes, and legacy colors; persists via DB helpers. DB: `database/local/waveforms.rs` stores/fetches serialized waveform rows. Command: `commands/waveforms.rs`.

### Audio

The `audio` module provides utilities for loading and decoding audio files using `symphonia`, and for generating mel spectrograms using FFT operations. The `beat_worker`, `root_worker`, and `stem_worker` modules coordinate with Python scripts to run heavy audio analysis tasks. They use `python_env` to manage the Python environment and spawn blocking tasks to run the Python workers.

### Command Handlers

All command handlers are registered in `lib.rs` using `tauri::generate_handler![]`. The commands are async functions that take `State` parameters to access the databases and other managed state. The `HostAudioState` is managed as application state and spawns a background task that broadcasts playback state updates continuously.

## TypeScript bindings (`ts-rs`)

- Generated from Rust models in `src-tauri/src/models/` via `ts-rs`.
- Not tracked in git (`src/bindings/schema.ts` is ignored).

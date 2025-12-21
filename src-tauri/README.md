# Rust Backend

## Overview

The backend is a Rust application built with Tauri that provides the core services for Luma. The entry point is `main.rs` which calls `luma_lib::run()` from `lib.rs`. The `lib.rs` file sets up the Tauri application, initializes the databases, registers command handlers, and starts background services.

### Database

The application uses a single SQLite database managed through the `database` module. The local database (`luma.db`) is initialized in `database::init_app_db()` and stored in the app's config directory. It contains tables for patterns, tracks, track metadata (beats, roots, stems, waveforms), scores, fixtures, venues, and implementations.

### Tracks

The `tracks` module handles all track-related operations. When you import a track via `import_track`, it computes a SHA256 hash of the file to detect duplicates, copies the file to the app's storage directory, extracts metadata using the `lofty` library, and saves it to the database. Then it kicks off background workers through `run_import_workers` which runs in parallel: `ensure_track_beats_for_path` uses the beat worker to detect beats and downbeats, `ensure_track_roots_for_path` uses the root worker to detect chord progressions, `ensure_track_stems_for_path` uses the stem worker to separate audio into stems, and `ensure_track_waveform` generates waveform preview data. These workers use mutexes to prevent duplicate work if multiple imports happen simultaneously. The beat worker calls Python scripts in `python/beat_worker.py`, the root worker calls `python/root_worker.py`, and the stem worker calls Python scripts for audio source separation. The `get_melspec` command loads audio and generates mel spectrograms for visualization.

### Patterns

The `patterns` module manages pattern definitions. Patterns are stored in the local database with their name and description. Graph implementations live alongside patterns in the same database, with `default_implementation_id` referencing the default graph for a pattern. When you call `get_pattern_graph`, it looks up the default implementation. When you call `save_pattern_graph`, it updates or creates the default implementation.

### Schema

The `schema` module defines all the types used for graph execution. It includes `NodeTypeDef` which describes what nodes are available (their inputs, outputs, parameters), `Graph` which is the serialized graph structure (nodes and edges), and the `run_graph` command which executes a graph. The graph execution uses `petgraph` to build a directed graph, then topologically sorts the nodes to determine execution order. It processes each node type (pattern_entry, view_channel, mel_spec_viewer, etc.) and passes data between nodes through their ports. The execution returns view data that gets displayed in the frontend.

### Playback

The unified `host_audio` module manages audio playback using the `rodio` library. It maintains a `HostAudioState` which holds the currently loaded audio segment (samples, sample rate, beat grid). The host audio system is shared by the pattern editor (segment preview with looping) and the track editor (full track playback). When you call `host_load_segment`, it loads a specific time range of a track for pattern preview. When you call `host_load_track`, it loads the full track for the track editor. The `host_play`, `host_pause`, and `host_seek` commands control playback, and `host_set_loop` enables segment looping for pattern preview. The state broadcasts updates via the `host-audio://state` event every 50 milliseconds. Playback runs in a separate thread using rodio's `Sink` with a custom `LoopingSamples` source that supports live loop toggling.

### Annotations

The `annotations` module manages track scores which are pattern placements on a track's timeline. Scores link a track to a pattern with start and end times and a z-index for layering. The scores are stored in the local database's `scores` and `track_scores` tables, with a default score created on demand.

### Waveforms

The `waveforms` module generates waveform preview data for tracks. It computes downsampled audio samples and optionally generates 3-band frequency envelopes for rekordbox-style waveform visualization. The waveform data is cached in the `track_waveforms` table.

### Audio

The `audio` module provides utilities for loading and decoding audio files using `symphonia`, and for generating mel spectrograms using FFT operations. The `beat_worker`, `root_worker`, and `stem_worker` modules coordinate with Python scripts to run heavy audio analysis tasks. They use `python_env` to manage the Python environment and spawn blocking tasks to run the Python workers.

### Command Handlers

All command handlers are registered in `lib.rs` using `tauri::generate_handler![]`. The commands are async functions that take `State` parameters to access the databases and other managed state. The `HostAudioState` is managed as application state and spawns a background task that broadcasts playback state updates continuously.

## TypeScript bindings (`ts-rs`)

- Generated from Rust models in `src-tauri/src/models/` via `ts-rs`.
- Not tracked in git (`src/bindings/schema.ts` is ignored).

# Rust Backend

## Overview

The backend is a Rust application built with Tauri that provides the core services for Luma. The entry point is `main.rs` which calls `luma_lib::run()` from `lib.rs`. The `lib.rs` file sets up the Tauri application, initializes the databases, registers command handlers, and starts background services.

### Database

The application uses two SQLite databases managed through the `database` module. The global database (`luma.db`) is initialized in `database::init_app_db()` and stored in the app's config directory. It contains tables for patterns, tracks, track metadata (beats, roots, stems, waveforms), track annotations, and recent projects. The project database is a separate SQLite file (the `.luma` project file) that gets opened when you create or open a project. It's managed through `ProjectDb` which is a mutex-wrapped optional connection pool. The project database contains the `implementations` table which stores the actual graph JSON for each pattern implementation. This separation follows the architecture where patterns are defined in the global library but their implementations are stored per-project.

### Tracks

The `tracks` module handles all track-related operations. When you import a track via `import_track`, it computes a SHA256 hash of the file to detect duplicates, copies the file to the app's storage directory, extracts metadata using the `lofty` library, and saves it to the database. Then it kicks off background workers through `run_import_workers` which runs in parallel: `ensure_track_beats_for_path` uses the beat worker to detect beats and downbeats, `ensure_track_roots_for_path` uses the root worker to detect chord progressions, `ensure_track_stems_for_path` uses the stem worker to separate audio into stems, and `ensure_track_waveform` generates waveform preview data. These workers use mutexes to prevent duplicate work if multiple imports happen simultaneously. The beat worker calls Python scripts in `python/beat_worker.py`, the root worker calls `python/root_worker.py`, and the stem worker calls Python scripts for audio source separation. The `get_melspec` command loads audio and generates mel spectrograms for visualization, and `load_track_playback` loads the full audio samples into the playback system.

### Patterns

The `patterns` module manages pattern definitions. Patterns are stored in the global database with just their name and description. The actual graph implementations are stored in the project database's `implementations` table. When you call `get_pattern_graph`, it looks up the implementation in the project database. When you call `save_pattern_graph`, it saves the graph JSON to the project database. This way patterns are portable across projects but each project can have different implementations of the same pattern.

### Schema

The `schema` module defines all the types used for graph execution. It includes `NodeTypeDef` which describes what nodes are available (their inputs, outputs, parameters), `Graph` which is the serialized graph structure (nodes and edges), and the `run_graph` command which executes a graph. The graph execution uses `petgraph` to build a directed graph, then topologically sorts the nodes to determine execution order. It processes each node type (pattern_entry, view_channel, mel_spec_viewer, etc.) and passes data between nodes through their ports. The execution returns view data that gets displayed in the frontend.

### Playback

The `playback` module manages audio playback using the `rodio` library. It maintains a `PatternPlaybackState` which holds a map of playback entries (audio samples, sample rates, beat grids). When you call `playback_play_node`, it starts playing audio from the specified entry. The playback runs in a separate thread and uses rodio's `Sink` to play audio samples. The state tracks the current playback position and broadcasts updates via Tauri events every 50 milliseconds. The frontend listens to these events to update the UI. The `playback_pause` and `playback_seek` commands control playback position.

### Project Manager

The `project_manager` module handles creating, opening, and closing project files. When you create a project, it calls `init_project_db` which creates a new SQLite file and initializes the implementations table. When you open a project, it opens the existing database file and stores the connection in `ProjectDb`. When you close a project, it closes the database connection. It also updates the `recent_projects` table in the global database to track recently opened projects.

### Annotations

The `annotations` module manages track annotations which are pattern placements on the timeline. Annotations link a track to a pattern with start and end times and a z-index for layering. The annotations are stored in the global database's `track_annotations` table.

### Waveforms

The `waveforms` module generates waveform preview data for tracks. It computes downsampled audio samples and optionally generates 3-band frequency envelopes for rekordbox-style waveform visualization. The waveform data is cached in the `track_waveforms` table.

### Audio

The `audio` module provides utilities for loading and decoding audio files using `symphonia`, and for generating mel spectrograms using FFT operations. The `beat_worker`, `root_worker`, and `stem_worker` modules coordinate with Python scripts to run heavy audio analysis tasks. They use `python_env` to manage the Python environment and spawn blocking tasks to run the Python workers.

### Command Handlers

All command handlers are registered in `lib.rs` using `tauri::generate_handler![]`. The commands are async functions that take `State` parameters to access the databases and other managed state. The `PatternPlaybackState` is managed as application state and spawns a background task that broadcasts playback state updates continuously.

## TypeScript bindings (`ts-rs`)

- Generated from Rust models in `src-tauri/src/models/` via `ts-rs`.
- Not tracked in git (`src/bindings/schema.ts` is ignored).

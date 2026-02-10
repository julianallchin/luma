# Luma Developer Guide

## Architecture Overview

Luma is a Tauri 2 desktop app with a React/TypeScript frontend and a Rust backend. The frontend handles UI rendering and user interaction; the backend handles all data processing, audio analysis, graph execution, DMX output, and network communication.

### Tech Stack

- **Frontend**: React 19, TypeScript, Zustand (state), React Flow (graph editor), Three.js (3D visualizer), Tailwind CSS, Vite, Bun
- **Backend**: Rust, Tauri 2, SQLite (sqlx), petgraph (graph execution), symphonia (audio decode), rodio (audio playback), ArtNet (DMX output)
- **ML Workers**: Python (beat_this for beats, demucs for stems, consonance-ACE for chords)
- **Networking**: StageLinQ (DJ equipment), ArtNet (DMX broadcast)

### Data Architecture: The "What" / "How" Split

Luma separates hardware-agnostic creative data from venue-specific hardware configuration:

**Global Library (`luma.db`)** -- stored in the Tauri app config directory:
- macOS: `~/Library/Application Support/com.luma.luma/luma.db`
- Patterns (definitions + node graphs)
- Tracks (metadata, beats, waveforms, stems, chord roots)
- Pattern categories
- App settings
- Syncs bidirectionally to Supabase for cloud backup and community sharing

**Venue Projects (`.luma` files)** -- user-chosen locations:
- Patched fixtures (DMX addresses, 3D positions, rotations)
- Fixture groups and tags
- Annotations/scores (pattern placements on track timelines)
- Venue-specific implementation overrides

### Module Organization

**Backend** (`src-tauri/src/`):

```
src-tauri/src/
  lib.rs                    # App initialization, command registration
  models/                   # Data structures (serde + ts-rs)
    node_graph.rs           # Signal, Graph, NodeInstance, Edge, Selection, BlendMode
    fixtures.rs             # PatchedFixture, FixtureDefinition, Channel, Capability
    groups.rs               # FixtureGroup, GroupMember, SelectionQuery
    tags.rs                 # FixtureTag, TagAssignment
    implementations.rs      # Implementation (graph JSON storage)
    patterns.rs             # Pattern, PatternCategory
    scores.rs               # Score, TrackScore (annotations)
    tracks.rs               # Track, TrackBeats, TrackStems, MelSpec
    universe.rs             # UniverseState, PrimitiveState
    venues.rs               # Venue
    waveforms.rs            # WaveformData
  database/
    local/                  # SQLite CRUD (16 files, pure SQL via sqlx)
    remote/                 # Supabase sync (15 files)
  services/                 # Business logic
    tracks.rs               # Track import orchestration
    groups.rs               # Group management, tag expression parsing, spatial queries
    fixtures.rs             # Fixture operations
    tags.rs                 # Tag management, auto-generation
    waveforms.rs            # Waveform generation
    cloud_sync.rs           # Bidirectional Supabase sync
    community_patterns.rs   # Community pattern sharing
  commands/                 # Tauri command handlers (thin wrappers)
  node_graph/               # Graph execution engine
    executor.rs             # Topological sort, node evaluation loop, ADSR
    context.rs              # Audio/beat loading, execution context
    state.rs                # ExecutionState (output accumulator)
    node_execution_context.rs # Per-execution context struct
    circle_fit.rs           # PCA + RANSAC circle fitting
    nodes/                  # Node type implementations
      apply.rs              # Output nodes (dimmer, color, position, strobe, speed)
      audio.rs              # Audio processing (stems, filters, beats, frequency)
      analysis.rs           # Music analysis (harmony, tension, mel spec)
      selection.rs          # Fixture selection (tags, attributes, random mask)
      color.rs              # Color generation (constant, gradient, chroma, spectral)
      signals.rs            # Signal math (math, ramp, noise, orbit, remap, etc.)
      mod.rs                # Node dispatch
  compositor.rs             # Track compositing, blend modes, layer caching
  render_engine.rs          # Real-time frame rendering (~60fps loop)
  engine/
    mod.rs                  # render_frame(): sample LayerTimeSeries at a point in time
  fixtures/
    engine.rs               # DMX generation, capability mapping
    parser.rs               # QLC+ .qxf XML parsing
    layout.rs               # Multi-head 3D offset computation
  audio/                    # Audio DSP
    decoder.rs              # Symphonia audio decoding
    cache.rs                # Audio sample caching
    analysis.rs             # FFT, mel spectrograms
    fft.rs, melspec.rs      # DSP utilities
    filters.rs              # IIR lowpass/highpass
    resample.rs             # Audio resampling
    stem_cache.rs           # Demucs stem caching
  artnet.rs                 # ArtNet DMX broadcast + node discovery
  host_audio.rs             # Audio playback (rodio)
  engine_dj/                # Engine DJ database integration
  stagelinq_manager.rs      # DJ device connection
  python_env.rs             # Python runtime management
  beat_worker.rs            # Background beat analysis
  stem_worker.rs            # Background stem separation
  root_worker.rs            # Background chord analysis
  settings.rs               # App settings persistence
```

**Frontend** (`src/`):

```
src/
  main.tsx                  # Entry point
  App.tsx                   # Main layout, view routing
  features/
    app/                    # Welcome screen, project dashboard
    patterns/               # Pattern editor (React Flow graph)
    track-editor/           # Timeline annotation editor
    tracks/                 # Track browser
    engine-dj/              # Engine DJ import UI
    venues/                 # Venue management
    universe/               # DMX fixture patching
    visualizer/             # 3D stage visualizer (Three.js)
    perform/                # Live performance deck display
    auth/                   # Supabase authentication
    settings/               # App settings
  shared/
    components/
      ui/                   # Shadcn UI components
      gradient-picker.tsx   # Color gradient editor
    lib/
      react-flow/           # React Flow editor utilities
      react-flow-editor.tsx # Graph editor wrapper
      utils.ts              # General utilities
```

---

## The Signal System

### Signal: A 3D Tensor

The `Signal` struct is the fundamental data type in Luma's graph engine. Defined in `/Users/julian/github/luma/src-tauri/src/models/node_graph.rs`:

```rust
pub struct Signal {
    pub n: usize,        // Spatial dimension (fixture count)
    pub t: usize,        // Temporal dimension (time samples)
    pub c: usize,        // Channel dimension (data components)
    pub data: Vec<f32>,  // Flat buffer, row-major
}
```

**Memory layout**: `data[n_idx * (t * c) + t_idx * c + c_idx]`

**Dimension semantics**:

- **N (spatial)**: Typically matches the number of items in a Selection. N=1 means "same value for all fixtures" (broadcasts during apply). N=10 means "per-fixture variation."
- **T (temporal)**: Time samples across the pattern's duration. Sampled at `SIMULATION_RATE` (60 Hz, defined as a constant in `nodes/mod.rs`). T=1 means constant over time.
- **C (channel)**: Data channels per sample. C=1 for dimmer/scalar, C=3 for RGB, C=4 for RGBA, C=2 for pan/tilt, C=12 for chroma.

### Broadcasting Rules

When two signals are combined in a binary operation (math node), dimensions broadcast:

- Output shape: `max(a.dim, b.dim)` for each of N, T, C
- If a dimension is 1 in one operand, it is repeated to match the other
- If both dimensions are >1 and different, modulo wrapping is used

This means:

- A color signal (N=1, T=1, C=4) multiplied by a per-fixture dimmer (N=10, T=256, C=1) produces an (N=10, T=256, C=4) result -- per-fixture, time-varying color
- Time signals automatically expand across fixtures; spatial signals expand across time

### Signal Flow: Graph to DMX

```
Signal (N x T x C tensor)
  | Apply nodes convert to...
PrimitiveTimeSeries (per fixture)
  |-- dimmer: Series(dim=1)
  |-- color: Series(dim=3 or 4)
  |-- position: Series(dim=2, pan/tilt degrees)
  |-- strobe: Series(dim=1)
  '-- speed: Series(dim=1)
  | Compositor merges layers by z-index...
LayerTimeSeries (all fixtures, full track duration)
  | Render engine samples at current time...
UniverseState (HashMap<primitive_id, PrimitiveState>)
  | DMX engine maps to hardware channels...
[u8; 512] per universe
  | ArtNet broadcasts...
UDP packets to port 6454
```

### Series and Sampling

A `Series` is the time-series representation for a single fixture's channel. Defined in `/Users/julian/github/luma/src-tauri/src/models/node_graph.rs`:

```rust
pub struct Series {
    pub dim: usize,                   // Number of channels
    pub labels: Option<Vec<String>>,  // Channel names (optional)
    pub samples: Vec<SeriesSample>,
}

pub struct SeriesSample {
    pub time: f32,              // Absolute time in seconds
    pub values: Vec<f32>,       // Channel values at this time
    pub label: Option<String>,
}
```

At render time, the compositor uses binary search (`partition_point`) for O(log n) lookup of the nearest sample, with optional linear interpolation between adjacent samples. See the `sample_series()` function in `/Users/julian/github/luma/src-tauri/src/compositor.rs`.

---

## The Node Graph Engine

### Execution Model

File: `/Users/julian/github/luma/src-tauri/src/node_graph/executor.rs`

1. **Graph Loading**: Parse `graph_json` from the implementation into `Graph { nodes, edges, args }`
2. **Dependency Resolution**: Build a `petgraph::Graph` from edges, run `toposort()` for execution order. Cycles are detected and rejected with an error.
3. **Context Loading**: If any node requires audio/beats (audio_input, beat_clock, stem_splitter, etc.), load the track's audio and beat grid via `context::load_context()`.
4. **Sequential Execution**: Process each node in topological order:
   - Gather incoming edges (lookup source node outputs from `ExecutionState`)
   - Execute node logic via `nodes::run_node()` (dispatch by `type_id`)
   - Store outputs in `ExecutionState` keyed by `(node_id, port_id)`
5. **Output Merging**: Collect all `apply_*` node outputs into a unified `LayerTimeSeries`. Multiple apply outputs for the same primitive use last-write-wins merging.
6. **Frame Rendering**: Render a preview frame at `start_time` via `engine::render_frame()` for immediate frontend visualization.

### ExecutionState

Defined in `/Users/julian/github/luma/src-tauri/src/node_graph/state.rs`:

```rust
pub struct ExecutionState {
    pub audio_buffers: HashMap<(String, String), AudioBuffer>,
    pub beat_grids: HashMap<(String, String), BeatGrid>,
    pub selections: HashMap<(String, String), Vec<Selection>>,
    pub signal_outputs: HashMap<(String, String), Signal>,
    pub apply_outputs: Vec<LayerTimeSeries>,
    pub color_outputs: HashMap<(String, String), String>,
    pub root_caches: HashMap<i64, RootCache>,
    pub view_results: HashMap<String, Signal>,
    pub mel_specs: HashMap<String, MelSpec>,
    pub color_views: HashMap<String, String>,
    pub node_timings: Vec<NodeTiming>,
}
```

All outputs are keyed by `(node_id, port_id)` tuples. Different output types (signals, selections, audio buffers, beat grids) are stored in separate HashMaps so type safety is maintained without runtime casting.

### Node Dispatch

File: `/Users/julian/github/luma/src-tauri/src/node_graph/nodes/mod.rs`

The top-level `run_node()` function dispatches to submodule handlers in order: selection, audio, signals, color, apply, analysis. Each submodule's `run_node()` returns `Ok(true)` if it handled the node type, or `Ok(false)` to pass to the next handler. This is a chain-of-responsibility pattern that keeps node implementations organized by category.

Each node receives a `NodeExecutionContext` (defined in `/Users/julian/github/luma/src-tauri/src/node_graph/node_execution_context.rs`) with:
- Access to incoming edges and source node outputs
- Database pools (main + project)
- FFT service, stem cache
- Graph context (track, venue, timing info)
- Pattern argument definitions and values
- Configuration flags (compute_visualizations, log settings)
- Pre-loaded audio buffer and beat grid from context loading

### Key Execution Details

**ADSR Envelope Generation** (in `executor.rs`):
- `adsr_durations(span_sec, attack, decay, sustain, release)`: Distributes a time span across attack/decay/sustain/release phases based on relative weights. Weights are normalized so they sum to the total span.
- `calc_envelope(t, peak, attack, decay, sustain, release, sustain_level, a_curve, d_curve)`: Generates a time-domain ADSR envelope value at time `t`. The `peak` parameter is the time of maximum amplitude.
- `shape_curve(x, curve)`: Applies exponential shaping to a linear [0,1] value. Positive curve values (0 to +1) create convex/snappy shapes (power 1-6). Negative values (-1 to 0) create concave/swell shapes. Zero is linear.

**Temporal Alignment**:
- All signals use absolute time (seconds from track start)
- Time mapping in nodes: `t_idx / (t_steps - 1) * duration + start_time`
- SeriesSample times are absolute, not relative to pattern start
- The `SIMULATION_RATE` constant (60.0 Hz) determines the maximum temporal resolution

**Selection Resolution** (in `/Users/julian/github/luma/src-tauri/src/node_graph/nodes/selection.rs`):
- Tag expressions are parsed into an AST by the expression parser in `/Users/julian/github/luma/src-tauri/src/services/groups.rs`
- Supported operators: `&` (AND), `|` (OR), `^` (XOR), `~` (NOT), `>` (fallback)
- Expressions are resolved against the venue's fixture groups and their tags
- Each matched fixture's heads are enumerated with global 3D positions computed from fixture position, rotation, and head layout offsets
- Spatial reference modes: `global` (all matched fixtures in one Selection) or `group_local` (one Selection per group, enabling per-group spatial effects)

---

## The Compositor

File: `/Users/julian/github/luma/src-tauri/src/compositor.rs`

### Purpose

The compositor takes all annotations (pattern placements) on a track, executes their pattern graphs, and merges the results into a single unified `LayerTimeSeries` that covers the entire track duration at 60 samples per second (`COMPOSITE_SAMPLE_RATE`).

### Blend Modes

```rust
Replace   // out = top
Add       // out = min(base + top, 1.0)
Multiply  // out = base * top
Screen    // out = 1 - (1-base)*(1-top)
Max       // out = max(base, top)
Min       // out = min(base, top)
Lighten   // out = max(base, top)  (alias for Max)
Value     // out = top * top + base * (1 - top)
```

For color blending, the `Value` mode uses the luminance of the top color as its own opacity factor, mixing top over base proportionally.

### Compositing Algorithm

1. Execute each annotation's pattern graph to produce a `LayerTimeSeries`
2. For each time sample (60 Hz across the track duration):
   - Initialize defaults: dimmer=0, color=black (RGBA 0,0,0,0), position=NaN, strobe=0, speed=1
   - Find all active annotations at this time
   - Sort by z_index ascending (painter's algorithm -- bottom layer first)
   - For each layer, for each fixture primitive:
     - **Dimmer**: `blend_values(current, layer, mode)` using the annotation's blend mode
     - **Color**: Alpha channel controls tint strength, not opacity. When alpha < 1.0, the layer's color is blended with the inherited color from below. Dimmer acts as the true opacity/intensity. The final color is computed as `blend_color(current, [hue_r, hue_g, hue_b, dimmer_value], Replace)`.
     - **Position**: Override by z-index. NaN values on either axis mean "hold previous valid value" for that axis. This allows a layer to control pan without affecting tilt.
     - **Strobe**: Blended like dimmer using the annotation's blend mode
     - **Speed**: Multiplicative. Any layer setting speed to 0 freezes the fixture (binary: >0.5 = fast, <=0.5 = frozen)

### Color Inheritance

Colors can "inherit" from layers below. If a layer emits a color with alpha < 1.0, it tints the inherited color rather than replacing it. If alpha is near zero, the color passes through unchanged. This enables:

- Base color wash on the bottom layer
- Tint overlay on top with low alpha
- Result: the base color shifted toward the tint

Layers that emit no color at all inherit the available color from below and combine it with their dimmer value. This means a dimmer-only pattern can "reveal" the color defined by a lower layer.

### Caching Strategy

Three levels of caching for performance:

1. **Layer Cache**: Keyed by `(track_id, annotation_id)` with an `AnnotationSignature` that hashes the graph JSON, argument values, z-index, time range, and blend mode. If a pattern's graph and args have not changed, its `LayerTimeSeries` is reused without re-executing the graph.

2. **Composite Cache**: If all annotations' metadata matches the previous composite (same annotation set, same z-indices, same time ranges, same args, same blend modes), return the cached composite immediately without even fetching graph JSON from the database.

3. **Incremental Compositing**: When only some annotations have changed, compute "dirty intervals" (time ranges affected by added, removed, or modified annotations). Only recompute samples within those intervals. Unchanged samples are copied from the cached composite.

Dirty interval calculation:
- Removed annotation: mark its old `[start, end)` as dirty
- Added annotation: mark its `[start, end)` as dirty
- Modified annotation: mark both old and new time ranges as dirty
- Overlapping intervals are merged into a minimal set

### Pre-Positioning

During gaps between annotations (time ranges where no pattern is active for a given fixture), the compositor looks ahead to find the next annotation that will control that fixture. It then sets the fixture's position and color to match the next pattern's starting state. This gives moving head fixtures time to physically travel to their target position before the next cue begins.

Pre-positioning only runs during gaps. If a pattern is active (even with dimmer at zero), the pattern's own position output is respected. This allows patterns to intentionally animate position while the fixture is dark.

---

## The DMX Output Pipeline

### Fixture Capability System

File: `/Users/julian/github/luma/src-tauri/src/fixtures/engine.rs`

The DMX engine maps abstract `PrimitiveState` values to concrete DMX channel values based on each fixture's definition. The `PrimitiveState` struct (defined in `/Users/julian/github/luma/src-tauri/src/models/universe.rs`) contains:

```rust
pub struct PrimitiveState {
    pub dimmer: f32,        // 0.0 - 1.0
    pub color: [f32; 3],    // RGB [0.0 - 1.0]
    pub strobe: f32,        // 0.0 (off) - 1.0 (fastest)
    pub position: [f32; 2], // [PanDeg, TiltDeg]
    pub speed: f32,         // 0.0 (frozen) or 1.0 (fast) - binary
}
```

**PrimitiveState to DMX mapping**:

| Property | DMX Mapping |
|----------|-------------|
| dimmer (0-1) | Master intensity channel, scaled by `max_dimmer` setting. For color wheel fixtures, multiplied by color luminance since the wheel cannot represent brightness. |
| color [R,G,B] (0-1) | RGB channels mapped to 0-255 each. If fixture has a color wheel instead of RGB mixing, the nearest wheel color is selected using perceptual color distance. |
| position [pan, tilt] (degrees) | Converted to 16-bit DMX values (MSB/LSB), normalized within the fixture's `pan_max`/`tilt_max` range. NaN values produce a `Hold` action that preserves the previous frame's value. |
| strobe (0-1) | Mapped to the fixture's shutter/strobe capability range. When zero, the shutter "Open" capability is selected. |
| speed (0/1) | Binary: 0 maps to DMX 255 (slowest/frozen), 1 maps to DMX 0 (fastest). Most fixtures use inverted speed channels. |

**Multi-head fixtures**: Each head maps to a separate primitive (e.g., `fixture-uuid:0`, `fixture-uuid:1`). The engine determines the correct primitive for each channel by checking the fixture definition's `<Head>` channel assignments. Master dimmer channels always read from the fixture-level primitive, even when physically listed inside a head. As a fallback, if no fixture-level primitive exists but `fixture-uuid:0` does, it is used for all unmapped channels.

**Color wheel fixtures**: When a fixture has a color wheel instead of RGB mixing, `map_nearest_color_capability()` computes perceptual color distance between the desired RGB and each wheel position's hex color (parsed from QLC+ capability resources). The distance metric penalizes saturation mismatches, especially for desaturated targets -- a gray target strongly prefers white over a saturated color that happens to be numerically close in RGB space. When the desired color is black/dark, the engine returns `Hold` to avoid flashing the wheel during blackout.

**Pan/tilt inversion**: Ceiling-mounted fixtures (detected when `rot_x` is approximately pi) automatically get inverted pan and tilt. The inversion check uses a tolerance of 0.5 radians around pi.

### ArtNet Broadcasting

File: `/Users/julian/github/luma/src-tauri/src/artnet.rs`

- Standard Art-Net protocol over UDP
- Packet format: `Art-Net\0` header + OpCode 0x5000 (OpDmx, little-endian) + protocol version 14 + sequence number + physical port + universe address + 512 DMX channel values
- Universe addressing: Port Address = (Net << 8) | (Subnet << 4) | (Universe & 0xF)
- Supports both broadcast (255.255.255.255:6454) and unicast to a configured IP
- Node discovery via ArtPoll (OpCode 0x2000) with periodic 3-second polling and ArtPollReply (OpCode 0x2100) parsing
- Socket management: binds to port 6454 on the configured interface, with automatic rebinding when settings change

### Render Engine

File: `/Users/julian/github/luma/src-tauri/src/render_engine.rs`

The render engine runs a continuous async loop at approximately 60fps (16ms sleep between iterations):

1. **Track Editor Mode**: Read current playback time from `HostAudioState` (the rodio playback state), sample the active `LayerTimeSeries` at that time via `engine::render_frame()`.
2. **Perform Mode**: For each deck with a non-zero volume, render its `LayerTimeSeries` at the deck's current time. Blend all contributing decks by weighted average (weights = effective volumes normalized by total volume).
3. **Output**: Emit the resulting `UniverseState` to the frontend via Tauri event (`universe-state-update`) for the 3D visualizer, and to the `ArtNetManager` for DMX broadcast.

The `render_frame()` function (in `/Users/julian/github/luma/src-tauri/src/engine/mod.rs`) samples each primitive's Series by finding the nearest sample via linear scan (closest time distance). Default values: dimmer=0, color=white (so dimmer alone produces visible light), strobe=0, position=(0,0), speed=1.

---

## Fixture Definition System

### QLC+ Format

File: `/Users/julian/github/luma/src-tauri/src/fixtures/parser.rs`

Luma uses the QLC+ community fixture library (thousands of definitions). Stored in the `resources/fixtures/` directory as `.qxf` XML files, organized by manufacturer subdirectories.

Key elements parsed:
- **Channels**: Name, preset (IntensityRed, PositionPan, etc.), group (Colour, Pan, Tilt, Intensity, Speed, Shutter, Gobo, etc.), capabilities (DMX value ranges with presets, color hex codes, labels)
- **Modes**: Named configurations mapping channel numbers to channel definitions, plus head definitions that assign channels to individual heads
- **Physical**: Dimensions (mm), layout grid (width x height cells), pan/tilt maximum ranges, focus properties

### Head Layout Computation

File: `/Users/julian/github/luma/src-tauri/src/fixtures/layout.rs`

For multi-head fixtures (LED bars, pixel strips):

1. Read physical dimensions and layout grid from the fixture definition
2. Compute cell size: `cell_w = width_mm / grid_width`, `cell_h = height_mm / grid_height`
3. Position each head in local coordinates, centered at the fixture's origin
4. If fewer heads than grid cells and they divide evenly, assume grouped layout (e.g., 12 cells / 4 heads = 3 cells per head, positioned at group center)
5. Head positions are returned in millimeters. The selection system (`selection.rs`) converts to meters and applies the fixture's 3D rotation (Euler angles: X roll, Y pitch, Z yaw) to produce global coordinates.

### Fixture Library Index

The fixture library path is resolved at startup from the Tauri resource directory (`resources/fixtures/2511260420`). Manufacturers are organized as subdirectories, with fixture models as `.qxf` files within them. Definitions are loaded on demand: when a patched fixture references a definition path, the `ArtNetManager` and selection system parse it via `parser::parse_definition()`.

---

## Database Schema

### Migrations

Location: `/Users/julian/github/luma/src-tauri/migrations/`

11 migrations total, applied in order by timestamp. Key tables:

| Table | Purpose |
|-------|---------|
| `venues` | Project containers; each `.luma` file is a venue |
| `fixtures` | Patched fixtures with DMX universe, address, 3D position (pos_x/y/z), rotation (rot_x/y/z), manufacturer, model, mode |
| `patterns` | Pattern definitions (name, description, category, publication status) |
| `implementations` | Node graph JSON for each pattern, with UID for cross-device sync |
| `scores` / `track_scores` | Pattern placements on track timelines (start_time, end_time, z_index, blend_mode, args) |
| `tracks` | Audio file references with metadata (title, artist, BPM, key, file path, hash) |
| `track_beats` | Beat positions and downbeats for tracks |
| `track_roots` | Chord root analysis results |
| `track_waveforms` | Binary waveform data (preview + full resolution) |
| `track_stems` | Stem separation file paths |
| `fixture_groups` / `fixture_group_members` | Spatial fixture grouping with axis assignments |
| `fixture_tags` / `fixture_tag_assignments` | Tag system for fixture selection expressions |
| `settings` | Key-value app settings |
| `categories` | Pattern categories |

### Cloud Sync

Bidirectional sync to Supabase is handled by the 15 files in `/Users/julian/github/luma/src-tauri/src/database/remote/`. Each syncable record has:

- `remote_id`: Supabase row ID (BIGINT stored as TEXT in SQLite)
- `uid`: UUID for cross-device identity matching
- `version`: Auto-incremented on each local update
- `synced_at`: Timestamp of last successful sync

Conflict resolution uses version comparison with last-write-wins semantics. The `common.rs` module in the remote database layer provides shared sync utilities.

---

## Selection System Deep Dive

### Tag Expression Parser

File: `/Users/julian/github/luma/src-tauri/src/services/groups.rs` (contains `parse_expression()` and the expression evaluator)

Tag expressions support boolean algebra over fixture group tags:

```
all                    -- all fixtures in the venue
front                  -- fixtures in groups tagged "front"
front & has_color      -- front fixtures with RGB capability
left | right           -- fixtures on either side
circular & ~blinder    -- circular fixtures that are not blinders
has_movement > has_color -- prefer movers, fall back to color
```

Operators: `&` (AND), `|` (OR), `^` (XOR), `~` (NOT), `>` (fallback), with parentheses for grouping.

The `>` (fallback) operator is unique: it evaluates the left side first, and only includes the right side's results if the left side matched zero fixtures. This enables graceful degradation across venues with different fixture inventories.

### Capability Tokens (auto-detected)

These special tokens are resolved by inspecting each fixture's definition at runtime:

- `has_color`: fixture has RGB intensity channels or a color wheel channel
- `has_movement`: fixture has pan/tilt channels
- `has_strobe`: fixture has a shutter/strobe channel

### Spatial Attributes

The `get_attribute` node (in `/Users/julian/github/luma/src-tauri/src/node_graph/nodes/selection.rs`) extracts per-fixture scalar values from a Selection and outputs them as a Signal with N = fixture count, T = 1, C = 1:

| Attribute | Description |
|-----------|-------------|
| `index` | Integer order within the selection (0, 1, 2, ...) |
| `normalized_index` | Order normalized to 0.0-1.0 range |
| `pos_x`, `pos_y`, `pos_z` | Absolute global position (meters) |
| `rel_x`, `rel_y`, `rel_z` | Position relative to selection bounding box (0.0-1.0) |
| `rel_major_span` | Position along the axis with largest physical range |
| `rel_major_count` | Position along the axis with most distinct fixture positions |
| `circle_radius` | Distance from the selection's centroid |
| `angular_position` | Angle on fitted circle via PCA + RANSAC (0.0-1.0) |
| `angular_index` | Index-based angular position: fixtures sorted by angle, then assigned equal spacing (0/n, 1/n, 2/n, ...) |

`rel_major_span` vs `rel_major_count`: The "span" variant picks the axis with the largest physical extent (useful for linear arrangements), while the "count" variant picks the axis with the most distinct position values (useful when fixtures are evenly spaced but the physical range is similar across axes).

### Circle Fitting Algorithm

File: `/Users/julian/github/luma/src-tauri/src/node_graph/circle_fit.rs`

For detecting circular fixture arrangements in arbitrary 3D space:

1. **Centroid**: Compute the mean position of all input points.
2. **PCA plane fitting**: Build the 3x3 covariance matrix of centered points. Extract the two dominant eigenvectors via power iteration with matrix deflation. These two vectors define the best-fit 2D plane containing the points.
3. **Project to 2D**: Map each 3D point onto the fitted plane using dot products with the two basis vectors.
4. **RANSAC circle fit**: Run 100 iterations of random 3-point sampling. For each sample, compute the circumcenter via the analytic formula. Count inliers (points within 2.5 distance units of the circle). Keep the fit with the most inliers. Early exit if 90% of points are inliers.
5. **Refinement**: Apply the Kasa algebraic circle fit on inliers, solving the normal equations via Cramer's rule (3x3 determinant). Recompute inliers with the refined circle.
6. **Angular positions**: `atan2(v - center_v, u - center_u)` normalized from [-pi, pi] to [0, 1].

The algorithm is deterministic (fixed seed for the pseudo-random generator) so results are reproducible across graph executions.

---

## Import Pipeline

### Track Import Flow

File: `/Users/julian/github/luma/src-tauri/src/services/tracks.rs`

```
Import request
  |
1. Hash audio file (SHA-256) for deduplication
  |
2. Copy to managed storage (~/.../tracks/{hash}.ext)
  |
3. Extract metadata (lofty crate: title, artist, album, BPM, key, album art)
  |
4. Parallel workers:
   |-- Beat detection (Python: beat_this) -> beat positions, BPM, downbeats
   |-- Stem separation (Python: demucs htdemucs) -> drums, bass, vocals, other
   '-- Waveform generation (Rust DSP) -> preview + full waveform data
  |
5. After stems complete:
   '-- Harmonic analysis (Python: consonance-ACE) -> chord sections, key
```

Audio is decoded using symphonia and resampled to a target sample rate of 48kHz (`TARGET_SAMPLE_RATE`). Stereo audio is converted to mono for analysis workers.

Progress is communicated to the frontend via Tauri events (`track-status-changed`). In-progress sets (`STEMS_IN_PROGRESS`, `ROOTS_IN_PROGRESS`) prevent duplicate worker spawns for the same track.

### Python Worker Management

File: `/Users/julian/github/luma/src-tauri/src/python_env.rs`

Luma manages a Python virtual environment for ML workers:

- Requires Python 3.12 (>= 3.12, < 3.13, enforced by `PY_MIN_VERSION` and `PY_MAX_VERSION_EXCLUSIVE`)
- Creates a venv in the app's cache directory
- Installs pip dependencies from two requirements files: the main `requirements.txt` (torch, demucs, beat_this, etc.) and `consonance-ACE/requirements.txt`
- Worker scripts are written to the cache directory from bundled source (`ensure_worker_script()`)
- Python resource directories (like consonance-ACE model files) are copied recursively from the build directory to the cache directory on first use
- Dependency installation is fingerprinted by SHA-256 hashing the requirements file contents, so reinstallation only triggers when requirements change

### StageLinQ Integration

Files: `/Users/julian/github/luma/src-tauri/crates/stagelinq/src/`

Custom Rust implementation of Denon's StageLinQ protocol for real-time DJ equipment integration:

- **Discovery** (`discovery.rs`): UDP broadcast/multicast for device detection on the local network
- **Device** (`device.rs`): TCP connection management for individual DJ devices
- **Protocol** (`protocol.rs`): Wire format encoding/decoding
- **Services** (`services/`): State Map service for track metadata and playback position, Beat Info service for real-time BPM and beat phase synchronization

The StageLinQ crate handles macOS multi-homed networking by binding UDP announce sockets per-interface (to the local IP, not 0.0.0.0) and binding TCP sockets to the matching subnet IP. This is necessary because macOS routes 169.254.x.x (link-local) traffic via the WiFi interface when multiple interfaces have link-local routes, and Denon Prime hardware uses link-local addressing.

---

## Design Decisions and Trade-offs

### Why petgraph for Node Execution?

Petgraph provides efficient topological sort (O(V+E)) and cycle detection. The graph is rebuilt from JSON on each execution -- this is fast enough (typically < 1ms for real-world graphs) and avoids the complexity of maintaining a live graph structure with cache invalidation. The `run_graph_internal` function uses a simple `HashMap<&str, NodeIndex>` mapping that is allocated and dropped per execution.

### Why SQLite + sqlx?

- **SQLite**: Zero-configuration, file-based, perfect for a desktop app. Venue projects are standalone `.luma` files that users can move, share, and back up.
- **sqlx**: Compile-time SQL verification via the `sqlx::query!` macro, async support, and type-safe result mapping. No ORM overhead.
- **Two databases**: The global library (`luma.db`) and venue project (`.luma`) are separate SQLite databases. This keeps portable creative data cleanly separated from venue-specific hardware configuration. Both databases are accessed through the same `SqlitePool` type, differentiated as `pool` (global) and `project_pool` (venue) throughout the codebase.

### Why Signals Are Flat Vec<f32>?

Cache-friendly memory layout. The 3D tensor could be represented as nested `Vec<Vec<Vec<f32>>>` but a flat buffer with computed indexing (`n_idx * (t * c) + t_idx * c + c_idx`) is faster for sequential access patterns in the graph engine. Most node operations iterate over the entire buffer linearly, making the flat layout optimal for CPU cache utilization. The broadcasting logic in binary operations simply computes index offsets with modulo wrapping rather than allocating expanded copies.

### Why Tags Instead of Direct Fixture References?

Patterns that reference fixtures by ID are not portable between venues. Tags create an indirection layer -- patterns reference abstract roles (`front`, `circular`, `has_movement`), and each venue maps those roles to its specific hardware through the group/tag assignment system. This is the core architectural decision enabling venue portability. A pattern designed in one venue with "front & has_color" will work in any venue that has groups tagged accordingly, regardless of the specific fixtures installed.

### Why Compositor Caching Is Multi-Level?

Graph execution is expensive (audio loading, FFT, ML inference). The three-level cache (layer, composite, incremental) means:

- **Editing one annotation** only re-executes that pattern's graph. All other annotations' cached layers are reused.
- **Moving an annotation** only recomposites the affected time ranges. The "dirty interval" system marks the old and new time ranges for recomputation.
- **Playing back with no edits** costs nearly zero -- the composite cache returns immediately if all annotation signatures match.

The layer cache uses an `AnnotationSignature` that includes a hash of the graph JSON, argument values, and metadata. The `matches_ignoring_seed` comparison excludes the stochastic `instance_seed` so that random patterns do not unnecessarily invalidate the cache when nothing else has changed.

### Why the "What/How" Split?

The separation between the global library (patterns, tracks) and venue projects (fixtures, groups, annotations) reflects the real-world workflow of lighting design:

- A lighting designer creates patterns once and reuses them across many venues
- Each venue has unique hardware that must be configured independently
- Annotations (when to play which pattern) are venue-specific because they depend on the fixture inventory
- Tracks and their analysis data (beats, stems, chords) are universal

This split also enables community pattern sharing: patterns can be published to Supabase and used by other designers without exposing venue-specific details.

---
title: Node Graph Migration Analysis
---

# Node Graph Module Migration

This document describes the refactoring of the node graph system from a monolithic `schema.rs` file (~5500 lines) into a modular structure under `src-tauri/src/node_graph/`.

## Overview

The original `schema.rs` contained all node graph functionality in a single file:

- Node type definitions
- Graph execution logic
- State management
- Audio/color/signal processing
- Context loading

This has been split into focused modules with clear responsibilities.

## New Module Structure

```
src-tauri/src/node_graph/
├── mod.rs                    # Module exports, NodeExecutionContext
├── context.rs                # Context loading, audio buffers, utilities
├── executor.rs               # Graph execution, topological sort, layer merging
├── state.rs                  # ExecutionState, timing, caches
└── nodes/
    ├── mod.rs                # Node dispatcher, constants, get_node_types()
    ├── analysis.rs           # harmony_analysis, mel_spec_viewer, view_signal, harmonic_tension
    ├── apply.rs              # apply_dimmer, apply_color, apply_strobe, apply_position, apply_speed
    ├── audio.rs              # audio_input, beat_clock, stem_splitter, frequency_amplitude, filters, beat_envelope
    ├── color.rs              # gradient, chroma_palette, spectral_shift, color
    ├── selection.rs          # select, get_attribute, random_select_mask
    └── signals.rs            # pattern_args, math, ramp, threshold, normalize, etc.
```

## Module Responsibilities

### `context.rs`

- `AudioBuffer` struct for audio sample storage with crop info
- `LoadedContext` struct encapsulating context loading results
- `needs_context()` - identifies nodes requiring context audio
- `parse_color_value()` / `parse_hex_color()` - color parsing utilities
- `crop_samples_to_range()` - audio sample cropping
- `beat_grid_relative_to_crop()` - beat grid offset calculation
- `load_context()` - loads context from DB with shared audio support

### `executor.rs`

- `run_graph()` - Tauri command wrapper with resource path resolution
- `run_graph_internal()` - main execution loop
- `SharedAudioContext` / `GraphExecutionConfig` structs
- `adsr_durations()` / `calc_envelope()` / `shape_curve()` - envelope utilities
- Topological sorting via petgraph
- Layer merge logic for Apply outputs
- Engine preview frame rendering

### `state.rs`

- `ExecutionState` - all runtime state during graph execution:
  - `audio_buffers`: HashMap<(String, String), AudioBuffer>
  - `beat_grids`: HashMap<(String, String), BeatGrid>
  - `selections`: HashMap<(String, String), Selection>
  - `signal_outputs`: HashMap<(String, String), Signal>
  - `apply_outputs`: Vec<LayerTimeSeries>
  - `color_outputs`: HashMap<(String, String), String>
  - `root_caches`: HashMap<i64, RootCache>
  - `view_results` / `mel_specs` / `color_views`: visualization outputs
  - `node_timings`: performance tracking
- `RootCache` / `NodeTiming` helper structs
- `record_timing()` helper method

### `nodes/mod.rs`

- `run_node()` dispatcher - routes to submodule handlers
- `get_node_types()` - aggregates NodeTypeDef from all submodules
- Constants: `PREVIEW_LENGTH = 256`, `SIMULATION_RATE = 60.0`, `CHROMA_DIM = 12`

Dispatch order: selection → audio → signals → color → apply → analysis

## Node Types by Category

| Category  | Nodes                                                                                                                                                                                                     | File           |
| --------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------- |
| Selection | `select`, `get_attribute`, `random_select_mask`                                                                                                                                                           | `selection.rs` |
| Audio     | `audio_input`, `beat_clock`, `stem_splitter`, `frequency_amplitude`, `lowpass_filter`, `highpass_filter`, `beat_envelope`                                                                                 | `audio.rs`     |
| Signals   | `pattern_args`, `math`, `round`, `ramp`, `ramp_between`, `threshold`, `normalize`, `falloff`, `invert`, `sine_wave`, `remap`, `smooth_movement`, `look_at_position`, `orbit`, `random_position`, `scalar` | `signals.rs`   |
| Color     | `gradient`, `chroma_palette`, `spectral_shift`, `color`                                                                                                                                                   | `color.rs`     |
| Apply     | `apply_dimmer`, `apply_color`, `apply_strobe`, `apply_position`, `apply_speed`                                                                                                                            | `apply.rs`     |
| Analysis  | `harmony_analysis`, `mel_spec_viewer`, `view_signal`, `harmonic_tension`                                                                                                                                  | `analysis.rs`  |

**Total: 39 node types**

## Fixes Applied During Migration

### 1. `pattern_args` scope fix (`signals.rs`)

The `pattern_args` node incorrectly referenced `arg_defs` and `arg_values` without the `ctx.` prefix.

```rust
// Before (broken)
for arg in arg_defs {
    let value = arg_values.get(&arg.id)...

// After (fixed)
for arg in ctx.arg_defs {
    let value = ctx.arg_values.get(&arg.id)...
```

### 2. Removed duplicate filter implementations

`lowpass_filter` and `highpass_filter` were duplicated in both `audio.rs` and `color.rs`. Removed the `color.rs` versions since `audio.rs` handles them first in dispatch order.

### 3. Moved misplaced nodes to `analysis.rs`

- `view_signal` (View category) - was in `color.rs`
- `harmonic_tension` (Math category) - was in `color.rs`

Both moved to `analysis.rs` where they logically belong.

## Architectural Improvements

- **Separation of concerns**: Node types organized by category
- **`NodeExecutionContext`**: Clean struct passing all execution context to nodes
- **`ExecutionState`**: Centralized state with helper methods
- **`LoadedContext`**: Encapsulates context loading results
- **Testability**: Smaller modules are easier to test in isolation

## Test Coverage Recommendations

- `pattern_args` node with various arg types (Color, Scalar)
- All 39 node types execute correctly
- Node dispatch order and handling
- Filter nodes (lowpass/highpass) work correctly
- Layer merging with multiple Apply nodes

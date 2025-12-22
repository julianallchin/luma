---
title: Compositing Buffer Design
---

# Luma Compositing Buffer Design (Rust Implementation)

## Core Concept

The compositing buffer is **time-series based**, like Photoshop's timeline or Premiere Pro. Pattern graph execution produces **time series** for each capability, then the compositor blends these layers to produce the final DMX output.

**All graph execution, compositing, and DMX rendering happens in Rust** (`src-tauri/src/`).

### Two Levels of Combination

**IMPORTANT:** There are two distinct operations for combining outputs:

1. **Within a Pattern - Merge (No Blending)**
   - Multiple Apply nodes in a single pattern → merged into one `LayerTimeSeries`
   - Simple union of capabilities (color from one Apply, dimmer from another)
   - Conflict if same capability applied twice to same primitive (ERROR)
   - No blend modes, no z-index - just merge

2. **Between Patterns - Composite (With Blending)**
   - Multiple patterns (each producing one `LayerTimeSeries`) → composited into `CompositeBuffer`
   - Z-index ordering (bottom to top)
   - Blend modes (Add, Multiply, Screen, etc.)
   - Time-based layering on the track timeline

**Example:**
```rust
// WITHIN PATTERN: Merge Apply nodes
Pattern "Ceiling Show" {
    Select("Ceiling") → Apply_Color(red)  \
                     → Apply_Dimmer(0.5) → MERGE → LayerTimeSeries
}

// BETWEEN PATTERNS: Composite on track
Track {
    Layer 0: Pattern "Ceiling Show"    (z=0, blend=Replace)
    Layer 1: Pattern "Floor Accents"   (z=1, blend=Add)      → COMPOSITE → CompositeBuffer
    Layer 2: Pattern "Strobe Burst"    (z=2, blend=Screen)
}
```

This document focuses on **Level 2 (Between Patterns)**. For within-pattern merging logic, see `unified-selection-and-capability-application.md`.

---

## Current Rust Architecture

### Existing Types (`src-tauri/src/models/schema.rs`)

```rust
pub enum PortType {
    Intensity,
    Audio,
    BeatGrid,
    Series,    // Already exists for time-series data!
    Color,     // Already exists!
}

pub struct Series {
    pub dim: usize,
    pub labels: Option<Vec<String>>,
    pub samples: Vec<SeriesSample>,
}

pub struct SeriesSample {
    pub time: f32,
    pub values: Vec<f32>,
    pub label: Option<String>,
}

pub struct GraphContext {
    pub track_id: i64,
    pub start_time: f32,
    pub end_time: f32,
    pub beat_grid: Option<BeatGrid>,
}
```

### Graph Execution (`src-tauri/src/schema.rs`)

Already implemented:
- `run_graph()` - executes pattern graphs using `petgraph` topological sort
- Audio processing nodes
- Beat grid support
- Series output for time-series data
- Context-based execution (track_id, start/end time)

---

## Proposed Extension: Compositing Layer

### 1. Primitive State (Per-Layer Output)

Each pattern execution outputs primitive states over time:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Color {
    pub r: f32,  // 0-1
    pub g: f32,
    pub b: f32,
    pub w: Option<f32>,  // For RGBW fixtures
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Position {
    pub pan: Option<f32>,   // -1 to 1 (normalized)
    pub tilt: Option<f32>,  // -1 to 1
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StrobeState {
    pub enabled: bool,
    pub rate: f32,  // Hz
}

// Use existing Series type for time-series capabilities
pub struct PrimitiveTimeSeries {
    pub primitive_id: String,

    // Each capability is a time series (using existing Series type)
    pub color: Option<Series>,      // dim=3 (RGB) or 4 (RGBW)
    pub dimmer: Option<Series>,     // dim=1 (brightness 0-1)
    pub position: Option<Series>,   // dim=2 (pan, tilt)
    pub strobe: Option<Series>,     // dim=2 (enabled, rate)
    pub zoom: Option<Series>,       // dim=1
    pub focus: Option<Series>,      // dim=1
}

pub struct LayerTimeSeries {
    // Metadata (set from annotation when placed on track)
    pub pattern_id: i64,
    pub z_index: i32,
    pub blend_mode: BlendMode,
    pub start_time: f32,
    pub end_time: f32,

    // Output from merged Apply nodes
    pub primitives: Vec<PrimitiveTimeSeries>,
}
```

### 2. Blend Modes

```rust
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum BlendMode {
    Replace,    // Top layer overwrites bottom
    Add,        // Add values (clamped to 1.0)
    Multiply,   // Multiply values (darkening)
    Screen,     // Inverse multiply (lightening)
    Max,        // Take maximum
    Min,        // Take minimum
}

fn blend_scalar(bottom: Option<f32>, top: Option<f32>, mode: BlendMode) -> Option<f32> {
    match (bottom, top) {
        (None, top) => top,
        (bottom, None) => bottom,
        (Some(b), Some(t)) => Some(match mode {
            BlendMode::Replace => t,
            BlendMode::Add => (b + t).min(1.0),
            BlendMode::Multiply => b * t,
            BlendMode::Screen => 1.0 - (1.0 - b) * (1.0 - t),
            BlendMode::Max => b.max(t),
            BlendMode::Min => b.min(t),
        }),
    }
}

fn blend_color(bottom: Option<&Color>, top: Option<&Color>, mode: BlendMode) -> Option<Color> {
    match (bottom, top) {
        (None, Some(t)) => Some(t.clone()),
        (Some(b), None) => Some(b.clone()),
        (Some(b), Some(t)) => Some(Color {
            r: blend_scalar(Some(b.r), Some(t.r), mode).unwrap_or(0.0),
            g: blend_scalar(Some(b.g), Some(t.g), mode).unwrap_or(0.0),
            b: blend_scalar(Some(b.b), Some(t.b), mode).unwrap_or(0.0),
            w: blend_scalar(b.w, t.w, mode),
        }),
        (None, None) => None,
    }
}
```

### 3. Transition Curves

Layers can optionally include an opacity curve (or "transition curve") that modulates the intensity of the layer over time. This allows for smooth fade-ins, fade-outs, and crossfades between patterns.

```rust
pub struct LayerTimeSeries {
    // ... existing fields ...
    pub z_index: i32,
    pub blend_mode: BlendMode,
    
    // NEW: Opacity curve for the layer (0.0 to 1.0)
    // If None, opacity is 1.0
    pub opacity: Option<Series>, 
    
    // ...
}
```

The compositing algorithm would multiply the layer's values by this opacity *before* blending with the underlying layers.

### 4. Composite Buffer

```rust
pub struct CompositeBuffer {
    pub start_time: f32,
    pub end_time: f32,
    pub sample_rate: f32,  // Hz (e.g., 60 for 60fps)

    // Final composited time series for each primitive
    pub primitives: HashMap<String, PrimitiveTimeSeries>,
}
```

### 4. Compositing Algorithm

```rust
pub fn composite_layers(
    layers: &[LayerTimeSeries],
    start_time: f32,
    end_time: f32,
    sample_rate: f32,
) -> Result<CompositeBuffer, String> {

    // Sort layers by z-index (bottom to top)
    let mut sorted_layers = layers.to_vec();
    sorted_layers.sort_by_key(|layer| layer.z_index);

    // Collect all primitive IDs across all layers
    let all_primitive_ids: HashSet<String> = sorted_layers
        .iter()
        .flat_map(|layer| layer.primitives.iter().map(|p| p.primitive_id.clone()))
        .collect();

    let num_samples = ((end_time - start_time) * sample_rate).ceil() as usize;
    let mut composite_primitives = HashMap::new();

    for primitive_id in all_primitive_ids {
        // Initialize empty time series for this primitive
        let mut color_samples: Vec<SeriesSample> = Vec::new();
        let mut dimmer_samples: Vec<SeriesSample> = Vec::new();
        let mut position_samples: Vec<SeriesSample> = Vec::new();

        // For each time point
        for i in 0..num_samples {
            let t = start_time + (i as f32 / sample_rate);

            let mut color_value: Option<Color> = None;
            let mut dimmer_value: Option<f32> = None;
            let mut position_value: Option<Position> = None;

            // Blend layers bottom-to-top
            for layer in &sorted_layers {
                if t < layer.start_time || t > layer.end_time {
                    continue;
                }

                let primitive = layer.primitives
                    .iter()
                    .find(|p| p.primitive_id == primitive_id);

                if let Some(prim) = primitive {
                    // Sample this layer's time series at time t
                    let layer_color = sample_series_as_color(&prim.color, t);
                    let layer_dimmer = sample_series_scalar(&prim.dimmer, t);
                    let layer_position = sample_series_as_position(&prim.position, t);

                    // Blend with accumulated values
                    color_value = blend_color(color_value.as_ref(), layer_color.as_ref(), layer.blend_mode);
                    dimmer_value = blend_scalar(dimmer_value, layer_dimmer, layer.blend_mode);
                    position_value = blend_position(position_value, layer_position, layer.blend_mode);
                }
            }

            // Store composited values at this time point
            if let Some(color) = color_value {
                color_samples.push(SeriesSample {
                    time: t,
                    values: vec![color.r, color.g, color.b],
                    label: None,
                });
            }

            if let Some(dimmer) = dimmer_value {
                dimmer_samples.push(SeriesSample {
                    time: t,
                    values: vec![dimmer],
                    label: None,
                });
            }

            if let Some(pos) = position_value {
                position_samples.push(SeriesSample {
                    time: t,
                    values: vec![pos.pan.unwrap_or(0.0), pos.tilt.unwrap_or(0.0)],
                    label: None,
                });
            }
        }

        composite_primitives.insert(
            primitive_id.clone(),
            PrimitiveTimeSeries {
                primitive_id,
                color: if !color_samples.is_empty() {
                    Some(Series { dim: 3, labels: None, samples: color_samples })
                } else {
                    None
                },
                dimmer: if !dimmer_samples.is_empty() {
                    Some(Series { dim: 1, labels: None, samples: dimmer_samples })
                } else {
                    None
                },
                position: if !position_samples.is_empty() {
                    Some(Series { dim: 2, labels: None, samples: position_samples })
                } else {
                    None
                },
                strobe: None,
                zoom: None,
                focus: None,
            },
        );
    }

    Ok(CompositeBuffer {
        start_time,
        end_time,
        sample_rate,
        primitives: composite_primitives,
    })
}

// Helper to sample Series at a specific time
fn sample_series_scalar(series: &Option<Series>, t: f32) -> Option<f32> {
    series.as_ref().and_then(|s| {
        // Find nearest sample by time
        s.samples
            .iter()
            .min_by(|a, b| {
                (a.time - t).abs().partial_cmp(&(b.time - t).abs()).unwrap()
            })
            .and_then(|sample| sample.values.first().copied())
    })
}

fn sample_series_as_color(series: &Option<Series>, t: f32) -> Option<Color> {
    series.as_ref().and_then(|s| {
        s.samples
            .iter()
            .min_by(|a, b| {
                (a.time - t).abs().partial_cmp(&(b.time - t).abs()).unwrap()
            })
            .map(|sample| Color {
                r: *sample.values.get(0).unwrap_or(&0.0),
                g: *sample.values.get(1).unwrap_or(&0.0),
                b: *sample.values.get(2).unwrap_or(&0.0),
                w: sample.values.get(3).copied(),
            })
    })
}

fn sample_series_as_position(series: &Option<Series>, t: f32) -> Option<Position> {
    series.as_ref().and_then(|s| {
        s.samples
            .iter()
            .min_by(|a, b| {
                (a.time - t).abs().partial_cmp(&(b.time - t).abs()).unwrap()
            })
            .map(|sample| Position {
                pan: sample.values.get(0).copied(),
                tilt: sample.values.get(1).copied(),
            })
    })
}
```

---

## DMX Rendering (QLC+ Integration)

At playback time, sample the composite buffer and render to DMX:

```rust
// Fixture capability lookup (inspired by QLC+)
pub struct FixtureCapabilities {
    pub rgb_channels: Option<[usize; 3]>,   // [R, G, B] DMX channels
    pub rgbw_channels: Option<[usize; 4]>,  // [R, G, B, W]
    pub dimmer_channel: Option<usize>,
    pub pan_channel: Option<usize>,
    pub tilt_channel: Option<usize>,
    pub strobe_channel: Option<usize>,
}

pub fn render_dmx_frame(
    buffer: &CompositeBuffer,
    current_time: f32,
    fixtures: &HashMap<String, FixtureCapabilities>,
) -> [u8; 512] {  // One DMX universe

    let mut dmx = [0u8; 512];

    for (primitive_id, time_series) in &buffer.primitives {
        let fixture = match fixtures.get(primitive_id) {
            Some(f) => f,
            None => continue,  // No fixture for this primitive
        };

        // Sample time series at current playback time
        let color = sample_series_as_color(&time_series.color, current_time);
        let dimmer = sample_series_scalar(&time_series.dimmer, current_time);
        let position = sample_series_as_position(&time_series.position, current_time);

        // Map to DMX channels (capability lookup)
        if let (Some(color), Some(rgb_ch)) = (color, fixture.rgb_channels) {
            dmx[rgb_ch[0]] = (color.r * 255.0) as u8;
            dmx[rgb_ch[1]] = (color.g * 255.0) as u8;
            dmx[rgb_ch[2]] = (color.b * 255.0) as u8;
        }

        if let (Some(color), Some(rgbw_ch)) = (color, fixture.rgbw_channels) {
            dmx[rgbw_ch[0]] = (color.r * 255.0) as u8;
            dmx[rgbw_ch[1]] = (color.g * 255.0) as u8;
            dmx[rgbw_ch[2]] = (color.b * 255.0) as u8;
            if let Some(w) = color.w {
                dmx[rgbw_ch[3]] = (w * 255.0) as u8;
            }
        }

        if let (Some(dimmer), Some(ch)) = (dimmer, fixture.dimmer_channel) {
            dmx[ch] = (dimmer * 255.0) as u8;
        }

        if let (Some(pos), Some(pan_ch), Some(tilt_ch)) =
            (position, fixture.pan_channel, fixture.tilt_channel) {

            if let Some(pan) = pos.pan {
                // Map -1..1 to 0..255
                dmx[pan_ch] = ((pan + 1.0) / 2.0 * 255.0) as u8;
            }
            if let Some(tilt) = pos.tilt {
                dmx[tilt_ch] = ((tilt + 1.0) / 2.0 * 255.0) as u8;
            }
        }
    }

    dmx
}
```

---

## Timeline Structure (Database)

Patterns are placed on the timeline as annotations:

```sql
-- Global DB (luma.db)
CREATE TABLE patterns (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- Project DB (venue.luma)
CREATE TABLE implementations (
    pattern_id INTEGER PRIMARY KEY,
    graph_json TEXT NOT NULL,  -- Serialized Graph
    updated_at DATETIME
);

CREATE TABLE annotations (
    id INTEGER PRIMARY KEY,
    track_id INTEGER,
    pattern_id INTEGER,
    start_beat REAL,
    end_beat REAL,
    z_index INTEGER DEFAULT 0,
    blend_mode TEXT DEFAULT 'replace',
    FOREIGN KEY (track_id) REFERENCES tracks(id),
    FOREIGN KEY (pattern_id) REFERENCES patterns(id)
);
```

---

## Example: Three Overlapping Patterns

### Annotation Data (from database):

```rust
let annotations = vec![
    Annotation {
        pattern_id: 1,  // "blue-wash"
        start_beat: 0.0,
        end_beat: 64.0,
        z_index: 1,
        blend_mode: BlendMode::Replace,
    },
    Annotation {
        pattern_id: 2,  // "beat-pulse"
        start_beat: 16.0,
        end_beat: 48.0,
        z_index: 2,
        blend_mode: BlendMode::Multiply,
    },
    Annotation {
        pattern_id: 3,  // "strobe-hit"
        start_beat: 32.0,
        end_beat: 33.0,
        z_index: 3,
        blend_mode: BlendMode::Add,
    },
];
```

### Graph Execution:

For each annotation, execute the pattern graph:

```rust
// 1. Load pattern graph from database
let graph_json = load_pattern_graph(annotation.pattern_id)?;
let graph: Graph = serde_json::from_str(&graph_json)?;

// 2. Execute graph with context - returns merged LayerTimeSeries
let context = GraphContext {
    track_id,
    start_time: beat_to_time(annotation.start_beat, &beat_grid),
    end_time: beat_to_time(annotation.end_beat, &beat_grid),
    beat_grid: Some(beat_grid.clone()),
};

// execute_pattern_graph() internally:
// - Runs the graph
// - Collects all Apply node outputs
// - Merges them with conflict detection
let mut layer = execute_pattern_graph(db, &graph, context, &fixtures).await?;

// 3. Add annotation metadata for compositing
layer.pattern_id = annotation.pattern_id;
layer.z_index = annotation.z_index;
layer.blend_mode = annotation.blend_mode;

layers.push(layer);
```

### Compositing:

```rust
let composite = composite_layers(&layers, 0.0, 60.0, 60.0)?;
```

### Playback:

```rust
// At playback time t = 20.5 seconds
let dmx_frame = render_dmx_frame(&composite, 20.5, &fixtures);
// Send dmx_frame to DMX interface
```

---

## Integration Points

### 1. Add Graph Node Types

Add output nodes to pattern graphs:

```rust
// In get_node_types()
NodeTypeDef {
    id: "apply_color".into(),
    name: "Apply Color".into(),
    description: Some("Apply color to selected primitives".into()),
    category: Some("Output".into()),
    inputs: vec![
        PortDef {
            id: "color_in".into(),
            name: "Color".into(),
            port_type: PortType::Color,
        }
    ],
    outputs: vec![],
    params: vec![
        ParamDef {
            id: "primitives".into(),
            name: "Primitive IDs (comma-separated)".into(),
            param_type: ParamType::Text,
            default_text: Some("fixture-1-head-0,fixture-2-head-0".into()),
        }
    ],
}
```

### 2. Extend `run_graph()` to Return Merged Apply Outputs

Graph execution should:
1. Execute all nodes in topological order
2. Collect outputs from all Apply nodes
3. Merge them with conflict detection (see `unified-selection-and-capability-application.md`)
4. Return the merged `LayerTimeSeries`

```rust
pub struct RunResult {
    // Existing preview data
    pub output_buffers: HashMap<(String, String), Vec<f32>>,
    pub series_views: HashMap<String, Series>,

    // NEW: Merged primitive outputs from all Apply nodes
    pub layer_output: LayerTimeSeries,
}

// Or more simply, have run_graph() return the LayerTimeSeries directly:
pub fn execute_pattern_graph(
    db: &Db,
    graph: &Graph,
    context: GraphContext,
    fixtures: &HashMap<String, Fixture>,
) -> Result<LayerTimeSeries, String> {
    // 1. Execute graph nodes
    let node_outputs = run_graph(db, graph, Some(context)).await?;

    // 2. Collect Apply node outputs
    let apply_outputs: Vec<LayerTimeSeries> = node_outputs
        .iter()
        .filter(|(node_id, _)| {
            graph.nodes.iter()
                .find(|n| n.id == *node_id)
                .map(|n| n.type_id.starts_with("apply_"))
                .unwrap_or(false)
        })
        .map(|(_, output)| output.clone())
        .collect();

    // 3. Merge with conflict detection
    merge_apply_outputs(apply_outputs)
}
```

### 3. Add Compositing Command

```rust
#[tauri::command]
pub async fn render_composite(
    db: State<'_, Db>,
    project_db: State<'_, ProjectDb>,
    track_id: i64,
    start_time: f32,
    end_time: f32,
) -> Result<CompositeBuffer, String> {
    // Load annotations for this track in time range
    // Execute each pattern graph
    // Composite layers
    // Return final buffer
}
```

---

## Key Takeaways

1. **Two-level combination system:**
   - **Within pattern:** Multiple Apply nodes merge (no blending, conflict detection)
   - **Between patterns:** Layers composite with z-index and blend modes
2. **Reuse existing Series type** for time-series data (already in schema.rs)
3. **All logic in Rust** - graph execution, merging, compositing, DMX rendering
4. **Pattern graphs output LayerTimeSeries** containing merged Apply node outputs
5. **Compositor blends LayerTimeSeries** based on z-index and blend mode
6. **DMX renderer samples at playback time** and maps to channels via fixture capabilities
7. **Annotations in database** define pattern placement, z-index, blend mode

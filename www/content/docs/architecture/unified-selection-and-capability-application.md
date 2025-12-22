---
title: Unified Selection And Capability Application
---

# Unified Selection and Capability Application

## Overview

This document describes how Luma's node graph handles fixture and primitive selection with capability-based application. The system allows selecting either entire fixtures OR individual heads (primitives) using the same representation, then applying capabilities like dimmer, color, position intelligently based on what's available.

---

## Core Design

### Selectable Enum

Both fixtures and primitives are represented as `Selectable`:

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Selectable {
    Fixture {
        fixture_id: String
    },
    Primitive {
        fixture_id: String,
        head_index: usize
    },
}
```

### Selection Type

Select nodes output lists of selectables:

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Selection {
    pub items: Vec<Selectable>,
}
```

This becomes a new `PortType::Selection` that flows through the graph.

---

## Node Types

### Select Node

Outputs a `Selection` containing fixtures or primitives based on semantic queries.

**Example: Selecting an entire fixture**
```rust
Selection {
    items: vec![
        Selectable::Fixture { fixture_id: "venue-tetra-bar-1".to_string() }
    ]
}
```

**Example: Selecting specific primitives**
```rust
Selection {
    items: vec![
        Selectable::Primitive { fixture_id: "venue-tetra-bar-1".to_string(), head_index: 0 },
        Selectable::Primitive { fixture_id: "venue-tetra-bar-1".to_string(), head_index: 2 },
        Selectable::Primitive { fixture_id: "clay-paky-k10-1".to_string(), head_index: 5 },
    ]
}
```

**Example: Mixed selection (whole fixtures + individual primitives)**
```rust
Selection {
    items: vec![
        Selectable::Fixture { fixture_id: "moving-head-1".to_string() },
        Selectable::Primitive { fixture_id: "tetra-bar-2".to_string(), head_index: 1 },
        Selectable::Primitive { fixture_id: "tetra-bar-2".to_string(), head_index: 3 },
    ]
}
```

### Apply Nodes

Apply nodes take a `Selection` and capability data, returning a `LayerTimeSeries` for compositing.

**Node types:**
- `apply_dimmer` (Intensity capability)
- `apply_color` (Colour capability)
- `apply_position` (Pan/Tilt capabilities)
- `apply_strobe` (Shutter capability)
- `apply_zoom` (Beam/Zoom capability)
- `apply_focus` (Beam/Focus capability)
- etc.

**Inputs:**
- `selection: Selection` - What to apply to
- `data: Series` - Time-series capability data

**Output:**
- `LayerTimeSeries` - Time-series output for compositing

---

## Capability Application Logic

### Basic Algorithm

For each `Selectable` in the selection:

1. **If Selectable::Fixture**:
   - Try to find fixture-wide capability (e.g., global master dimmer)
   - If found, apply data to that channel
   - If not found, expand to all heads and apply to each head that has the capability

2. **If Selectable::Primitive**:
   - Try to find head-specific capability
   - If found, apply data to that channel
   - If not found, fall back to fixture-wide capability
   - If neither exists, skip (graceful degradation)

### Example: apply_dimmer

```rust
pub fn apply_dimmer(
    selection: Selection,
    dimmer_data: Series,
    fixtures: &HashMap<String, Fixture>,
) -> LayerTimeSeries {
    let mut layer = LayerTimeSeries {
        primitives: Vec::new(),
    };

    for selectable in selection.items {
        match selectable {
            Selectable::Fixture { fixture_id } => {
                let fixture = fixtures.get(&fixture_id).unwrap();

                // Try global master dimmer first
                if let Some(master_ch) = fixture.master_intensity_channel() {
                    // Apply to global dimmer (affects all heads)
                    layer.primitives.push(PrimitiveTimeSeries {
                        primitive_id: format!("{}:global", fixture_id),
                        dimmer: Some(dimmer_data.clone()),
                        ..Default::default()
                    });
                } else {
                    // No global dimmer, apply to all heads individually
                    for head_idx in 0..fixture.heads().len() {
                        if let Some(_) = fixture.channel_number(QLCChannel::Intensity, MSB, head_idx) {
                            layer.primitives.push(PrimitiveTimeSeries {
                                primitive_id: format!("{}:{}", fixture_id, head_idx),
                                dimmer: Some(dimmer_data.clone()),
                                ..Default::default()
                            });
                        }
                    }
                }
            }

            Selectable::Primitive { fixture_id, head_index } => {
                let fixture = fixtures.get(&fixture_id).unwrap();

                // Try head-specific dimmer
                if let Some(_) = fixture.channel_number(QLCChannel::Intensity, MSB, head_index) {
                    layer.primitives.push(PrimitiveTimeSeries {
                        primitive_id: format!("{}:{}", fixture_id, head_index),
                        dimmer: Some(dimmer_data.clone()),
                        ..Default::default()
                    });
                }
                // Fall back to fixture-wide dimmer
                else if let Some(_) = fixture.master_intensity_channel() {
                    layer.primitives.push(PrimitiveTimeSeries {
                        primitive_id: format!("{}:global", fixture_id),
                        dimmer: Some(dimmer_data.clone()),
                        ..Default::default()
                    });
                }
                // Neither exists, skip gracefully
            }
        }
    }

    layer
}
```

### Example: apply_color

```rust
pub fn apply_color(
    selection: Selection,
    color_data: Series, // dim=3 (RGB) or dim=4 (RGBW)
    fixtures: &HashMap<String, Fixture>,
) -> LayerTimeSeries {
    let mut layer = LayerTimeSeries {
        primitives: Vec::new(),
    };

    for selectable in selection.items {
        match selectable {
            Selectable::Fixture { fixture_id } => {
                let fixture = fixtures.get(&fixture_id).unwrap();

                // Check if fixture has global RGB (rare)
                // Usually RGB is head-specific, so expand to all heads
                for head_idx in 0..fixture.heads().len() {
                    let rgb_channels = fixture.rgb_channels(head_idx);
                    if !rgb_channels.is_empty() {
                        layer.primitives.push(PrimitiveTimeSeries {
                            primitive_id: format!("{}:{}", fixture_id, head_idx),
                            color: Some(color_data.clone()),
                            ..Default::default()
                        });
                    }
                }
            }

            Selectable::Primitive { fixture_id, head_index } => {
                let fixture = fixtures.get(&fixture_id).unwrap();
                let rgb_channels = fixture.rgb_channels(head_index);

                if !rgb_channels.is_empty() {
                    layer.primitives.push(PrimitiveTimeSeries {
                        primitive_id: format!("{}:{}", fixture_id, head_index),
                        color: Some(color_data.clone()),
                        ..Default::default()
                    });
                }
                // No RGB on this head, skip gracefully
            }
        }
    }

    layer
}
```

### Example: apply_position

```rust
pub fn apply_position(
    selection: Selection,
    position_data: Series, // dim=2 (pan, tilt)
    fixtures: &HashMap<String, Fixture>,
) -> LayerTimeSeries {
    let mut layer = LayerTimeSeries {
        primitives: Vec::new(),
    };

    for selectable in selection.items {
        match selectable {
            Selectable::Fixture { fixture_id } => {
                let fixture = fixtures.get(&fixture_id).unwrap();

                // Check if fixture has global pan/tilt (fixture-wide movement)
                // This would be channels NOT inside any <Head> tag
                if let Some(_) = fixture.global_channel_number(QLCChannel::Pan, MSB) {
                    // Fixture moves as a whole
                    layer.primitives.push(PrimitiveTimeSeries {
                        primitive_id: format!("{}:global", fixture_id),
                        position: Some(position_data.clone()),
                        ..Default::default()
                    });
                } else {
                    // Check if individual heads have pan/tilt
                    for head_idx in 0..fixture.heads().len() {
                        if let Some(_) = fixture.channel_number(QLCChannel::Pan, MSB, head_idx) {
                            layer.primitives.push(PrimitiveTimeSeries {
                                primitive_id: format!("{}:{}", fixture_id, head_idx),
                                position: Some(position_data.clone()),
                                ..Default::default()
                            });
                        }
                    }
                }
            }

            Selectable::Primitive { fixture_id, head_index } => {
                let fixture = fixtures.get(&fixture_id).unwrap();

                // Try head-specific pan/tilt
                if let Some(_) = fixture.channel_number(QLCChannel::Pan, MSB, head_index) {
                    layer.primitives.push(PrimitiveTimeSeries {
                        primitive_id: format!("{}:{}", fixture_id, head_index),
                        position: Some(position_data.clone()),
                        ..Default::default()
                    });
                }
                // Fall back to fixture-wide pan/tilt
                else if let Some(_) = fixture.global_channel_number(QLCChannel::Pan, MSB) {
                    layer.primitives.push(PrimitiveTimeSeries {
                        primitive_id: format!("{}:global", fixture_id),
                        position: Some(position_data.clone()),
                        ..Default::default()
                    });
                }
                // No position capability, skip
            }
        }
    }

    layer
}
```

---

## Real-World Examples

### Example 1: Venue Tetra Bar (4 independent RGB heads)

**Fixture structure:**
- No global dimmer
- 4 heads, each with: R, G, B, Amber, Dimmer, Strobe

**Scenario: Select entire fixture, apply dimmer**
```rust
Selection {
    items: vec![Selectable::Fixture { fixture_id: "tetra-1" }]
}
```

Result: Dimmer applied to all 4 heads individually (channels 4, 10, 16, 22).

**Scenario: Select heads 0 and 2, apply color**
```rust
Selection {
    items: vec![
        Selectable::Primitive { fixture_id: "tetra-1", head_index: 0 },
        Selectable::Primitive { fixture_id: "tetra-1", head_index: 2 },
    ]
}
```

Result: Color applied to head 0 (R=0, G=1, B=2) and head 2 (R=12, G=13, B=14).

### Example 2: Clay Paky B-EYE K10 (moving head with 37 LED pixels)

**Fixture structure:**
- Global pan/tilt (channels 13, 15) - NOT in any head
- Global dimmer (channel 1)
- 37 heads with individual RGBW

**Scenario: Select entire fixture, apply position**
```rust
Selection {
    items: vec![Selectable::Fixture { fixture_id: "k10-1" }]
}
```

Result: Position applied to global pan/tilt (channels 13, 15) - moves entire fixture.

**Scenario: Select head 5, apply color**
```rust
Selection {
    items: vec![
        Selectable::Primitive { fixture_id: "k10-1", head_index: 5 }
    ]
}
```

Result: Color applied to head 5's RGBW channels only.

**Scenario: Select head 5, apply dimmer**
```rust
Selection {
    items: vec![
        Selectable::Primitive { fixture_id: "k10-1", head_index: 5 }
    ]
}
```

Result: Head 5 doesn't have individual dimmer, falls back to global dimmer (channel 1). Global dimmer affects all pixels.

### Example 3: Simple Moving Head (single head, global dimmer)

**Fixture structure:**
- Global pan/tilt
- Global dimmer
- Single head with RGB

**Scenario: Select fixture, apply dimmer**
```rust
Selection {
    items: vec![Selectable::Fixture { fixture_id: "moving-head-1" }]
}
```

Result: Dimmer applied to global dimmer channel.

**Scenario: Select head 0, apply dimmer**
```rust
Selection {
    items: vec![
        Selectable::Primitive { fixture_id: "moving-head-1", head_index: 0 }
    ]
}
```

Result: Head 0 has no individual dimmer, falls back to global dimmer.

---

## Merging Multiple Apply Nodes Within a Pattern

A single pattern graph can have multiple Apply nodes outputting to different capabilities. These outputs must be **merged** (not blended) into a single `LayerTimeSeries`.

**Key principle:** Blending only happens between patterns at the track layer level. Within a pattern, Apply nodes simply merge their capabilities.

### Merge Algorithm

```rust
pub fn merge_apply_outputs(
    apply_outputs: Vec<LayerTimeSeries>
) -> Result<LayerTimeSeries, String> {
    let mut merged_primitives: HashMap<String, PrimitiveTimeSeries> = HashMap::new();

    for layer in apply_outputs {
        for primitive in layer.primitives {
            let entry = merged_primitives
                .entry(primitive.primitive_id.clone())
                .or_insert_with(|| PrimitiveTimeSeries {
                    primitive_id: primitive.primitive_id.clone(),
                    color: None,
                    dimmer: None,
                    position: None,
                    strobe: None,
                    zoom: None,
                    focus: None,
                });

            // Conflict detection - same primitive, same capability = ERROR
            if primitive.color.is_some() && entry.color.is_some() {
                return Err(format!(
                    "Conflict: 'color' applied multiple times to primitive '{}'",
                    primitive.primitive_id
                ));
            }
            if primitive.dimmer.is_some() && entry.dimmer.is_some() {
                return Err(format!(
                    "Conflict: 'dimmer' applied multiple times to primitive '{}'",
                    primitive.primitive_id
                ));
            }
            if primitive.position.is_some() && entry.position.is_some() {
                return Err(format!(
                    "Conflict: 'position' applied multiple times to primitive '{}'",
                    primitive.primitive_id
                ));
            }
            if primitive.strobe.is_some() && entry.strobe.is_some() {
                return Err(format!(
                    "Conflict: 'strobe' applied multiple times to primitive '{}'",
                    primitive.primitive_id
                ));
            }
            if primitive.zoom.is_some() && entry.zoom.is_some() {
                return Err(format!(
                    "Conflict: 'zoom' applied multiple times to primitive '{}'",
                    primitive.primitive_id
                ));
            }
            if primitive.focus.is_some() && entry.focus.is_some() {
                return Err(format!(
                    "Conflict: 'focus' applied multiple times to primitive '{}'",
                    primitive.primitive_id
                ));
            }

            // Merge capabilities (union)
            if primitive.color.is_some() { entry.color = primitive.color; }
            if primitive.dimmer.is_some() { entry.dimmer = primitive.dimmer; }
            if primitive.position.is_some() { entry.position = primitive.position; }
            if primitive.strobe.is_some() { entry.strobe = primitive.strobe; }
            if primitive.zoom.is_some() { entry.zoom = primitive.zoom; }
            if primitive.focus.is_some() { entry.focus = primitive.focus; }
        }
    }

    Ok(LayerTimeSeries {
        primitives: merged_primitives.into_values().collect(),
    })
}
```

### Valid Pattern Examples

**✅ Different selectables, same capability:**
```rust
// Pattern graph:
Select("The Ceiling") → Apply_Color(red)
Select("The Floor") → Apply_Color(blue)

// Merged output:
LayerTimeSeries {
    primitives: vec![
        PrimitiveTimeSeries {
            primitive_id: "ceiling-fixture-1:0",
            color: Some(red_series),  // from first Apply
            ..
        },
        PrimitiveTimeSeries {
            primitive_id: "floor-fixture-1:0",
            color: Some(blue_series),  // from second Apply
            ..
        },
    ]
}
// ✅ OK - different primitives
```

**✅ Same selectable, different capabilities:**
```rust
// Pattern graph:
Select("The Ceiling") → Apply_Color(red)
                     ↘ Apply_Dimmer(0.5)
                     ↘ Apply_Strobe(flash)

// Merged output:
LayerTimeSeries {
    primitives: vec![
        PrimitiveTimeSeries {
            primitive_id: "ceiling-fixture-1:0",
            color: Some(red_series),      // from Apply_Color
            dimmer: Some(dimmer_series),  // from Apply_Dimmer
            strobe: Some(strobe_series),  // from Apply_Strobe
            ..
        },
    ]
}
// ✅ OK - same primitive, different capabilities
```

### Invalid Pattern Examples

**❌ Same selectable, same capability:**
```rust
// Pattern graph:
Select("The Ceiling") → Apply_Color(red)
Select("The Ceiling") → Apply_Color(blue)  // Both selections overlap!

// Merge attempt on primitive "ceiling-fixture-1:0":
// color: Some(red_series) from first Apply
// color: Some(blue_series) from second Apply

// ERROR: Conflict: 'color' applied multiple times to primitive 'ceiling-fixture-1:0'
```

This prevents ambiguity - if you want to blend red and blue, create two separate patterns and use blend modes at the track layer level.

### Graph Execution Integration

```rust
pub fn execute_pattern_graph(
    graph: &Graph,
    context: &GraphContext,
    fixtures: &HashMap<String, Fixture>,
) -> Result<LayerTimeSeries, String> {
    // 1. Execute graph (topological sort)
    let node_outputs = run_graph(graph, context);

    // 2. Collect all Apply node outputs
    let mut apply_outputs = Vec::new();
    for (node_id, node) in &graph.nodes {
        if node.type_id.starts_with("apply_") {
            if let Some(output) = node_outputs.get(node_id) {
                // Each Apply node returns a LayerTimeSeries
                apply_outputs.push(output.clone());
            }
        }
    }

    // 3. Merge all Apply outputs with conflict detection
    merge_apply_outputs(apply_outputs)
}
```

---

## Integration with Compositing

Apply nodes output `LayerTimeSeries` which flows into the compositing system described in `compositing-buffer-design.md`:

**Within Pattern (Merge - No Blending):**
```rust
// Pattern graph has multiple Apply nodes:
Select("The Ceiling") → Apply_Color(red_pulse)
                     ↘ Apply_Dimmer(fade)

// Merged into single LayerTimeSeries (no blending):
let pattern_output = execute_pattern_graph(&graph, &context, &fixtures)?;
```

**Between Patterns (Composite - With Blending):**
```rust
// Track has multiple patterns as layers:
let layer1 = execute_pattern_graph(&pattern1_graph, &ctx, &fixtures)?; // "Red Pulse"
let layer2 = execute_pattern_graph(&pattern2_graph, &ctx, &fixtures)?; // "Dim Fade"

// Compositor blends them by z-index and blend mode
let composite = composite_layers(vec![
    (layer1, z_index: 0, blend_mode: BlendMode::Add),
    (layer2, z_index: 1, blend_mode: BlendMode::Multiply),
], start_time, end_time, sample_rate)?;

// DMX renderer samples at playback time
let dmx_frame = render_dmx(&composite, current_time, fixtures);
```

---

## Fixture Interface Requirements

To support this system, the Fixture type needs these methods:

```rust
impl Fixture {
    // Head-specific capability lookup (QLC+ style)
    fn channel_number(&self, capability: QLCChannel::Group, control_byte: ControlByte, head: usize) -> Option<u32>;

    // Fixture-wide capability lookup (for global channels)
    fn global_channel_number(&self, capability: QLCChannel::Group, control_byte: ControlByte) -> Option<u32>;

    // Convenience methods
    fn master_intensity_channel(&self) -> Option<u32>;
    fn rgb_channels(&self, head: usize) -> Vec<u32>;
    fn rgbw_channels(&self, head: usize) -> Vec<u32>;
    fn cmy_channels(&self, head: usize) -> Vec<u32>;

    // Metadata
    fn heads(&self) -> &[QLCFixtureHead];
}
```

These map directly to QLC+ fixture definitions as described in `qlcplus-capability-mapping.md`.

---

## Benefits

✅ **Unified representation**: Fixtures and primitives use the same selection type
✅ **Graceful degradation**: Missing capabilities are skipped automatically
✅ **Flexible selection**: Can mix whole fixtures and individual heads
✅ **Intelligent fallback**: Head-specific → fixture-wide → skip
✅ **Handles diverse fixtures**: Works with global dimmers, per-head RGB, fixture-wide pan/tilt
✅ **Composable**: Apply nodes output standard LayerTimeSeries for merging/compositing
✅ **Conflict detection**: Prevents ambiguous patterns (same capability applied twice to same primitive)
✅ **Clear separation**: Merging within patterns vs compositing between patterns

---

## Summary

The selection system uses a simple `Selectable` enum to represent both fixtures and primitives uniformly. Apply nodes query capabilities using QLC+-style channel lookup with intelligent fallback logic. Multiple Apply nodes within a pattern merge their outputs with conflict detection (not blending). This handles the full diversity of DMX fixtures (global controls, per-head capabilities, mixed configurations) while keeping the node graph interface clean and semantic. Blending only happens between patterns at the track layer level.

use crate::models::node_graph::{BlendMode, LayerTimeSeries, Series};
use crate::models::universe::{PrimitiveState, UniverseState};
use std::collections::HashMap;

/// Sample a Series at `current_time` using binary search. Step mode: holds the
/// previous sample's value until the next one. Matches DMX wire semantics.
/// Returns values for all channels, or `defaults` if the series has no samples.
fn sample_series(series: &Series, current_time: f32, defaults: &[f32]) -> Vec<f32> {
    let samples = &series.samples;
    if samples.is_empty() {
        return defaults.to_vec();
    }

    // Binary search for the first sample at or after current_time.
    let idx = samples.partition_point(|s| s.time < current_time);

    let s = if idx == 0 {
        &samples[0]
    } else {
        &samples[idx - 1]
    };
    pad_values(&s.values, defaults)
}

/// Pad values to match defaults length.
fn pad_values(values: &[f32], defaults: &[f32]) -> Vec<f32> {
    let len = values.len().max(defaults.len());
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        result.push(
            values
                .get(i)
                .copied()
                .unwrap_or(defaults.get(i).copied().unwrap_or(0.0)),
        );
    }
    result
}

pub fn render_frame(layer: &LayerTimeSeries, current_time: f32) -> UniverseState {
    let mut primitives = HashMap::new();

    for prim in &layer.primitives {
        let dimmer = prim
            .dimmer
            .as_ref()
            .map(|s| sample_series(s, current_time, &[0.0])[0])
            .unwrap_or(0.0);

        let color_vals = prim
            .color
            .as_ref()
            .map(|s| sample_series(s, current_time, &[1.0, 1.0, 1.0]))
            .unwrap_or_else(|| vec![1.0, 1.0, 1.0]);
        let color = [
            color_vals.get(0).copied().unwrap_or(1.0),
            color_vals.get(1).copied().unwrap_or(1.0),
            color_vals.get(2).copied().unwrap_or(1.0),
        ];

        let strobe = prim
            .strobe
            .as_ref()
            .map(|s| sample_series(s, current_time, &[0.0])[0])
            .unwrap_or(0.0);

        let pos_vals = prim
            .position
            .as_ref()
            .map(|s| sample_series(s, current_time, &[0.0, 0.0]))
            .unwrap_or_else(|| vec![0.0, 0.0]);
        let position = [
            pos_vals.get(0).copied().unwrap_or(0.0),
            pos_vals.get(1).copied().unwrap_or(0.0),
        ];

        let speed = prim
            .speed
            .as_ref()
            .map(|s| sample_series(s, current_time, &[1.0])[0])
            .unwrap_or(1.0);

        primitives.insert(
            prim.primitive_id.clone(),
            PrimitiveState {
                dimmer: dimmer.clamp(0.0, 1.0),
                color,
                strobe: strobe.clamp(0.0, 1.0),
                position,
                speed: if speed > 0.5 { 1.0 } else { 0.0 },
            },
        );
    }

    UniverseState { primitives }
}

/// Composite a cue layer onto an existing universe at a given playback time,
/// blending only the channels that are actually set (`Some`) in the layer.
/// Channels that are `None` in the `PrimitiveTimeSeries` pass through from the
/// base unchanged — matching how the track-editor compositor handles partial
/// layers (e.g. `apply_strobe` only sets strobe; dimmer/color are untouched).
///
/// `intensity` scales dimmer before blending (master/group intensity).
/// `allowed_fixtures`: if Some, only affects primitives whose fixture_id prefix is in the set.
pub fn composite_layer_frame(
    base: &mut UniverseState,
    layer: &LayerTimeSeries,
    time: f32,
    blend_mode: BlendMode,
    intensity: f32,
    allowed_fixtures: Option<&std::collections::HashSet<&str>>,
) {
    use crate::compositor::{blend_color, blend_values};

    for prim in &layer.primitives {
        // Apply target filter
        if let Some(allowed) = allowed_fixtures {
            let fixture_id = prim
                .primitive_id
                .find(':')
                .map(|c| &prim.primitive_id[..c])
                .unwrap_or(&prim.primitive_id);
            if !allowed.contains(fixture_id) {
                continue;
            }
        }

        let base_prim = base
            .primitives
            .entry(prim.primitive_id.clone())
            .or_insert_with(|| PrimitiveState {
                dimmer: 0.0,
                color: [0.0, 0.0, 0.0],
                strobe: 0.0,
                position: [0.0, 0.0],
                speed: 0.0,
            });

        // Dimmer — only blend if this layer sets it
        if let Some(series) = &prim.dimmer {
            let val = sample_series(series, time, &[0.0])
                .first()
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let scaled = (val * intensity).clamp(0.0, 1.0);
            base_prim.dimmer = blend_values(base_prim.dimmer, scaled, blend_mode).clamp(0.0, 1.0);
        }

        // Color — only blend if this layer sets it
        if let Some(series) = &prim.color {
            let vals = sample_series(series, time, &[1.0, 1.0, 1.0, 1.0]);
            let r = vals.first().copied().unwrap_or(1.0).clamp(0.0, 1.0);
            let g = vals.get(1).copied().unwrap_or(1.0).clamp(0.0, 1.0);
            let b = vals.get(2).copied().unwrap_or(1.0).clamp(0.0, 1.0);
            let a = vals.get(3).copied().unwrap_or(1.0).clamp(0.0, 1.0);
            let base_rgba = [
                base_prim.color[0],
                base_prim.color[1],
                base_prim.color[2],
                base_prim.dimmer,
            ];
            let top_rgba = [r, g, b, a];
            let blended = blend_color(&base_rgba, &top_rgba, blend_mode);
            base_prim.color = [
                blended
                    .first()
                    .copied()
                    .unwrap_or(base_prim.color[0])
                    .clamp(0.0, 1.0),
                blended
                    .get(1)
                    .copied()
                    .unwrap_or(base_prim.color[1])
                    .clamp(0.0, 1.0),
                blended
                    .get(2)
                    .copied()
                    .unwrap_or(base_prim.color[2])
                    .clamp(0.0, 1.0),
            ];
        }

        // Strobe — only blend if this layer sets it
        if let Some(series) = &prim.strobe {
            let val = sample_series(series, time, &[0.0])
                .first()
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            base_prim.strobe = blend_values(base_prim.strobe, val, blend_mode).clamp(0.0, 1.0);
        }

        // Position — only update if this layer sets it
        if let Some(series) = &prim.position {
            let vals = sample_series(series, time, &[0.0, 0.0]);
            base_prim.position = [
                vals.first().copied().unwrap_or(base_prim.position[0]),
                vals.get(1).copied().unwrap_or(base_prim.position[1]),
            ];
        }

        // Speed — only update if this layer sets it
        if let Some(series) = &prim.speed {
            let val = sample_series(series, time, &[1.0])
                .first()
                .copied()
                .unwrap_or(1.0);
            base_prim.speed = if val > 0.5 { 1.0 } else { 0.0 };
        }
    }
}

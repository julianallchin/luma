use crate::models::node_graph::{BlendMode, LayerTimeSeries, Series};
use crate::models::universe::{PrimitiveState, UniverseState};
use std::collections::HashMap;

/// Sample a Series at `current_time` using binary search.
/// When `interpolate` is true, linearly interpolates between adjacent samples.
/// When false, holds the previous sample's value (step mode).
/// Returns values for all channels, or `defaults` if the series has no samples.
fn sample_series(
    series: &Series,
    current_time: f32,
    defaults: &[f32],
    interpolate: bool,
) -> Vec<f32> {
    let samples = &series.samples;
    if samples.is_empty() {
        return defaults.to_vec();
    }

    // Single sample — return it directly
    if samples.len() == 1 {
        return pad_values(&samples[0].values, defaults);
    }

    // Binary search for the insertion point
    let idx = samples.partition_point(|s| s.time < current_time);

    if idx == 0 {
        // Before first sample — hold first
        return pad_values(&samples[0].values, defaults);
    }
    if idx >= samples.len() {
        // After last sample — hold last
        return pad_values(&samples[samples.len() - 1].values, defaults);
    }

    // Between samples[idx-1] and samples[idx]
    let s0 = &samples[idx - 1];

    if !interpolate {
        // Step mode: hold previous sample value
        return pad_values(&s0.values, defaults);
    }

    // Linear interpolation
    let s1 = &samples[idx];
    let dt = s1.time - s0.time;
    let t = if dt > 1e-9 {
        ((current_time - s0.time) / dt).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let dim = series.dim.max(defaults.len());
    let mut result = Vec::with_capacity(dim);
    for i in 0..dim {
        let v0 = s0
            .values
            .get(i)
            .copied()
            .unwrap_or(defaults.get(i).copied().unwrap_or(0.0));
        let v1 = s1
            .values
            .get(i)
            .copied()
            .unwrap_or(defaults.get(i).copied().unwrap_or(0.0));
        // Skip lerp for NaN values (used as "hold" sentinel in position)
        if v0.is_nan() {
            result.push(v1);
        } else if v1.is_nan() {
            result.push(v0);
        } else {
            result.push(v0 + t * (v1 - v0));
        }
    }
    result
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

/// Find the maximum value a piecewise-linear Series reaches over [t0, t1].
/// Evaluates at both endpoints plus every sample point inside the window.
fn max_series_in_range(series: &Series, t0: f32, t1: f32, default: f32) -> f32 {
    let samples = &series.samples;
    if samples.is_empty() {
        return default;
    }
    let v0 = sample_series(series, t0, &[default], true)[0];
    let v1 = sample_series(series, t1, &[default], true)[0];
    let mut max_val = v0.max(v1);
    // Also evaluate at every sample point within the window (piecewise-linear peaks)
    let start_idx = samples.partition_point(|s| s.time < t0);
    let end_idx = samples.partition_point(|s| s.time <= t1);
    for s in &samples[start_idx..end_idx] {
        if let Some(&v) = s.values.first() {
            max_val = max_val.max(v);
        }
    }
    max_val
}

/// Like `render_frame` but takes the max dimmer each fixture reached over [t_prev, t_now].
/// Color, position, strobe, and speed are still snapshotted at t_now.
/// This prevents fast chases from skipping fixtures whose on-window falls between two frames.
pub fn render_frame_max(layer: &LayerTimeSeries, t_prev: f32, t_now: f32) -> UniverseState {
    let mut primitives = HashMap::new();

    for prim in &layer.primitives {
        let dimmer = prim
            .dimmer
            .as_ref()
            .map(|s| max_series_in_range(s, t_prev, t_now, 0.0))
            .unwrap_or(0.0);

        let color_vals = prim
            .color
            .as_ref()
            .map(|s| sample_series(s, t_now, &[1.0, 1.0, 1.0], true))
            .unwrap_or_else(|| vec![1.0, 1.0, 1.0]);
        let color = [
            color_vals.get(0).copied().unwrap_or(1.0),
            color_vals.get(1).copied().unwrap_or(1.0),
            color_vals.get(2).copied().unwrap_or(1.0),
        ];

        let strobe = prim
            .strobe
            .as_ref()
            .map(|s| sample_series(s, t_now, &[0.0], true)[0])
            .unwrap_or(0.0);

        let pos_vals = prim
            .position
            .as_ref()
            .map(|s| sample_series(s, t_now, &[0.0, 0.0], false))
            .unwrap_or_else(|| vec![0.0, 0.0]);
        let position = [
            pos_vals.get(0).copied().unwrap_or(0.0),
            pos_vals.get(1).copied().unwrap_or(0.0),
        ];

        let speed = prim
            .speed
            .as_ref()
            .map(|s| sample_series(s, t_now, &[1.0], true)[0])
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

pub fn render_frame(layer: &LayerTimeSeries, current_time: f32) -> UniverseState {
    let mut primitives = HashMap::new();

    for prim in &layer.primitives {
        let dimmer = prim
            .dimmer
            .as_ref()
            .map(|s| sample_series(s, current_time, &[0.0], true)[0])
            .unwrap_or(0.0);

        let color_vals = prim
            .color
            .as_ref()
            .map(|s| sample_series(s, current_time, &[1.0, 1.0, 1.0], true))
            .unwrap_or_else(|| vec![1.0, 1.0, 1.0]);
        let color = [
            color_vals.get(0).copied().unwrap_or(1.0),
            color_vals.get(1).copied().unwrap_or(1.0),
            color_vals.get(2).copied().unwrap_or(1.0),
        ];

        let strobe = prim
            .strobe
            .as_ref()
            .map(|s| sample_series(s, current_time, &[0.0], true)[0])
            .unwrap_or(0.0);

        let pos_vals = prim
            .position
            .as_ref()
            .map(|s| sample_series(s, current_time, &[0.0, 0.0], false))
            .unwrap_or_else(|| vec![0.0, 0.0]);
        let position = [
            pos_vals.get(0).copied().unwrap_or(0.0),
            pos_vals.get(1).copied().unwrap_or(0.0),
        ];

        let speed = prim
            .speed
            .as_ref()
            .map(|s| sample_series(s, current_time, &[1.0], true)[0])
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
            let val = sample_series(series, time, &[0.0], true)
                .first()
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let scaled = (val * intensity).clamp(0.0, 1.0);
            base_prim.dimmer = blend_values(base_prim.dimmer, scaled, blend_mode).clamp(0.0, 1.0);
        }

        // Color — only blend if this layer sets it
        if let Some(series) = &prim.color {
            let vals = sample_series(series, time, &[1.0, 1.0, 1.0, 1.0], true);
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
            let val = sample_series(series, time, &[0.0], false)
                .first()
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            base_prim.strobe = blend_values(base_prim.strobe, val, blend_mode).clamp(0.0, 1.0);
        }

        // Position — only update if this layer sets it
        if let Some(series) = &prim.position {
            let vals = sample_series(series, time, &[0.0, 0.0], false);
            base_prim.position = [
                vals.first().copied().unwrap_or(base_prim.position[0]),
                vals.get(1).copied().unwrap_or(base_prim.position[1]),
            ];
        }

        // Speed — only update if this layer sets it
        if let Some(series) = &prim.speed {
            let val = sample_series(series, time, &[1.0], true)
                .first()
                .copied()
                .unwrap_or(1.0);
            base_prim.speed = if val > 0.5 { 1.0 } else { 0.0 };
        }
    }
}

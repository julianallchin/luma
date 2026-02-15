use crate::models::node_graph::{LayerTimeSeries, Series};
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

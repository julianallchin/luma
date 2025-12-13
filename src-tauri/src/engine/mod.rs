use crate::models::schema::LayerTimeSeries;
use crate::models::universe::{PrimitiveState, UniverseState};
use std::collections::HashMap;

// Simple "Engine" stub for preview rendering
// In future, this will be the real-time loop.
pub fn render_frame(layer: &LayerTimeSeries, current_time: f32) -> UniverseState {
    let mut primitives = HashMap::new();

    for prim in &layer.primitives {
        // Sample Dimmer
        let dimmer = prim
            .dimmer
            .as_ref()
            .and_then(|series| {
                // Find closest sample
                // Assuming sorted samples
                // Linear interpolation would be better
                let mut val = 0.0;
                let mut min_dist = f32::MAX;
                for s in &series.samples {
                    let dist = (s.time - current_time).abs();
                    if dist < min_dist {
                        min_dist = dist;
                        val = s.values.first().copied().unwrap_or(0.0);
                    }
                }
                // If "too far", assume 0? Or hold last? For preview, hold closest.
                Some(val)
            })
            .unwrap_or(0.0);

        // Sample Color
        let color = prim
            .color
            .as_ref()
            .and_then(|series| {
                let mut val = [0.0, 0.0, 0.0];
                let mut min_dist = f32::MAX;
                for s in &series.samples {
                    let dist = (s.time - current_time).abs();
                    if dist < min_dist {
                        min_dist = dist;
                        val = [
                            s.values.get(0).copied().unwrap_or(0.0),
                            s.values.get(1).copied().unwrap_or(0.0),
                            s.values.get(2).copied().unwrap_or(0.0),
                        ];
                    }
                }
                Some(val)
            })
            .unwrap_or([1.0, 1.0, 1.0]); // Default to White so dimmer works alone

        // Sample Strobe (0.0 = open/off, 1.0 = fastest)
        let strobe = prim
            .strobe
            .as_ref()
            .and_then(|series| {
                let mut val = 0.0;
                let mut min_dist = f32::MAX;
                for s in &series.samples {
                    let dist = (s.time - current_time).abs();
                    if dist < min_dist {
                        min_dist = dist;
                        val = s.values.first().copied().unwrap_or(0.0);
                    }
                }
                Some(val)
            })
            .unwrap_or(0.0);

        // Sample Position (PanDeg, TiltDeg)
        let position = prim
            .position
            .as_ref()
            .and_then(|series| {
                let mut val = [0.0, 0.0];
                let mut min_dist = f32::MAX;
                for s in &series.samples {
                    let dist = (s.time - current_time).abs();
                    if dist < min_dist {
                        min_dist = dist;
                        val = [
                            s.values.get(0).copied().unwrap_or(0.0),
                            s.values.get(1).copied().unwrap_or(0.0),
                        ];
                    }
                }
                Some(val)
            })
            .unwrap_or([0.0, 0.0]);

        primitives.insert(
            prim.primitive_id.clone(),
            PrimitiveState {
                dimmer: dimmer.clamp(0.0, 1.0),
                color,
                strobe: strobe.clamp(0.0, 1.0),
                position,
            },
        );
    }

    UniverseState { primitives }
}

pub mod edit;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PortType {
    Beats,
    Proportion,
    Position,
    Boolean,
    Mapping,
    Coordinates,
    Boundary,
    Envelope,
    Mask,
    Lighting,

    Intensity,
    Audio,
    BeatGrid,
    Series,
    Color,
    Selection,
    Signal,
    Events,
    Stops,
}

impl PortType {
    /// The type's wire spelling — the string key UI palettes color ports by
    /// (`luma_ui` keeps string keys so it does not depend on this crate).
    /// Matched exhaustively so a new port type is a compile error here rather
    /// than a silently grey wire.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            PortType::Beats => "Beats",
            PortType::Proportion => "Proportion",
            PortType::Position => "Position",
            PortType::Boolean => "Boolean",
            PortType::Mapping => "Mapping",
            PortType::Coordinates => "Coordinates",
            PortType::Boundary => "Boundary",
            PortType::Envelope => "Envelope",
            PortType::Mask => "Mask",
            PortType::Lighting => "Lighting",
            PortType::Intensity => "Intensity",
            PortType::Audio => "Audio",
            PortType::BeatGrid => "BeatGrid",
            PortType::Series => "Series",
            PortType::Color => "Color",
            PortType::Selection => "Selection",
            PortType::Signal => "Signal",
            PortType::Events => "Events",
            PortType::Stops => "Stops",
        }
    }
}

/// One choice in a [`ParamType::Enum`]: the string stored in the graph, and the
/// label a picker shows for it.
#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ParamOption {
    pub id: String,
    pub label: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ParamType {
    Number,
    Text,
    /// A closed set of strings, in menu order. The options are a projection of
    /// the lowering vocabulary itself (`eval::ops::math::MATH_OPS` and
    /// friends), so a picker cannot offer a value that will not compile — the
    /// whole point of the variant. Anything a picker should *not* constrain
    /// stays [`ParamType::Text`].
    Enum {
        options: Vec<ParamOption>,
    },
}

impl ParamType {
    /// A closed picker over a lowering vocabulary table's `(id, label, op)`
    /// rows — see `eval::ops::math::MATH_OPS`. The op column stays behind in the
    /// compiler; only the authoring pair crosses the seam.
    pub fn enum_of<T>(table: &[(&str, &str, T)]) -> Self {
        ParamType::Enum {
            options: table
                .iter()
                .map(|(id, label, _)| ParamOption {
                    id: (*id).to_string(),
                    label: (*label).to_string(),
                })
                .collect(),
        }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PatternArgType {
    AudioSource,
    Drum,
    Envelope,
    Beats,
    Proportion,
    Position,
    Boolean,
    Mapping,
    Boundary,

    Color,
    Scalar,
    Selection,
    Palette,
    Gradient,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename_all = "camelCase")]
pub struct PatternArgDef {
    pub id: String,
    pub name: String,
    pub arg_type: PatternArgType,
    #[ts(type = "Record<string, unknown>")]
    pub default_value: Value,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PortDef {
    pub id: String,
    pub name: String,
    pub port_type: PortType,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ParamDef {
    pub id: String,
    pub name: String,
    pub param_type: ParamType,
    pub default_number: Option<f32>,
    pub default_text: Option<String>,
    /// `(min, max)` for a number a slider can carry. Catalogue knowledge, not
    /// view knowledge: both editors used to hardcode these per node type, and
    /// the copies drifted. `None` means the param has no natural bounds and
    /// renders as a bare field.
    #[serde(default)]
    pub range: Option<(f32, f32)>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct NodeTypeDef {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub inputs: Vec<PortDef>,
    pub outputs: Vec<PortDef>,
    pub params: Vec<ParamDef>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename_all = "camelCase")]
pub struct NodeInstance {
    pub id: String,
    pub type_id: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: HashMap<String, Value>,
    pub position_x: Option<f64>,
    pub position_y: Option<f64>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename_all = "camelCase")]
pub struct Graph {
    pub nodes: Vec<NodeInstance>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub args: Vec<PatternArgDef>,
}

/// Context provided by the host for graph execution.
/// The host is responsible for loading audio and computing beat grids.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct GraphContext {
    pub track_id: String,
    pub venue_id: String,
    pub start_time: f32,
    pub end_time: f32,
    pub beat_grid: Option<BeatGrid>,
    #[ts(type = "Record<string, unknown> | undefined")]
    pub arg_values: Option<HashMap<String, Value>>,
    #[ts(type = "number | undefined")]
    pub instance_seed: Option<u64>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BeatGrid {
    pub beats: Vec<f32>,
    pub downbeats: Vec<f32>,
    pub bpm: f32,
    pub downbeat_offset: f32,
    pub beats_per_bar: i32,
}

impl BeatGrid {
    /// The same musical origin and variable-tempo clock for authored clips,
    /// preview sampling, and playback.
    pub fn timeline(&self) -> luma_patterns::Result<luma_patterns::BeatTimeline> {
        luma_patterns::BeatTimeline::new(
            self.beats.iter().map(|time| f64::from(*time)).collect(),
            f64::from(
                self.downbeats
                    .first()
                    .copied()
                    .unwrap_or(self.downbeat_offset),
            ),
        )
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SeriesSample {
    pub time: f32,
    pub values: Vec<f32>,
    pub label: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct Series {
    pub dim: usize,
    pub labels: Option<Vec<String>>,
    pub samples: Vec<SeriesSample>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub n: usize,       // Spatial dimension (Selection size)
    pub t: usize,       // Temporal dimension (Time samples)
    pub c: usize,       // Channel dimension (Data components)
    pub data: Vec<f32>, // Flat buffer: [n * (t * c) + t * c + c]
}

/// An ordered set of color anchor points defining a 1D color function on
/// `t ∈ [0,1]`. Consumers either use the stops discretely (`colors()`) or
/// sample at arbitrary positions (`sample(u)` does exact OKLab interpolation
/// between bracketing stops).
///
/// Authored as either a `Palette` node (uniform-spaced stops, swatch UI) or a
/// `Gradient` node (user-positioned stops). The data structure is the same.
#[derive(TS, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stops {
    /// (t_position, rgba) — `t` in `[0,1]`, the list is sorted ascending by t.
    pub stops: Vec<(f32, [f32; 4])>,
}

impl Stops {
    pub fn len(&self) -> usize {
        self.stops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stops.is_empty()
    }

    /// Sample the color function at `u ∈ [0,1]`. Linear OKLab interpolation
    /// between bracketing stops. Clamps to the endpoints outside [0,1].
    pub fn sample(&self, u: f32) -> [f32; 4] {
        use luma_patterns::oklab::{oklab_to_srgb, srgb_to_oklab};
        if self.stops.is_empty() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        if self.stops.len() == 1 {
            return self.stops[0].1;
        }
        let u = u.clamp(0.0, 1.0);
        if u <= self.stops[0].0 {
            return self.stops[0].1;
        }
        if u >= self.stops[self.stops.len() - 1].0 {
            return self.stops[self.stops.len() - 1].1;
        }
        // Binary search for the right bracketing pair.
        let mut lo = 0usize;
        let mut hi = self.stops.len() - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.stops[mid].0 <= u {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (t0, c0) = self.stops[lo];
        let (t1, c1) = self.stops[hi];
        let span = (t1 - t0).max(1e-6);
        let local = ((u - t0) / span).clamp(0.0, 1.0);
        let (l0, a0, b0) = srgb_to_oklab(c0[0], c0[1], c0[2]);
        let (l1, a1, b1) = srgb_to_oklab(c1[0], c1[1], c1[2]);
        let l = l0 + (l1 - l0) * local;
        let a = a0 + (a1 - a0) * local;
        let b = b0 + (b1 - b0) * local;
        let (r, g, bb) = oklab_to_srgb(l, a, b);
        [r, g, bb, c0[3] + (c1[3] - c0[3]) * local]
    }

    /// Sample at `k` evenly-spaced u positions.
    pub fn sample_uniform(&self, k: usize) -> Vec<[f32; 4]> {
        if k == 0 {
            return Vec::new();
        }
        (0..k)
            .map(|i| {
                let u = if k == 1 {
                    0.0
                } else {
                    i as f32 / (k - 1) as f32
                };
                self.sample(u)
            })
            .collect()
    }

    /// Get the raw stop colors in order (positions discarded). For "use the
    /// K colors as-is" consumers.
    pub fn colors(&self) -> Vec<[f32; 4]> {
        self.stops.iter().map(|(_, c)| *c).collect()
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AudioCrop {
    pub start_seconds: f32,
    pub end_seconds: f32,
}

pub use luma_patterns::BlendMode;

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PrimitiveTimeSeries {
    pub primitive_id: String,
    // Using Series for each capability
    pub color: Option<Series>,    // dim=3 (RGB) or 4 (RGBW)
    pub dimmer: Option<Series>,   // dim=1
    pub position: Option<Series>, // dim=2 (Pan, Tilt)
    pub strobe: Option<Series>,   // dim=2 (Enabled, Rate)
    pub speed: Option<Series>,    // dim=1 (0 = frozen, 1 = fast)
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LayerTimeSeries {
    pub primitives: Vec<PrimitiveTimeSeries>,
}

#[derive(TS, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RunResult {
    pub views: HashMap<String, Signal>,
    pub mel_specs: HashMap<String, crate::models::tracks::MelSpec>,
    pub color_views: HashMap<String, String>,
    pub universe_state: Option<crate::models::universe::UniverseState>,
}

#[cfg(test)]
mod gradient_contract_tests {
    use super::Stops;

    #[test]
    fn canonical_gradients_preserve_existing_perceptual_fades() {
        for colors in [
            [[1., 0., 0.], [0., 0., 1.], [0., 1., 0.]],
            [[0., 0., 0.], [0.2, 0.5, 0.9], [1., 1., 1.]],
        ] {
            let legacy = Stops {
                stops: [0., 0.3, 1.]
                    .into_iter()
                    .zip(colors)
                    .map(|(t, c)| (t, [c[0], c[1], c[2], 1.]))
                    .collect(),
            };
            let graph = luma_patterns::Gradient {
                stops: legacy
                    .stops
                    .iter()
                    .map(|(t, c)| luma_patterns::ColorStop {
                        t: f64::from(*t),
                        color: [c[0], c[1], c[2]].map(f64::from),
                    })
                    .collect(),
            };
            for step in 0..=256 {
                let t = step as f32 / 256.;
                let old = legacy.sample(t);
                let new = graph.sample(f64::from(t));
                for channel in 0..3 {
                    assert!(
                        (f64::from(old[channel]) - new[channel]).abs() < 2e-6,
                        "t={t}: {old:?} {new:?}"
                    );
                }
            }
        }
    }
}

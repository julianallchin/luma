//! Shipped presets: clip graphs named as the effect, curve shapes,
//! gradients, and frequency bands for `audio`. The data lives in
//! `presets.json` (spec section 9).
use crate::clip_graph::{ClipGraph, Kind};
use crate::{BlendMode, Clip, Curve, Gradient, Selection};
use serde::{Deserialize, Serialize};

/// A named clip: a graph and the blend mode it ships with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipPreset {
    pub name: String,
    pub blend_mode: BlendMode,
    /// What the preset needs from the rig to look right, such as "needs
    /// vertical bars".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub graph: ClipGraph,
}

/// A named curve shape: points over x 0–1 with values 0–1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurvePreset {
    pub name: String,
    pub curve: Curve<f64>,
}

/// A named gradient.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradientPreset {
    pub name: String,
    pub gradient: Gradient,
}

/// A named frequency band of the full mix for an `audio` node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BandPreset {
    pub name: String,
    pub low_hz: f64,
    pub high_hz: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presets {
    pub clips: Vec<ClipPreset>,
    pub curves: Vec<CurvePreset>,
    pub gradients: Vec<GradientPreset>,
    pub bands: Vec<BandPreset>,
}

/// The shipped presets, in menu order.
pub fn presets() -> &'static Presets {
    static PRESETS: std::sync::OnceLock<Presets> = std::sync::OnceLock::new();
    PRESETS.get_or_init(|| {
        serde_json::from_str(include_str!("presets.json")).expect("shipped presets")
    })
}

impl Presets {
    /// The clip preset called `name`. Names are unique across kinds.
    pub fn clip(&self, name: &str) -> Option<&ClipPreset> {
        self.clips.iter().find(|preset| preset.name == name)
    }
    /// The curve shape called `name`.
    pub fn curve(&self, name: &str) -> Option<&Curve<f64>> {
        self.curves
            .iter()
            .find(|preset| preset.name == name)
            .map(|preset| &preset.curve)
    }
    /// The gradient called `name`.
    pub fn gradient(&self, name: &str) -> Option<&Gradient> {
        self.gradients
            .iter()
            .find(|preset| preset.name == name)
            .map(|preset| &preset.gradient)
    }
    /// The band called `name`, as `(low_hz, high_hz)`.
    pub fn band(&self, name: &str) -> Option<(f64, f64)> {
        self.bands
            .iter()
            .find(|preset| preset.name == name)
            .map(|preset| (preset.low_hz, preset.high_hz))
    }
}

impl ClipPreset {
    /// The output kind of the preset's graph.
    pub fn output_kind(&self) -> Kind {
        self.graph
            .output_kind()
            .expect("a shipped preset has an output")
    }

    /// A new clip of this preset: its name, blend mode and a copy of its
    /// graph, over all heads.
    pub fn clip(&self, start: f64, duration: f64) -> Clip {
        Clip {
            name: self.name.clone(),
            start,
            duration,
            seed: 0,
            selection_seed: None,
            selection: Selection::all(),
            z_index: 0,
            blend_mode: self.blend_mode,
            graph: self.graph.clone(),
        }
    }
}

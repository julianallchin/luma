use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One named option of a choice input: the id stored in the clip, and the
/// label a picker shows for it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ParamOption {
    pub id: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PatternArgType {
    Envelope,
    Beats,
    Proportion,
    Position,
    Boolean,
    Mapping,
    Boundary,
    /// One of a form input's named options.
    Choice,

    Color,
    Scalar,
    Selection,
    Gradient,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatternArgDef {
    pub id: String,
    pub name: String,
    pub arg_type: PatternArgType,
    pub default_value: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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

pub use luma_patterns::BlendMode;

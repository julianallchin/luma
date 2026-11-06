use serde::{Deserialize, Serialize};

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

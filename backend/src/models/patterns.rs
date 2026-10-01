use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationPreview {
    pub annotation_id: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8, sRGB: the light mapped for a screen.
    pub pixels: Vec<u8>,
    /// The mean of `pixels`, sRGB 0–1.
    pub dominant_color: [f32; 3],
    /// An aim clip's pan and tilt, when any of its heads can move: the
    /// timeline draws these curves instead of `pixels`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aim: Option<AimCurves>,
}

/// The solved pan and tilt of an aim clip's heads over its span, one band
/// each. A curve is one sample per step from the clip's start to its end,
/// each 0 (bottom of the band) to 1 (top). Heads that move alike share one
/// curve, so a band holds only the distinct tracks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AimCurves {
    pub pan: Vec<Vec<f32>>,
    pub tilt: Vec<Vec<f32>>,
}

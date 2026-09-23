use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationPreview {
    pub annotation_id: String,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub dominant_color: [f32; 3],
}

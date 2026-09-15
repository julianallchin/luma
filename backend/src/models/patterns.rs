use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationPreview {
    pub annotation_id: String,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub dominant_color: [f32; 3],
}

#[derive(Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PatternSummary {
    /// None for a library Pattern; otherwise owned by this score.
    pub score_id: Option<String>,
    pub id: String,
    pub uid: Option<String>,
    pub name: String,
    pub description: Option<String>,
    #[sqlx(rename = "category_name")]
    pub category_name: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
    #[sqlx(rename = "is_verified")]
    pub is_verified: bool,
    pub author_name: Option<String>,
    #[sqlx(rename = "forked_from_id")]
    pub forked_from_id: Option<String>,
}

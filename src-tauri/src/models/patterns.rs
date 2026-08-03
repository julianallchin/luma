use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AnnotationPreview {
    pub annotation_id: String,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub dominant_color: [f32; 3],
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternSummary {
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

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ForkPatternInput {
    pub source_pattern_id: String,
    pub source_implementation_id: String,
    /// Caller-owned idempotency key. Retrying this exact request returns the
    /// same target pattern, implementation, and authored revision.
    pub request_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ForkPatternResult {
    pub pattern: PatternSummary,
    pub implementation_id: String,
    pub document_id: String,
    pub revision_id: String,
    pub applied_to_current_projection: bool,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternCategory {
    pub id: String,
    pub uid: Option<String>,
    pub name: String,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

#[allow(dead_code)]
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternDetail {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub graph_json: String,
    pub created_at: String,
    pub updated_at: String,
}

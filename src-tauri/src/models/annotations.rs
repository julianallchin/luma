use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A track annotation represents a pattern placed on a track's timeline
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackAnnotation {
    #[ts(type = "number")]
    pub id: i64,
    #[ts(type = "number")]
    pub track_id: i64,
    #[ts(type = "number")]
    pub pattern_id: i64,
    pub start_time: f64,
    pub end_time: f64,
    #[ts(type = "number")]
    pub z_index: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for creating a new annotation
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateAnnotationInput {
    #[ts(type = "number")]
    pub track_id: i64,
    #[ts(type = "number")]
    pub pattern_id: i64,
    pub start_time: f64,
    pub end_time: f64,
    #[ts(type = "number")]
    pub z_index: i64,
}

/// Input for updating an annotation
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct UpdateAnnotationInput {
    #[ts(type = "number")]
    pub id: i64,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    #[ts(type = "number | null")]
    pub z_index: Option<i64>,
}

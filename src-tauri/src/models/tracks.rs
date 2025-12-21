use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackSummary {
    #[ts(type = "number")]
    pub id: i64,
    pub track_hash: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[ts(type = "number | null")]
    pub track_number: Option<i64>,
    #[ts(type = "number | null")]
    pub disc_number: Option<i64>,
    #[ts(type = "number | null")]
    pub duration_seconds: Option<f64>,
    pub file_path: String,
    pub album_art_path: Option<String>,
    pub album_art_mime: Option<String>,
    pub album_art_data: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct MelSpec {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub beat_grid: Option<crate::models::node_graph::BeatGrid>,
}

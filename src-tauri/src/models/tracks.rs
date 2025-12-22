use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackSummary {
    #[ts(type = "number")]
    pub id: i64,
    pub remote_id: Option<String>,
    pub uid: Option<String>,
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
    pub storage_path: Option<String>,
    pub album_art_path: Option<String>,
    pub album_art_mime: Option<String>,
    /// Computed field - base64 data URL generated from album_art_path, not stored in DB
    #[sqlx(skip)]
    pub album_art_data: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Beat analysis data for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackBeats {
    #[ts(type = "number")]
    #[sqlx(rename = "track_id")]
    pub track_id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[sqlx(rename = "beats_json")]
    pub beats_json: String,
    #[sqlx(rename = "downbeats_json")]
    pub downbeats_json: String,
    pub bpm: Option<f64>,
    #[sqlx(rename = "downbeat_offset")]
    pub downbeat_offset: Option<f64>,
    #[ts(type = "number | null")]
    #[sqlx(rename = "beats_per_bar")]
    pub beats_per_bar: Option<i64>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

/// Root/section analysis data for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackRoots {
    #[ts(type = "number")]
    #[sqlx(rename = "track_id")]
    pub track_id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[sqlx(rename = "sections_json")]
    pub sections_json: String,
    /// Local file path to logits data
    #[sqlx(rename = "logits_path")]
    pub logits_path: Option<String>,
    /// Cloud storage path for compressed logits
    #[sqlx(rename = "logits_storage_path")]
    pub logits_storage_path: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

/// Stem audio file for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackStem {
    #[ts(type = "number")]
    #[sqlx(rename = "track_id")]
    pub track_id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[sqlx(rename = "stem_name")]
    pub stem_name: String,
    /// Local file path to stem audio
    #[sqlx(rename = "file_path")]
    pub file_path: String,
    /// Cloud storage path for compressed stem
    #[sqlx(rename = "storage_path")]
    pub storage_path: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
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

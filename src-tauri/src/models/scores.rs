use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use ts_rs::TS;

use super::node_graph::BlendMode;

/// A score is a named collection of pattern placements for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Score {
    #[ts(type = "number")]
    pub id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[ts(type = "number")]
    #[sqlx(rename = "track_id")]
    pub track_id: i64,
    pub name: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

/// A track score represents a pattern placed on a score's timeline
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackScore {
    #[ts(type = "number")]
    pub id: i64,
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[ts(type = "number")]
    pub score_id: i64,
    #[ts(type = "number")]
    pub pattern_id: i64,
    pub start_time: f64,
    pub end_time: f64,
    #[ts(type = "number")]
    pub z_index: i64,
    pub blend_mode: BlendMode,
    #[ts(type = "Record<string, unknown>")]
    pub args: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for creating a new score container
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateScoreContainerInput {
    #[ts(type = "number")]
    pub track_id: i64,
    pub name: Option<String>,
}

/// Input for creating a new track score (pattern placement)
/// The backend automatically finds or creates the score container for the track.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateTrackScoreInput {
    #[ts(type = "number")]
    pub track_id: i64,
    #[ts(type = "number")]
    pub pattern_id: i64,
    pub start_time: f64,
    pub end_time: f64,
    #[ts(type = "number")]
    pub z_index: i64,
    #[serde(default)]
    pub blend_mode: Option<BlendMode>,
    #[serde(default)]
    #[ts(type = "Record<string, unknown> | undefined")]
    pub args: Option<Value>,
}

/// Input for updating a track score
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct UpdateTrackScoreInput {
    #[ts(type = "number")]
    pub id: i64,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    #[ts(type = "number | null")]
    pub z_index: Option<i64>,
    pub blend_mode: Option<BlendMode>,
    #[serde(default)]
    #[ts(type = "Record<string, unknown> | undefined")]
    pub args: Option<Value>,
}

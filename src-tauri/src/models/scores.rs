use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row};
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

impl<'r> FromRow<'r, SqliteRow> for TrackScore {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let id: i64 = row.try_get("id")?;
        let remote_id: Option<String> = row.try_get("remote_id")?;
        let uid: Option<String> = row.try_get("uid")?;
        let score_id: i64 = row.try_get("score_id")?;
        let pattern_id: i64 = row.try_get("pattern_id")?;
        let start_time: f64 = row.try_get("start_time")?;
        let end_time: f64 = row.try_get("end_time")?;
        let z_index: i64 = row.try_get("z_index")?;
        let created_at: String = row.try_get("created_at")?;
        let updated_at: String = row.try_get("updated_at")?;

        // Deserialize blend_mode from plain string to enum
        let blend_mode_str: String = row.try_get("blend_mode")?;
        let blend_mode: BlendMode = serde_json::from_str(&format!("\"{}\"", blend_mode_str))
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        // Deserialize args from JSON string
        let args_json: String = row.try_get("args_json")?;
        let args: Value = serde_json::from_str(&args_json)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        Ok(TrackScore {
            id,
            remote_id,
            uid,
            score_id,
            pattern_id,
            start_time,
            end_time,
            z_index,
            blend_mode,
            args,
            created_at,
            updated_at,
        })
    }
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

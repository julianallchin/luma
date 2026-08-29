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
    pub id: String,
    pub uid: Option<String>,
    #[sqlx(rename = "track_id")]
    pub track_id: String,
    #[sqlx(rename = "venue_id")]
    pub venue_id: String,
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
    pub id: String,
    pub uid: Option<String>,
    pub score_id: String,
    pub pattern_id: String,
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
        let id: String = row.try_get("id")?;
        let uid: Option<String> = row.try_get("uid")?;
        let score_id: String = row.try_get("score_id")?;
        let pattern_id: String = row.try_get("pattern_id")?;
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
        let args: Value =
            serde_json::from_str(&args_json).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        Ok(TrackScore {
            id,
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

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ScoreSummary {
    pub id: String,
    pub uid: Option<String>,
    #[sqlx(rename = "venue_id")]
    pub venue_id: Option<String>,
    /// The venue's display name, joined in by the listing queries so a
    /// cross-venue picker needs no second lookup. `None` only when the join
    /// found no venue row — a summary is never grouped by id alone.
    #[sqlx(rename = "venue_name")]
    pub venue_name: Option<String>,
    pub name: Option<String>,
    /// The score's rank by `created_at` within its `(track, venue)`, 1-based
    /// — a handle a person can say out loud for a document whose identity is
    /// a uuid. Computed at read time and never stored: it is a *position* in
    /// a list, so deleting the first score renumbers the rest and anything
    /// that had written the old number down would be wrong.
    #[ts(type = "number")]
    pub ordinal: i64,
    #[sqlx(rename = "annotation_count")]
    #[ts(type = "number")]
    pub annotation_count: i64,
    /// Who wrote the newest revision of this score's authored document, in the
    /// open vocabulary [`crate::models::authored_state::ActorLabel`] reads.
    ///
    /// Provenance, not ownership: `uid` says whose document it is, and a score
    /// an agent authored through its owner's session is still owned by that
    /// person. `None` for a score with no authored history — one seeded
    /// straight into the table, or written before the column existed.
    #[sqlx(rename = "last_actor")]
    pub last_actor: Option<String>,
    /// When that revision was authored. The score row's own `updated_at` moves
    /// for reasons that are not authorship, so this is what a surface showing
    /// "last worked on" should read.
    #[sqlx(rename = "last_authored_at")]
    pub last_authored_at: Option<String>,
    /// How many revisions the document has, counting every one the history
    /// list would show — including the prepare half of an agent turn, so the
    /// number here and the rows there cannot disagree.
    #[sqlx(rename = "revision_count")]
    #[ts(type = "number")]
    pub revision_count: i64,
    /// What the agent runs that authored this score cost, in dollars, summed
    /// over every thread whose revisions touched the document. `None` when no
    /// run against it was ever priced — an operator's own edits, or a run
    /// whose harness reported no cost.
    #[sqlx(rename = "cost_usd")]
    pub cost_usd: Option<f64>,
    /// Tokens those same runs spent, all four counts summed. Zero rather than
    /// `None`: "no recorded run" and "a run that spent nothing" are the same
    /// number to a reader, and an unspent score simply says nothing.
    #[sqlx(rename = "total_tokens")]
    #[ts(type = "number")]
    pub total_tokens: i64,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

/// Input for creating a track score
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateTrackScoreInput {
    /// Caller-owned idempotency key. Retries must reuse this UUID.
    pub request_id: String,
    pub score_id: String,
    pub track_id: String,
    pub pattern_id: String,
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

/// Input for updating a track score.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct UpdateTrackScoreInput {
    /// Caller-owned idempotency key. Retries must reuse this UUID.
    pub operation_id: String,
    /// Stable containing scope. A retry must not depend on the clip row still
    /// existing (notably after a successful delete whose response was lost).
    pub score_id: String,
    pub track_id: String,
    pub id: String,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    #[ts(type = "number | null")]
    pub z_index: Option<i64>,
    pub blend_mode: Option<BlendMode>,
    #[serde(default)]
    #[ts(type = "Record<string, unknown> | undefined")]
    pub args: Option<Value>,
}

/// Input for deleting one clip through the idempotent authored-score edit
/// protocol. The containing score is explicit so an exact retry remains
/// resolvable after the clip itself has been removed.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct DeleteTrackScoreInput {
    /// Caller-owned idempotency key. Retries must reuse this UUID.
    pub operation_id: String,
    pub score_id: String,
    pub track_id: String,
    pub id: String,
}

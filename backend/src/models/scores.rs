use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

/// A score is a named collection of pattern placements for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
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

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
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
    /// When that edit landed. The score row's own `updated_at` moves for
    /// reasons that are not authorship, so this is what a surface showing
    /// "last worked on" should read.
    #[sqlx(rename = "last_authored_at")]
    pub last_authored_at: Option<String>,
    /// What the agent runs against this score cost, in dollars, summed over
    /// every thread bound to it. `None` when no run against it was ever priced
    /// — an operator's own edits, or a run whose harness reported no cost.
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

//! The canonical score document owns both graph definitions and clip instances.
//! Reads and projection writes share the authored-history transaction; NULL in
//! the score row preserves an existing, not-yet-migrated score.
use crate::services::track_edits::TrackScope;
use luma_patterns::{standard_library, Score};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

pub(crate) mod merge;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphScoreDocument {
    pub revision: String,
    pub score: Score,
}
impl GraphScoreDocument {
    pub fn new(score: Score) -> Result<Self, String> {
        score
            .validate(&standard_library())
            .map_err(|error| error.to_string())?;
        if score.clips.len() > 2048 {
            return Err("a score may contain at most 2,048 clips".into());
        }
        let source = source(&score)?;
        let mut hash = Sha256::new();
        hash.update(b"luma.graph-score.v2\0");
        hash.update(source.as_bytes());
        Ok(Self {
            revision: format!("sha256:{:x}", hash.finalize()),
            score,
        })
    }

    pub fn from_source(source: &str) -> Result<Self, String> {
        if source.len() > 6 * 1024 * 1024 {
            return Err("score document exceeds 6 MiB".into());
        }
        let score = serde_json::from_str(source)
            .map_err(|error| format!("invalid score document: {error}"))?;
        Self::new(score)
    }

    pub fn source(&self) -> Result<String, String> {
        source(&self.score)
    }
}

fn source(score: &Score) -> Result<String, String> {
    let value = serde_json::to_value(score).map_err(|error| error.to_string())?;
    let source = format!("{}\n", crate::canonical_json::to_string(&value));
    if source.len() > 6 * 1024 * 1024 {
        return Err("score document exceeds 6 MiB".into());
    }
    Ok(source)
}

/// Exact owner, track and venue checks take place on the same connection as
/// the read. Callers acquire read/write admission before entering this layer.
pub(crate) async fn load(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner: Option<&str>,
) -> Result<Option<GraphScoreDocument>, String> {
    let found: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT track_id, venue_id, uid, graph_document_json FROM scores WHERE id = ?",
    )
    .bind(&scope.score_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| error.to_string())?;
    let Some((track, venue, uid, source)) = found else {
        return Err("score does not exist".into());
    };
    if track != scope.track_id || venue != scope.venue_id || uid.as_deref() != owner {
        return Err("score does not belong to the exact authored scope".into());
    }
    source
        .as_deref()
        .map(GraphScoreDocument::from_source)
        .transpose()
}

/// Called only by the authored projector, after admission and its head CAS.
/// The source and the removal of old clip projections are one transaction.
pub(crate) async fn project(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner: Option<&str>,
    candidate: &GraphScoreDocument,
) -> Result<(), String> {
    // Revalidate the public deserialized boundary rather than trusting a
    // caller-supplied revision or a prior preview's validation.
    let checked = GraphScoreDocument::new(candidate.score.clone())?;
    if checked.revision != candidate.revision {
        return Err("score revision does not match its content".into());
    }
    load(connection, scope, owner).await?;
    let updated = sqlx::query("UPDATE scores SET graph_document_json = ? WHERE id = ? AND track_id = ? AND venue_id = ? AND uid IS ?")
        .bind(checked.source()?).bind(&scope.score_id).bind(&scope.track_id).bind(&scope.venue_id).bind(owner)
        .execute(&mut *connection).await.map_err(|error| error.to_string())?.rows_affected();
    if updated != 1 {
        return Err("score disappeared during authored projection".into());
    }
    sqlx::query("DELETE FROM track_scores WHERE score_id = ?")
        .bind(&scope.score_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

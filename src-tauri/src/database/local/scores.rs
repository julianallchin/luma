use sqlx::{FromRow, SqlitePool};

use crate::models::node_graph::BlendMode;
use crate::models::scores::{CreateTrackScoreInput, TrackScore, UpdateTrackScoreInput};
use serde_json::Value;

// Helper struct for update operations
#[derive(FromRow)]
struct ExistingTrackScoreFields {
    pattern_id: i64,
    start_time: f64,
    end_time: f64,
    z_index: i64,
    blend_mode: String,
    args_json: String,
}

/// Core: list all track_scores for a track (via scores table)
pub async fn get_scores_for_track(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<Vec<TrackScore>, String> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT track_scores.id, track_scores.remote_id, track_scores.uid, track_scores.score_id, track_scores.pattern_id, track_scores.start_time, track_scores.end_time, track_scores.z_index, track_scores.blend_mode, track_scores.args_json, track_scores.created_at, track_scores.updated_at
         FROM track_scores
         JOIN scores ON track_scores.score_id = scores.id
         WHERE scores.track_id = ?
         ORDER BY track_scores.start_time ASC, track_scores.z_index ASC",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list track_scores: {}", e))
}

/// Core: create a new track_score entry
/// Automatically finds or creates the score container for the track.
pub async fn create_track_score(
    pool: &SqlitePool,
    payload: CreateTrackScoreInput,
) -> Result<TrackScore, String> {
    // Get or create the score container for this track
    let score_id = ensure_score_id(pool, payload.track_id).await?;

    let res = sqlx::query(
        "INSERT INTO track_scores (score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(score_id)
    .bind(payload.pattern_id)
    .bind(payload.start_time)
    .bind(payload.end_time)
    .bind(payload.z_index)
    .bind(blend_mode_to_string(
        &payload.blend_mode.unwrap_or(BlendMode::Replace),
    ))
    .bind(
        payload
            .args
            .unwrap_or_else(|| Value::Object(Default::default()))
            .to_string(),
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create track_score: {}", e))?;

    let id = res.last_insert_rowid();
    sqlx::query_as::<_, TrackScore>(
        "SELECT id, remote_id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at
         FROM track_scores
         WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch inserted track_score: {}", e))
}

/// Core: update an existing track_score
pub async fn update_track_score(
    pool: &SqlitePool,
    payload: UpdateTrackScoreInput,
) -> Result<(), String> {
    // Fetch existing to merge defaults
    let existing: Option<ExistingTrackScoreFields> = sqlx::query_as(
        "SELECT pattern_id, start_time, end_time, z_index, blend_mode, args_json FROM track_scores WHERE id = ?",
    )
    .bind(payload.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load track_score for update: {}", e))?;

    let Some(existing) = existing else {
        return Err(format!("TrackScore {} not found", payload.id));
    };

    let new_start = payload.start_time.unwrap_or(existing.start_time);
    let new_end = payload.end_time.unwrap_or(existing.end_time);
    let new_z = payload.z_index.unwrap_or(existing.z_index);
    let new_blend = payload
        .blend_mode
        .map(|b| blend_mode_to_string(&b))
        .unwrap_or(existing.blend_mode);
    let new_args = payload
        .args
        .unwrap_or_else(|| serde_json::from_str(&existing.args_json).unwrap_or_default())
        .to_string();

    let result = sqlx::query(
        "UPDATE track_scores
         SET start_time = ?, end_time = ?, z_index = ?, blend_mode = ?, args_json = ?, updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(new_start)
    .bind(new_end)
    .bind(new_z)
    .bind(new_blend)
    .bind(new_args)
    .bind(payload.id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update track_score: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("TrackScore {} not found", payload.id));
    }

    Ok(())
}

/// Core: delete a track_score
pub async fn delete_track_score(pool: &SqlitePool, id: i64) -> Result<(), String> {
    let result = sqlx::query("DELETE FROM track_scores WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete track_score: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("TrackScore {} not found", id));
    }

    Ok(())
}

fn blend_mode_to_string(blend_mode: &BlendMode) -> String {
    match serde_json::to_string(blend_mode) {
        Ok(s) => s.trim_matches('"').to_string(),
        Err(_) => "replace".to_string(),
    }
}

async fn ensure_score_id(pool: &SqlitePool, track_id: i64) -> Result<i64, String> {
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM scores WHERE track_id = ? ORDER BY id DESC LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to find score for track {}: {}", track_id, e))?;

    if let Some(id) = existing {
        return Ok(id);
    }

    let res = sqlx::query("INSERT INTO scores (track_id) VALUES (?)")
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create score for track {}: {}", track_id, e))?;

    Ok(res.last_insert_rowid())
}

// -----------------------------------------------------------------------------
// Sync support
// -----------------------------------------------------------------------------

use crate::models::scores::Score;

/// Fetch a score by ID
pub async fn get_score(pool: &SqlitePool, id: i64) -> Result<Score, String> {
    sqlx::query_as::<_, Score>(
        "SELECT id, remote_id, uid, track_id, name, created_at, updated_at FROM scores WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch score: {}", e))
}

/// List all scores
pub async fn list_scores(pool: &SqlitePool) -> Result<Vec<Score>, String> {
    sqlx::query_as::<_, Score>(
        "SELECT id, remote_id, uid, track_id, name, created_at, updated_at FROM scores",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list scores: {}", e))
}

/// Set remote_id for a score after syncing to cloud
pub async fn set_score_remote_id(pool: &SqlitePool, id: i64, remote_id: i64) -> Result<(), String> {
    sqlx::query("UPDATE scores SET remote_id = ? WHERE id = ?")
        .bind(remote_id.to_string())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set score remote_id: {}", e))?;
    Ok(())
}

/// Fetch a track_score by ID
pub async fn get_track_score_row(pool: &SqlitePool, id: i64) -> Result<TrackScore, String> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT id, remote_id, uid, score_id, pattern_id, start_time, end_time, z_index,
         blend_mode, args_json, created_at, updated_at
         FROM track_scores WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch track_score: {}", e))
}

/// List all track_score IDs
pub async fn list_track_score_ids(pool: &SqlitePool) -> Result<Vec<i64>, String> {
    sqlx::query_scalar("SELECT id FROM track_scores")
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list track_scores: {}", e))
}

/// Set remote_id for a track_score after syncing to cloud
pub async fn set_track_score_remote_id(
    pool: &SqlitePool,
    id: i64,
    remote_id: i64,
) -> Result<(), String> {
    sqlx::query("UPDATE track_scores SET remote_id = ? WHERE id = ?")
        .bind(remote_id.to_string())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set track_score remote_id: {}", e))?;
    Ok(())
}

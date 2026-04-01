use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::models::node_graph::BlendMode;
use crate::models::scores::{
    CreateTrackScoreInput, Score, ScoreSummary, TrackScore, UpdateTrackScoreInput,
};
use serde_json::Value;

/// Minimum annotation duration = 1/32 of a bar.
/// Falls back to 120 BPM / 4 beats-per-bar when no beat grid exists.
async fn min_annotation_duration(pool: &SqlitePool, track_id: &str) -> f64 {
    let row: Option<(f64, i64)> =
        sqlx::query_as("SELECT bpm, beats_per_bar FROM track_beats WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

    let (bpm, beats_per_bar) = row.unwrap_or((120.0, 4));
    let bar_duration = (beats_per_bar as f64 / bpm) * 60.0;
    bar_duration / 32.0
}

fn validate_duration(start: f64, end: f64, min_dur: f64) -> Result<(), String> {
    let dur = end - start;
    if dur < min_dur {
        return Err(format!(
            "Annotation too short ({:.4}s). Minimum is 1/32 bar ({:.4}s).",
            dur, min_dur
        ));
    }
    Ok(())
}

#[derive(FromRow)]
struct ExistingTrackScoreFields {
    start_time: f64,
    end_time: f64,
    z_index: i64,
    blend_mode: String,
    args_json: String,
}

/// List all track_scores for a (track, venue) pair
pub async fn get_scores_for_track(
    pool: &SqlitePool,
    track_id: &str,
    venue_id: &str,
) -> Result<Vec<TrackScore>, String> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT track_scores.id, track_scores.uid, track_scores.score_id, track_scores.pattern_id, track_scores.start_time, track_scores.end_time, track_scores.z_index, track_scores.blend_mode, track_scores.args_json, track_scores.created_at, track_scores.updated_at
         FROM track_scores
         JOIN scores ON track_scores.score_id = scores.id
         WHERE scores.track_id = ? AND scores.venue_id = ?
         ORDER BY track_scores.start_time ASC, track_scores.z_index ASC",
    )
    .bind(track_id)
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list track_scores: {}", e))
}

/// Create a new track_score entry.
pub async fn create_track_score(
    pool: &SqlitePool,
    payload: CreateTrackScoreInput,
) -> Result<TrackScore, String> {
    let min_dur = min_annotation_duration(pool, &payload.track_id).await;
    validate_duration(payload.start_time, payload.end_time, min_dur)?;

    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO track_scores (id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json)
         VALUES (?, (SELECT uid FROM scores WHERE id = ?), ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&payload.score_id)
    .bind(&payload.score_id)
    .bind(&payload.pattern_id)
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

    sqlx::query_as::<_, TrackScore>(
        "SELECT id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at
         FROM track_scores
         WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch inserted track_score: {}", e))
}

/// Update an existing track_score.
pub async fn update_track_score(
    pool: &SqlitePool,
    payload: UpdateTrackScoreInput,
) -> Result<(), String> {
    let existing: Option<ExistingTrackScoreFields> = sqlx::query_as(
        "SELECT start_time, end_time, z_index, blend_mode, args_json FROM track_scores WHERE id = ?",
    )
    .bind(&payload.id)
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

    let track_id: String = sqlx::query_scalar(
        "SELECT s.track_id FROM track_scores ts JOIN scores s ON ts.score_id = s.id WHERE ts.id = ?",
    )
    .bind(&payload.id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to resolve track for annotation: {}", e))?;
    let min_dur = min_annotation_duration(pool, &track_id).await;
    validate_duration(new_start, new_end, min_dur)?;

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
    .bind(&payload.id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update track_score: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("TrackScore {} not found", payload.id));
    }

    Ok(())
}

/// Delete a track_score.
pub async fn delete_track_score(pool: &SqlitePool, id: &str) -> Result<(), String> {
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

/// Delete a score.
pub async fn delete_score(pool: &SqlitePool, id: &str) -> Result<(), String> {
    let result = sqlx::query("DELETE FROM scores WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete score: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("Score {} not found", id));
    }

    Ok(())
}

fn blend_mode_to_string(blend_mode: &BlendMode) -> String {
    match serde_json::to_string(blend_mode) {
        Ok(s) => s.trim_matches('"').to_string(),
        Err(_) => "replace".to_string(),
    }
}

/// Create a new score for a (track, venue, user).
pub async fn create_score(
    pool: &SqlitePool,
    track_id: &str,
    venue_id: &str,
    uid: &str,
    name: Option<&str>,
) -> Result<Score, String> {
    let id = Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO scores (id, track_id, venue_id, uid, name) VALUES (?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(track_id)
        .bind(venue_id)
        .bind(uid)
        .bind(name)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create score: {}", e))?;

    get_score(pool, &id).await
}

/// List scores for a (track, venue) pair.
pub async fn list_scores_for_track(
    pool: &SqlitePool,
    track_id: &str,
    venue_id: &str,
) -> Result<Vec<ScoreSummary>, String> {
    sqlx::query_as::<_, ScoreSummary>(
        "SELECT s.id, s.uid, s.name,
                COUNT(ts.id) AS annotation_count,
                s.created_at, s.updated_at
         FROM scores s
         LEFT JOIN track_scores ts ON ts.score_id = s.id
         WHERE s.track_id = ? AND s.venue_id = ?
         GROUP BY s.id
         ORDER BY s.updated_at DESC",
    )
    .bind(track_id)
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list scores for track: {}", e))
}

/// Fetch a score by ID
pub async fn get_score(pool: &SqlitePool, id: &str) -> Result<Score, String> {
    sqlx::query_as::<_, Score>(
        "SELECT id, uid, track_id, venue_id, name, created_at, updated_at FROM scores WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch score: {}", e))
}

/// List all track_scores for a given score_id
pub async fn list_track_scores_for_score(
    pool: &SqlitePool,
    score_id: &str,
) -> Result<Vec<TrackScore>, String> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at
         FROM track_scores
         WHERE score_id = ?
         ORDER BY start_time ASC, z_index ASC",
    )
    .bind(score_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list track_scores for score {}: {}", score_id, e))
}

/// Atomically replace all track_scores for a specific score.
/// Uses a diff-based upsert instead of DELETE-all + INSERT-all, so that rows
/// whose data hasn't changed keep their existing `synced_at` value and are not
/// re-queued for sync on every undo/redo operation.
pub async fn replace_track_scores(
    pool: &SqlitePool,
    score_id: &str,
    track_id: &str,
    scores: Vec<TrackScore>,
) -> Result<(), String> {
    let min_dur = min_annotation_duration(pool, track_id).await;
    // Filter out degenerate annotations instead of rejecting the whole batch
    let scores: Vec<TrackScore> = scores
        .into_iter()
        .filter(|s| (s.end_time - s.start_time) >= min_dur)
        .collect();

    let new_ids: std::collections::HashSet<&str> = scores.iter().map(|s| s.id.as_str()).collect();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Delete rows that are no longer in the new set.
    let existing_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM track_scores WHERE score_id = ?")
            .bind(score_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("Failed to fetch existing track_score ids: {}", e))?;

    for existing_id in &existing_ids {
        if !new_ids.contains(existing_id.as_str()) {
            sqlx::query("DELETE FROM track_scores WHERE id = ?")
                .bind(existing_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to delete track_score: {}", e))?;
        }
    }

    // Upsert each row. On conflict (existing row), update all mutable fields
    // but deliberately leave `synced_at` untouched — if the data is the same
    // as what was previously synced, the row stays clean.
    // `version = version + 1` in the DO UPDATE prevents the updated_at trigger
    // from firing (trigger condition: OLD.version = NEW.version), so updated_at
    // stays as the value we're restoring rather than being overwritten with NOW().
    for s in &scores {
        sqlx::query(
            "INSERT INTO track_scores (id, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, uid, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               score_id    = excluded.score_id,
               pattern_id  = excluded.pattern_id,
               start_time  = excluded.start_time,
               end_time    = excluded.end_time,
               z_index     = excluded.z_index,
               blend_mode  = excluded.blend_mode,
               args_json   = excluded.args_json,
               uid         = excluded.uid,
               updated_at  = excluded.updated_at,
               version     = version + 1",
        )
        .bind(&s.id)
        .bind(score_id)
        .bind(&s.pattern_id)
        .bind(s.start_time)
        .bind(s.end_time)
        .bind(s.z_index)
        .bind(blend_mode_to_string(&s.blend_mode))
        .bind(s.args.to_string())
        .bind(&s.uid)
        .bind(&s.created_at)
        .bind(&s.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to upsert track_score: {}", e))?;
    }

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit transaction: {}", e))?;

    Ok(())
}

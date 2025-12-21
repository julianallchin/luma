use sqlx::SqlitePool;

use crate::models::schema::BlendMode;
use crate::models::scores::{CreateScoreInput, TrackScore, UpdateScoreInput};
use serde_json::Value;

/// Core: list all scores for a track
pub async fn get_scores_for_track(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<Vec<TrackScore>, String> {
    let rows: Vec<(i64, i64, i64, f64, f64, i64, String, String, String, String)> = sqlx::query_as(
        "SELECT id, track_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at
         FROM track_scores
         WHERE track_id = ?
         ORDER BY start_time ASC, z_index ASC",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list scores: {}", e))?;

    rows.into_iter()
        .map(row_to_score)
        .collect::<Result<Vec<_>, _>>()
}

/// Core: create a new score entry
pub async fn create_score(
    pool: &SqlitePool,
    payload: CreateScoreInput,
) -> Result<TrackScore, String> {
    let res = sqlx::query(
        "INSERT INTO track_scores (track_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(payload.track_id)
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
    .map_err(|e| format!("Failed to create score: {}", e))?;

    let id = res.last_insert_rowid();
    let row: (i64, i64, i64, f64, f64, i64, String, String, String, String) =
        sqlx::query_as(
            "SELECT id, track_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at
         FROM track_scores WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to fetch inserted score: {}", e))?;

    row_to_score(row)
}

/// Core: update an existing score
pub async fn update_score(
    pool: &SqlitePool,
    payload: UpdateScoreInput,
) -> Result<(), String> {
    // Fetch existing to merge defaults
    let existing: Option<(i64, f64, f64, i64, String, String)> = sqlx::query_as(
        "SELECT pattern_id, start_time, end_time, z_index, blend_mode, args_json FROM track_scores WHERE id = ?",
    )
    .bind(payload.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load score for update: {}", e))?;

    let Some((_pattern_id, start_time, end_time, z_index, blend_mode, args_json)) = existing else {
        return Err(format!("Score {} not found", payload.id));
    };

    let new_start = payload.start_time.unwrap_or(start_time);
    let new_end = payload.end_time.unwrap_or(end_time);
    let new_z = payload.z_index.unwrap_or(z_index);
    let new_blend = payload
        .blend_mode
        .map(|b| blend_mode_to_string(&b))
        .unwrap_or(blend_mode);
    let new_args = payload
        .args
        .unwrap_or_else(|| serde_json::from_str(&args_json).unwrap_or_default())
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
    .map_err(|e| format!("Failed to update score: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("Score {} not found", payload.id));
    }

    Ok(())
}

/// Core: delete a score
pub async fn delete_score(pool: &SqlitePool, id: i64) -> Result<(), String> {
    let result = sqlx::query("DELETE FROM track_scores WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete score: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("Score {} not found", id));
    }

    Ok(())
}

fn row_to_score(
    row: (i64, i64, i64, f64, f64, i64, String, String, String, String),
) -> Result<TrackScore, String> {
    let (id, track_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at) =
        row;
    let blend_mode = serde_json::from_str::<BlendMode>(&format!("\"{}\"", blend_mode))
        .map_err(|_| format!("Invalid blend mode '{}'", blend_mode))?;
    let args = serde_json::from_str(&args_json)
        .map_err(|e| format!("Failed to parse args_json: {}", e))?;
    Ok(TrackScore {
        id,
        track_id,
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

fn blend_mode_to_string(blend_mode: &BlendMode) -> String {
    match serde_json::to_string(blend_mode) {
        Ok(s) => s.trim_matches('"').to_string(),
        Err(_) => "replace".to_string(),
    }
}

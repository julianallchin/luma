use sqlx::SqlitePool;
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
    if !start.is_finite() || !end.is_finite() {
        return Err("Annotation times must be finite.".to_string());
    }
    let dur = end - start;
    if dur < min_dur {
        return Err(format!(
            "Annotation too short ({:.4}s). Minimum is 1/32 bar ({:.4}s).",
            dur, min_dur
        ));
    }
    Ok(())
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

/// Return the venue_id of the most-recently-updated score for a track that has
/// at least one annotation, if any. Used by previews that only receive a track_id.
pub async fn get_venue_for_track(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<String>, String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT s.venue_id
         FROM scores s
         JOIN track_scores ts ON ts.score_id = s.id
         WHERE s.track_id = ?
         GROUP BY s.id
         ORDER BY s.updated_at DESC
         LIMIT 1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to resolve venue for track: {}", e))?;
    Ok(row.map(|r| r.0))
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
    if payload.start_time.is_some_and(|value| !value.is_finite())
        || payload.end_time.is_some_and(|value| !value.is_finite())
    {
        return Err("Annotation times must be finite.".to_string());
    }

    let track_id: String = sqlx::query_scalar(
        "SELECT s.track_id FROM track_scores ts JOIN scores s ON ts.score_id = s.id WHERE ts.id = ?",
    )
    .bind(&payload.id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to resolve track for annotation: {}", e))?;
    let min_dur = min_annotation_duration(pool, &track_id).await;
    let blend_mode = payload.blend_mode.as_ref().map(blend_mode_to_string);
    let args_json = payload.args.as_ref().map(Value::to_string);

    // Apply only explicitly supplied fields to the row as it exists at write
    // time. A delayed timing drag can no longer restore stale args (or vice
    // versa) after another writer commits. Timing validation is part of the
    // same statement; untouched legacy-short rows remain editable losslessly.
    let result = sqlx::query(
        "UPDATE track_scores
         SET start_time = COALESCE(?1, start_time),
             end_time = COALESCE(?2, end_time),
             z_index = COALESCE(?3, z_index),
             blend_mode = COALESCE(?4, blend_mode),
             args_json = COALESCE(?5, args_json)
         WHERE id = ?6
           AND ((?1 IS NULL AND ?2 IS NULL)
                OR COALESCE(?2, end_time) - COALESCE(?1, start_time) >= ?7)",
    )
    .bind(payload.start_time)
    .bind(payload.end_time)
    .bind(payload.z_index)
    .bind(blend_mode)
    .bind(args_json)
    .bind(&payload.id)
    .bind(min_dur)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update track_score: {}", e))?;

    if result.rows_affected() == 0 {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM track_scores WHERE id = ?)")
                .bind(&payload.id)
                .fetch_one(pool)
                .await
                .map_err(|e| format!("Failed to verify track_score update: {e}"))?;
        return if exists {
            Err(format!(
                "Annotation too short. Minimum is 1/32 bar ({min_dur:.4}s)."
            ))
        } else {
            Err(format!("TrackScore {} not found", payload.id))
        };
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

/// List scores for a (track, venue) pair. An empty `venue_id` lists the
/// track's scores across ALL venues — used by the pattern editor, which runs
/// outside a venue route (each returned summary carries its own `venue_id`).
pub async fn list_scores_for_track(
    pool: &SqlitePool,
    track_id: &str,
    venue_id: &str,
) -> Result<Vec<ScoreSummary>, String> {
    const ALL_VENUES: &str = "SELECT s.id, s.uid, s.venue_id, s.name,
                COUNT(ts.id) AS annotation_count,
                s.created_at, s.updated_at
         FROM scores s
         LEFT JOIN track_scores ts ON ts.score_id = s.id
         WHERE s.track_id = ?
         GROUP BY s.id
         ORDER BY s.updated_at DESC";
    const ONE_VENUE: &str = "SELECT s.id, s.uid, s.venue_id, s.name,
                COUNT(ts.id) AS annotation_count,
                s.created_at, s.updated_at
         FROM scores s
         LEFT JOIN track_scores ts ON ts.score_id = s.id
         WHERE s.track_id = ? AND s.venue_id = ?
         GROUP BY s.id
         ORDER BY s.updated_at DESC";
    let query = if venue_id.is_empty() {
        sqlx::query_as::<_, ScoreSummary>(ALL_VENUES).bind(track_id)
    } else {
        sqlx::query_as::<_, ScoreSummary>(ONE_VENUE)
            .bind(track_id)
            .bind(venue_id)
    };
    query
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

#[cfg(test)]
mod update_track_score_tests {
    use super::update_track_score;
    use crate::models::scores::UpdateTrackScoreInput;
    use serde_json::{json, Value};
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE scores (id TEXT PRIMARY KEY, track_id TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE track_scores (
                id TEXT PRIMARY KEY,
                score_id TEXT NOT NULL,
                start_time REAL NOT NULL,
                end_time REAL NOT NULL,
                z_index INTEGER NOT NULL,
                blend_mode TEXT NOT NULL,
                args_json TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO scores (id, track_id) VALUES ('score', 'track')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track_scores
             (id, score_id, start_time, end_time, z_index, blend_mode, args_json)
             VALUES ('clip', 'score', 0.0, 1.0, 3, 'replace', '{\"color\":\"red\"}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn update() -> UpdateTrackScoreInput {
        UpdateTrackScoreInput {
            id: "clip".to_string(),
            start_time: None,
            end_time: None,
            z_index: None,
            blend_mode: None,
            args: None,
        }
    }

    #[tokio::test]
    async fn delayed_partial_updates_preserve_each_others_fields() {
        let pool = pool().await;
        let timing = UpdateTrackScoreInput {
            start_time: Some(2.0),
            end_time: Some(4.0),
            ..update()
        };
        let args = UpdateTrackScoreInput {
            args: Some(json!({ "color": "blue" })),
            ..update()
        };

        let (timing_result, args_result) = tokio::join!(
            update_track_score(&pool, timing),
            update_track_score(&pool, args)
        );
        timing_result.unwrap();
        args_result.unwrap();

        let (start, end, z_index, args_json): (f64, f64, i64, String) = sqlx::query_as(
            "SELECT start_time, end_time, z_index, args_json FROM track_scores WHERE id = 'clip'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((start, end, z_index), (2.0, 4.0, 3));
        assert_eq!(
            serde_json::from_str::<Value>(&args_json).unwrap(),
            json!({ "color": "blue" })
        );
    }

    #[tokio::test]
    async fn touched_invalid_timing_is_rejected_without_mutating_the_row() {
        let pool = pool().await;
        let invalid = UpdateTrackScoreInput {
            start_time: Some(0.99),
            ..update()
        };

        let error = update_track_score(&pool, invalid).await.unwrap_err();
        assert!(error.contains("Annotation too short"));

        let timing: (f64, f64) =
            sqlx::query_as("SELECT start_time, end_time FROM track_scores WHERE id = 'clip'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(timing, (0.0, 1.0));
    }
}

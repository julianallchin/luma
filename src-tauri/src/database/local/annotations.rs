use sqlx::FromRow;
use tauri::State;

use crate::database::Db;
use crate::models::annotations::{CreateAnnotationInput, TrackAnnotation, UpdateAnnotationInput};
use crate::models::schema::BlendMode;

#[derive(FromRow)]
struct AnnotationRow {
    id: i64,
    track_id: i64,
    pattern_id: i64,
    start_time: f64,
    end_time: f64,
    z_index: i64,
    blend_mode: String,
    args_json: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<AnnotationRow> for TrackAnnotation {
    fn from(row: AnnotationRow) -> Self {
        // Parse blend_mode from string, default to Replace if invalid
        let blend_mode = serde_json::from_str::<BlendMode>(&format!("\"{}\"", row.blend_mode))
            .unwrap_or(BlendMode::Replace);

        TrackAnnotation {
            id: row.id,
            track_id: row.track_id,
            pattern_id: row.pattern_id,
            start_time: row.start_time,
            end_time: row.end_time,
            z_index: row.z_index,
            blend_mode,
            args: row
                .args_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_else(|| serde_json::json!({})),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

fn blend_mode_to_string(blend_mode: &BlendMode) -> String {
    // Convert BlendMode to the string format stored in database (camelCase)
    match serde_json::to_string(blend_mode) {
        Ok(s) => s.trim_matches('"').to_string(),
        Err(_) => "replace".to_string(),
    }
}

async fn ensure_default_score(db: &sqlx::SqlitePool, track_id: i64) -> Result<i64, String> {
    if let Some((id,)) =
        sqlx::query_as::<_, (i64,)>("SELECT id FROM scores WHERE track_id = ? ORDER BY id LIMIT 1")
            .bind(track_id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("Failed to fetch score: {}", e))?
    {
        return Ok(id);
    }

    let result = sqlx::query("INSERT INTO scores (track_id, name) VALUES (?, ?)")
        .bind(track_id)
        .bind("main")
        .execute(db)
        .await
        .map_err(|e| format!("Failed to create default score: {}", e))?;

    Ok(result.last_insert_rowid())
}

/// Core: list all annotations for a track
pub async fn get_annotations_for_track(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> Result<Vec<TrackAnnotation>, String> {
    let rows = sqlx::query_as::<_, AnnotationRow>(
        "SELECT ts.id, s.track_id, ts.pattern_id, ts.start_time, ts.end_time, ts.z_index, ts.blend_mode, ts.args_json, ts.created_at, ts.updated_at
         FROM track_scores ts
         JOIN scores s ON ts.score_id = s.id
         WHERE s.track_id = ?
         ORDER BY ts.z_index ASC, ts.start_time ASC",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list annotations: {}", e))?;

    Ok(rows.into_iter().map(Into::into).collect())
}

/// Tauri: list all annotations for a track
#[tauri::command]
pub async fn list_annotations(
    db: State<'_, Db>,
    track_id: i64,
) -> Result<Vec<TrackAnnotation>, String> {
    get_annotations_for_track(&db.0, track_id).await
}

/// Core: create a new annotation
pub async fn create_annotation_record(
    pool: &sqlx::SqlitePool,
    input: CreateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    // Validate times
    if input.start_time >= input.end_time {
        return Err("Start time must be less than end time".into());
    }
    if input.start_time < 0.0 {
        return Err("Start time must be non-negative".into());
    }

    let blend_mode = input.blend_mode.unwrap_or(BlendMode::Replace);
    let blend_mode_str = blend_mode_to_string(&blend_mode);
    let args_json = input
        .args
        .unwrap_or_else(|| serde_json::json!({}))
        .to_string();

    let score_id = ensure_default_score(pool, input.track_id).await?;

    let result = sqlx::query(
        "INSERT INTO track_scores (score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(score_id)
    .bind(input.pattern_id)
    .bind(input.start_time)
    .bind(input.end_time)
    .bind(input.z_index)
    .bind(blend_mode_str)
    .bind(args_json)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create annotation: {}", e))?;

    let id = result.last_insert_rowid();

    let row = sqlx::query_as::<_, AnnotationRow>(
        "SELECT ts.id, s.track_id, ts.pattern_id, ts.start_time, ts.end_time, ts.z_index, ts.blend_mode, ts.args_json, ts.created_at, ts.updated_at
         FROM track_scores ts
         JOIN scores s ON ts.score_id = s.id
         WHERE ts.id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch created annotation: {}", e))?;

    Ok(row.into())
}

/// Tauri: create a new annotation
#[tauri::command]
pub async fn create_annotation(
    db: State<'_, Db>,
    input: CreateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    create_annotation_record(&db.0, input).await
}

/// Core: update an existing annotation
pub async fn update_annotation_record(
    pool: &sqlx::SqlitePool,
    input: UpdateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    // Fetch existing annotation to merge with updates
    let existing = sqlx::query_as::<_, AnnotationRow>(
        "SELECT ts.id, s.track_id, ts.pattern_id, ts.start_time, ts.end_time, ts.z_index, ts.blend_mode, ts.args_json, ts.created_at, ts.updated_at
         FROM track_scores ts
         JOIN scores s ON ts.score_id = s.id
         WHERE ts.id = ?",
    )
    .bind(input.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch annotation: {}", e))?
    .ok_or_else(|| format!("Annotation {} not found", input.id))?;

    let start_time = input.start_time.unwrap_or(existing.start_time);
    let end_time = input.end_time.unwrap_or(existing.end_time);
    let z_index = input.z_index.unwrap_or(existing.z_index);

    // Handle blend_mode update
    let blend_mode_str = if let Some(blend_mode) = input.blend_mode {
        blend_mode_to_string(&blend_mode)
    } else {
        existing.blend_mode
    };

    let args_json = if let Some(args) = input.args {
        args.to_string()
    } else {
        existing.args_json.unwrap_or_else(|| "{}".into())
    };

    // Validate times
    if start_time >= end_time {
        return Err("Start time must be less than end time".into());
    }
    if start_time < 0.0 {
        return Err("Start time must be non-negative".into());
    }

    sqlx::query(
        "UPDATE track_scores
         SET start_time = ?, end_time = ?, z_index = ?, blend_mode = ?, args_json = ?, updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(start_time)
    .bind(end_time)
    .bind(z_index)
    .bind(blend_mode_str)
    .bind(args_json)
    .bind(input.id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update annotation: {}", e))?;

    let row = sqlx::query_as::<_, AnnotationRow>(
        "SELECT ts.id, s.track_id, ts.pattern_id, ts.start_time, ts.end_time, ts.z_index, ts.blend_mode, ts.args_json, ts.created_at, ts.updated_at
         FROM track_scores ts
         JOIN scores s ON ts.score_id = s.id
         WHERE ts.id = ?",
    )
    .bind(input.id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch updated annotation: {}", e))?;

    Ok(row.into())
}

/// Tauri: update an existing annotation
#[tauri::command]
pub async fn update_annotation(
    db: State<'_, Db>,
    input: UpdateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    update_annotation_record(&db.0, input).await
}

/// Core: delete an annotation
pub async fn delete_annotation_record(
    pool: &sqlx::SqlitePool,
    annotation_id: i64,
) -> Result<(), String> {
    sqlx::query("DELETE FROM track_scores WHERE id = ?")
        .bind(annotation_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete annotation: {}", e))?;

    Ok(())
}

/// Tauri: delete an annotation
#[tauri::command]
pub async fn delete_annotation(db: State<'_, Db>, annotation_id: i64) -> Result<(), String> {
    delete_annotation_record(&db.0, annotation_id).await
}

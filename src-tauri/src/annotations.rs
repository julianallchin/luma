use sqlx::FromRow;
use tauri::State;

use crate::database::Db;
use crate::models::annotations::{CreateAnnotationInput, TrackAnnotation, UpdateAnnotationInput};

#[derive(FromRow)]
struct AnnotationRow {
    id: i64,
    track_id: i64,
    pattern_id: i64,
    start_time: f64,
    end_time: f64,
    z_index: i64,
    created_at: String,
    updated_at: String,
}

impl From<AnnotationRow> for TrackAnnotation {
    fn from(row: AnnotationRow) -> Self {
        TrackAnnotation {
            id: row.id,
            track_id: row.track_id,
            pattern_id: row.pattern_id,
            start_time: row.start_time,
            end_time: row.end_time,
            z_index: row.z_index,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// List all annotations for a track
#[tauri::command]
pub async fn list_annotations(
    db: State<'_, Db>,
    track_id: i64,
) -> Result<Vec<TrackAnnotation>, String> {
    let rows = sqlx::query_as::<_, AnnotationRow>(
        "SELECT id, track_id, pattern_id, start_time, end_time, z_index, created_at, updated_at 
         FROM track_annotations 
         WHERE track_id = ? 
         ORDER BY z_index ASC, start_time ASC",
    )
    .bind(track_id)
    .fetch_all(&db.0)
    .await
    .map_err(|e| format!("Failed to list annotations: {}", e))?;

    Ok(rows.into_iter().map(Into::into).collect())
}

/// Create a new annotation
#[tauri::command]
pub async fn create_annotation(
    db: State<'_, Db>,
    input: CreateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    // Validate times
    if input.start_time >= input.end_time {
        return Err("Start time must be less than end time".into());
    }
    if input.start_time < 0.0 {
        return Err("Start time must be non-negative".into());
    }

    let result = sqlx::query(
        "INSERT INTO track_annotations (track_id, pattern_id, start_time, end_time, z_index)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(input.track_id)
    .bind(input.pattern_id)
    .bind(input.start_time)
    .bind(input.end_time)
    .bind(input.z_index)
    .execute(&db.0)
    .await
    .map_err(|e| format!("Failed to create annotation: {}", e))?;

    let id = result.last_insert_rowid();

    let row = sqlx::query_as::<_, AnnotationRow>(
        "SELECT id, track_id, pattern_id, start_time, end_time, z_index, created_at, updated_at 
         FROM track_annotations WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch created annotation: {}", e))?;

    Ok(row.into())
}

/// Update an existing annotation
#[tauri::command]
pub async fn update_annotation(
    db: State<'_, Db>,
    input: UpdateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    // Fetch existing annotation to merge with updates
    let existing = sqlx::query_as::<_, AnnotationRow>(
        "SELECT id, track_id, pattern_id, start_time, end_time, z_index, created_at, updated_at 
         FROM track_annotations WHERE id = ?",
    )
    .bind(input.id)
    .fetch_optional(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch annotation: {}", e))?
    .ok_or_else(|| format!("Annotation {} not found", input.id))?;

    let start_time = input.start_time.unwrap_or(existing.start_time);
    let end_time = input.end_time.unwrap_or(existing.end_time);
    let z_index = input.z_index.unwrap_or(existing.z_index);

    // Validate times
    if start_time >= end_time {
        return Err("Start time must be less than end time".into());
    }
    if start_time < 0.0 {
        return Err("Start time must be non-negative".into());
    }

    sqlx::query(
        "UPDATE track_annotations 
         SET start_time = ?, end_time = ?, z_index = ?, updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(start_time)
    .bind(end_time)
    .bind(z_index)
    .bind(input.id)
    .execute(&db.0)
    .await
    .map_err(|e| format!("Failed to update annotation: {}", e))?;

    let row = sqlx::query_as::<_, AnnotationRow>(
        "SELECT id, track_id, pattern_id, start_time, end_time, z_index, created_at, updated_at 
         FROM track_annotations WHERE id = ?",
    )
    .bind(input.id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch updated annotation: {}", e))?;

    Ok(row.into())
}

/// Delete an annotation
#[tauri::command]
pub async fn delete_annotation(db: State<'_, Db>, annotation_id: i64) -> Result<(), String> {
    let result = sqlx::query("DELETE FROM track_annotations WHERE id = ?")
        .bind(annotation_id)
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to delete annotation: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("Annotation {} not found", annotation_id));
    }

    Ok(())
}

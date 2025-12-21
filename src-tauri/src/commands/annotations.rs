//! Tauri commands for annotation operations

use tauri::State;

use crate::database::local::annotations as db;
use crate::database::Db;
use crate::models::annotations::{CreateAnnotationInput, TrackAnnotation, UpdateAnnotationInput};

#[tauri::command]
pub async fn list_annotations(
    db: State<'_, Db>,
    track_id: i64,
) -> Result<Vec<TrackAnnotation>, String> {
    db::get_annotations_for_track(&db.0, track_id).await
}

#[tauri::command]
pub async fn create_annotation(
    db: State<'_, Db>,
    input: CreateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    db::create_annotation_record(&db.0, input).await
}

#[tauri::command]
pub async fn update_annotation(
    db: State<'_, Db>,
    input: UpdateAnnotationInput,
) -> Result<TrackAnnotation, String> {
    db::update_annotation_record(&db.0, input).await
}

#[tauri::command]
pub async fn delete_annotation(
    db: State<'_, Db>,
    annotation_id: i64,
) -> Result<(), String> {
    db::delete_annotation_record(&db.0, annotation_id).await
}

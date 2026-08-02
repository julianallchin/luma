//! Tauri commands for stage piece operations.

use tauri::State;

use crate::database::local::stage as stage_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::Db;
use crate::models::stage::StagePiece;

#[tauri::command]
pub async fn list_stage_pieces(
    db: State<'_, Db>,
    venue_id: String,
) -> Result<Vec<StagePiece>, String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    stage_db::get_stage_pieces(&mut access).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn place_stage_piece(
    db: State<'_, Db>,
    venue_id: String,
    mesh_path: String,
    kind: String,
    parent_piece_id: Option<String>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
    scale: Option<f64>,
    label: Option<String>,
) -> Result<StagePiece, String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    let piece = stage_db::insert_stage_piece(
        &mut access,
        &mesh_path,
        &kind,
        parent_piece_id.as_deref(),
        pos_x,
        pos_y,
        pos_z,
        rot_x,
        rot_y,
        rot_z,
        scale.unwrap_or(1.0),
        label.as_deref(),
    )
    .await?;
    access.commit().await?;
    Ok(piece)
}

/// Move + (re)parent a piece in a single atomic update. Pass
/// `parent_piece_id = None` to detach. pos/rot are interpreted in the
/// resulting parent's local space (or world space if detached).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn move_stage_piece(
    db: State<'_, Db>,
    id: String,
    parent_piece_id: Option<String>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::StagePiece(&id)).await?;
    stage_db::update_stage_piece_pose(
        &mut access,
        &id,
        parent_piece_id.as_deref(),
        pos_x,
        pos_y,
        pos_z,
        rot_x,
        rot_y,
        rot_z,
    )
    .await?;
    access.commit().await
}

#[tauri::command]
pub async fn rename_stage_piece(
    db: State<'_, Db>,
    id: String,
    label: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::StagePiece(&id)).await?;
    stage_db::update_stage_piece_label(&mut access, &id, &label).await?;
    access.commit().await
}

#[tauri::command]
pub async fn delete_stage_piece(db: State<'_, Db>, id: String) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::StagePiece(&id)).await?;
    stage_db::delete_stage_piece(&mut access, &id).await?;
    access.commit().await
}

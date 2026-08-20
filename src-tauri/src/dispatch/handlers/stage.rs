//! Stage pieces — the physical scenery a venue is built from.
//!
//! Pose fields are parent-local when `parent_piece_id` is set and world-space
//! otherwise; the tree itself lives only in `parent_piece_id`, so these
//! handlers deal in flat rows.

use crate::database::local::stage as stage_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::{AppServices, CommandError};
use crate::models::stage::StagePiece;

/// Flat list for one venue; the tree lives in `parent_piece_id`.
pub async fn list_stage_pieces(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<StagePiece>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(stage_db::get_stage_pieces(&mut access).await?)
}

/// Insert one piece. `scale` defaults to 1.0; pose is parent-local when
/// `parent_piece_id` is set.
#[allow(clippy::too_many_arguments)]
pub async fn place_stage_piece(
    services: &AppServices,
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
) -> Result<StagePiece, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
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
#[allow(clippy::too_many_arguments)]
pub async fn move_stage_piece(
    services: &AppServices,
    id: String,
    parent_piece_id: Option<String>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::StagePiece(&id)).await?;
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
    Ok(access.commit().await?)
}

/// `label` is non-nullable, so there is no way to clear it back to null.
pub async fn rename_stage_piece(
    services: &AppServices,
    id: String,
    label: String,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::StagePiece(&id)).await?;
    stage_db::update_stage_piece_label(&mut access, &id, &label).await?;
    Ok(access.commit().await?)
}

/// Deletes the piece's entire descendant subtree — `parent_piece_id` is
/// `ON DELETE CASCADE`. The write scope is the named piece only; descendants
/// go with it without a per-resource authorization check of their own.
pub async fn delete_stage_piece(services: &AppServices, id: String) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::StagePiece(&id)).await?;
    stage_db::delete_stage_piece(&mut access, &id).await?;
    Ok(access.commit().await?)
}

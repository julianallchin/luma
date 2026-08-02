use uuid::Uuid;

use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, Write};
use crate::models::stage::StagePiece;

// -----------------------------------------------------------------------------
// Inserts / Updates / Deletes
// -----------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn insert_stage_piece(
    access: &mut VenueAccess<'_, Write>,
    mesh_path: &str,
    kind: &str,
    parent_piece_id: Option<&str>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
    scale: f64,
    label: Option<&str>,
) -> Result<StagePiece, String> {
    let id = Uuid::new_v4().to_string();
    let venue_id = access.venue_id().to_string();
    let principal = access.principal().map(str::to_owned);
    ensure_parent_in_venue(access, parent_piece_id).await?;

    sqlx::query(
        "INSERT INTO stage_pieces (id, uid, venue_id, mesh_path, kind, parent_piece_id, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(principal)
    .bind(&venue_id)
    .bind(mesh_path)
    .bind(kind)
    .bind(parent_piece_id)
    .bind(label)
    .bind(pos_x)
    .bind(pos_y)
    .bind(pos_z)
    .bind(rot_x)
    .bind(rot_y)
    .bind(rot_z)
    .bind(scale)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to insert stage piece: {e}"))?;

    sqlx::query_as::<_, StagePiece>(
        "SELECT id, uid, venue_id, mesh_path, kind, label, pos_x, pos_y, pos_z,
                rot_x, rot_y, rot_z, scale, parent_piece_id
         FROM stage_pieces WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to read inserted stage piece: {error}"))
}

/// Update the piece's transform AND its parent. Pass `parent_piece_id = None`
/// to detach from the current parent (pos/rot become world-space again).
#[allow(clippy::too_many_arguments)]
pub async fn update_stage_piece_pose(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    parent_piece_id: Option<&str>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<u64, String> {
    ensure_parent_in_venue(access, parent_piece_id).await?;
    let result = sqlx::query(
        "UPDATE stage_pieces SET parent_piece_id = ?, pos_x = ?, pos_y = ?, pos_z = ?, rot_x = ?, rot_y = ?, rot_z = ? WHERE id = ?",
    )
    .bind(parent_piece_id)
    .bind(pos_x)
    .bind(pos_y)
    .bind(pos_z)
    .bind(rot_x)
    .bind(rot_y)
    .bind(rot_z)
    .bind(id)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to update stage piece pose: {e}"))?;
    Ok(result.rows_affected())
}

async fn ensure_parent_in_venue(
    access: &mut VenueAccess<'_, Write>,
    parent_piece_id: Option<&str>,
) -> Result<(), String> {
    let Some(parent_piece_id) = parent_piece_id else {
        return Ok(());
    };
    let venue_id = access.venue_id().to_string();
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM stage_pieces WHERE id = ? AND venue_id = ?
         )",
    )
    .bind(parent_piece_id)
    .bind(venue_id)
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to validate stage parent: {error}"))?;
    if exists != 1 {
        return Err("Stage parent is not available in this venue".into());
    }
    Ok(())
}

pub async fn update_stage_piece_label(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    label: &str,
) -> Result<u64, String> {
    let result = sqlx::query("UPDATE stage_pieces SET label = ? WHERE id = ?")
        .bind(label)
        .bind(id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to rename stage piece: {e}"))?;
    Ok(result.rows_affected())
}

pub async fn delete_stage_piece(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
) -> Result<(), String> {
    sqlx::query("DELETE FROM stage_pieces WHERE id = ?")
        .bind(id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to delete stage piece: {e}"))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Queries
// -----------------------------------------------------------------------------

pub async fn get_stage_pieces(
    access: &mut VenueAccess<'_, Read>,
) -> Result<Vec<StagePiece>, String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query_as::<_, StagePiece>(
        "SELECT id, uid, venue_id, mesh_path, kind, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale, parent_piece_id
         FROM stage_pieces WHERE venue_id = ? ORDER BY created_at ASC",
    )
    .bind(venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to get stage pieces: {e}"))
}

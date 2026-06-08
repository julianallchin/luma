use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::stage::StagePiece;

// -----------------------------------------------------------------------------
// Inserts / Updates / Deletes
// -----------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn insert_stage_piece(
    pool: &SqlitePool,
    venue_id: &str,
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

    sqlx::query(
        "INSERT INTO stage_pieces (id, uid, venue_id, mesh_path, kind, parent_piece_id, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale)
         VALUES (?, (SELECT uid FROM venues WHERE id = ?), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(venue_id)
    .bind(venue_id)
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
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to insert stage piece: {e}"))?;

    Ok(StagePiece {
        id,
        uid: None,
        venue_id: venue_id.to_string(),
        mesh_path: mesh_path.to_string(),
        kind: kind.to_string(),
        label: label.map(|s| s.to_string()),
        pos_x,
        pos_y,
        pos_z,
        rot_x,
        rot_y,
        rot_z,
        scale,
        parent_piece_id: parent_piece_id.map(|s| s.to_string()),
    })
}

/// Update the piece's transform AND its parent. Pass `parent_piece_id = None`
/// to detach from the current parent (pos/rot become world-space again).
#[allow(clippy::too_many_arguments)]
pub async fn update_stage_piece_pose(
    pool: &SqlitePool,
    id: &str,
    parent_piece_id: Option<&str>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<u64, String> {
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
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update stage piece pose: {e}"))?;
    Ok(result.rows_affected())
}

pub async fn update_stage_piece_label(
    pool: &SqlitePool,
    id: &str,
    label: &str,
) -> Result<u64, String> {
    let result = sqlx::query("UPDATE stage_pieces SET label = ? WHERE id = ?")
        .bind(label)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to rename stage piece: {e}"))?;
    Ok(result.rows_affected())
}

pub async fn delete_stage_piece(pool: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM stage_pieces WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete stage piece: {e}"))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Queries
// -----------------------------------------------------------------------------

pub async fn get_stage_pieces(
    pool: &SqlitePool,
    venue_id: &str,
) -> Result<Vec<StagePiece>, String> {
    sqlx::query_as::<_, StagePiece>(
        "SELECT id, uid, venue_id, mesh_path, kind, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale, parent_piece_id
         FROM stage_pieces WHERE venue_id = ? ORDER BY created_at ASC",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get stage pieces: {e}"))
}

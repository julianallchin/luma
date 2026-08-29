//! The `stage_pieces` table, read-only.
//!
//! Superseded by the venue graph (`docs/design/venue-graph.md`, phase 3). The
//! only reader left is [`crate::venue_graph::migrate`], which converts these
//! rows into nodes and edges the first time a venue is opened; the writers are
//! gone, so nothing adds to what it has to convert.
//!
//! The table itself is not dropped here — see the header of
//! `migrations/20260829000000_venue_graph.sql`.

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::stage::StagePiece;

/// One venue's pieces in creation order, which is the order the old flattening
/// walked and therefore the order the conversion reproduces.
///
/// # Errors
/// Fails if `stage_pieces` cannot be read.
pub async fn get_stage_pieces(
    access: &mut impl AuthorizedVenue,
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

//! A venue's Perform set: its patterns (`perform_patterns`) and the layout
//! that plays them (`perform_layouts`).
//!
//! Pattern ids are the caller's: a new pattern and the cell that holds it
//! are made together, so the client names the pattern before it is stored.

use crate::database::local::deletes;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::perform::{PerformLayout, PerformPattern};

type Row = (String, String, String, i64, String, String, String);

/// The venue's Perform patterns, the top of the set first.
pub async fn list(access: &mut impl AuthorizedVenue) -> Result<Vec<PerformPattern>, String> {
    let venue_id = access.venue_id().to_owned();
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, name, selection_json, z_index, blend_mode, seed, graph_json
         FROM perform_patterns WHERE venue_id = ? ORDER BY z_index DESC, created_at, id",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to list Perform patterns: {error}"))?;
    rows.into_iter()
        .map(|(id, name, selection, z_index, blend_mode, seed, graph)| {
            Ok(PerformPattern {
                id,
                venue_id: venue_id.clone(),
                name,
                selection: from_json(&selection)?,
                z_index,
                blend_mode: from_json(&format!("\"{blend_mode}\""))?,
                seed: seed
                    .parse()
                    .map_err(|_| format!("pattern seed `{seed}` is not a u64"))?,
                graph: from_json(&graph)?,
            })
        })
        .collect()
}

/// Store `pattern` in this venue, new or replacing the one with its id.
/// Answers it as stored, or `None` when its id is another venue's.
pub async fn save(
    access: &mut VenueAccess<'_, Write>,
    pattern: PerformPattern,
) -> Result<Option<PerformPattern>, String> {
    let pattern = PerformPattern {
        venue_id: access.venue_id().to_owned(),
        ..pattern
    };
    let uid = access.principal().map(str::to_owned);
    let done = sqlx::query(
        "INSERT INTO perform_patterns (id, uid, venue_id, name, selection_json, z_index,
             blend_mode, seed, graph_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET name = excluded.name,
             selection_json = excluded.selection_json, z_index = excluded.z_index,
             blend_mode = excluded.blend_mode, seed = excluded.seed,
             graph_json = excluded.graph_json
         WHERE perform_patterns.venue_id = excluded.venue_id",
    )
    .bind(&pattern.id)
    .bind(uid)
    .bind(&pattern.venue_id)
    .bind(&pattern.name)
    .bind(json(&pattern.selection)?)
    .bind(pattern.z_index)
    .bind(pattern.blend_mode.name())
    .bind(pattern.seed.to_string())
    .bind(json(&pattern.graph)?)
    .execute(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to save Perform pattern: {error}"))?;
    Ok((done.rows_affected() == 1).then_some(pattern))
}

/// Answers how many patterns it deleted: one, or none when `id` is not this
/// venue's.
pub async fn delete(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    deletes::delete_where(
        access.connection(),
        "perform_patterns",
        "id = ? AND venue_id = ?",
        &[id, &venue_id],
    )
    .await
    .map(|deleted| deleted as u64)
}

/// The venue's layout, or `None` before one is saved.
pub async fn layout(access: &mut impl AuthorizedVenue) -> Result<Option<PerformLayout>, String> {
    let venue_id = access.venue_id().to_owned();
    let stored: Option<String> =
        sqlx::query_scalar("SELECT layout_json FROM perform_layouts WHERE id = ?")
            .bind(&venue_id)
            .fetch_optional(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to read the Perform layout: {error}"))?;
    stored.as_deref().map(from_json).transpose()
}

/// Replace the venue's layout.
pub async fn save_layout(
    access: &mut VenueAccess<'_, Write>,
    layout: &PerformLayout,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_owned();
    let uid = access.principal().map(str::to_owned);
    sqlx::query(
        "INSERT INTO perform_layouts (id, uid, venue_id, layout_json) VALUES (?1, ?2, ?1, ?3)
         ON CONFLICT (id) DO UPDATE SET layout_json = excluded.layout_json",
    )
    .bind(&venue_id)
    .bind(uid)
    .bind(json(layout)?)
    .execute(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to save the Perform layout: {error}"))?;
    Ok(())
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

fn from_json<T: serde::de::DeserializeOwned>(source: &str) -> Result<T, String> {
    serde_json::from_str(source).map_err(|error| format!("unreadable Perform set: {error}"))
}

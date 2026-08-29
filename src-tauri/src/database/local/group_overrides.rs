//! `fixture_group_overrides` — the manual edits sitting on top of derivation.
//!
//! Rows in, rows out. What an override *means* is
//! [`crate::services::group_derivation`]'s business; this module only knows
//! that a row exists for a derived group id and what it holds.

use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};

/// One row: a derived node someone named.
///
/// Every optional column is one edit verb. The row freezes the node's
/// *identity* — its label and where it hangs — and never its membership; see
/// the migration header.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GroupOverride {
    /// The derived id, `derive_groups`'s hash of the venue and the path.
    pub group_id: String,
    /// The derivation path this override froze, `/`-joined.
    pub path: String,
    /// Rename; `None` keeps the derived label.
    pub label: Option<String>,
    /// Move; `None` keeps the derived parent, `Some("")` means the top level.
    pub parent_id: Option<String>,
    /// Merge; the group its fixtures are counted under instead.
    pub merged_into: Option<String>,
}

/// Every override in the venue, in path order.
///
/// # Errors
/// Fails if the table cannot be read.
pub async fn list(access: &mut impl AuthorizedVenue) -> Result<Vec<GroupOverride>, String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query_as::<_, GroupOverride>(
        "SELECT group_id, path, label, parent_id, merged_into
         FROM fixture_group_overrides WHERE venue_id = ? ORDER BY path ASC, group_id ASC",
    )
    .bind(venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to read group overrides: {e}"))
}

/// Write one override, replacing whatever was there.
///
/// Upsert rather than insert-or-update at the call site: "touched" is one bit
/// and a caller should not have to ask whether it was already set.
///
/// # Errors
/// Fails if the write is refused — most often by write admission.
pub async fn put(
    access: &mut VenueAccess<'_, Write>,
    group_id: &str,
    path: &str,
    label: Option<&str>,
    parent_id: Option<&str>,
    merged_into: Option<&str>,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query(
        "INSERT INTO fixture_group_overrides
             (group_id, venue_id, path, label, parent_id, merged_into)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(group_id) DO UPDATE SET
             path = excluded.path,
             label = COALESCE(excluded.label, fixture_group_overrides.label),
             parent_id = COALESCE(excluded.parent_id, fixture_group_overrides.parent_id),
             merged_into = COALESCE(excluded.merged_into, fixture_group_overrides.merged_into),
             version = fixture_group_overrides.version",
    )
    .bind(group_id)
    .bind(venue_id)
    .bind(path)
    .bind(label)
    .bind(parent_id)
    .bind(merged_into)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to write the group override: {e}"))?;
    Ok(())
}

/// Drop an override, restoring derivation for that node.
///
/// # Errors
/// Fails if the delete is refused.
pub async fn remove(access: &mut VenueAccess<'_, Write>, group_id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_string();
    Ok(
        sqlx::query("DELETE FROM fixture_group_overrides WHERE group_id = ? AND venue_id = ?")
            .bind(group_id)
            .bind(venue_id)
            .execute(&mut *access.connection())
            .await
            .map_err(|e| format!("Failed to delete the group override: {e}"))?
            .rows_affected(),
    )
}

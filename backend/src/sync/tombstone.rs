//! The deletion fact.
//!
//! A local delete removes the row, which is what keeps every read path in the
//! app correct without a single `WHERE deleted_at IS NULL`. What the row can no
//! longer say — "this identity was deleted here, and the server has not been
//! told" — is recorded here instead.
//!
//! This is not a queue. There is no payload, no operation type and no retry
//! state, at most one entry per identity, and an entry is dropped the moment
//! the remote accepts it or the row comes back. See `docs/design/sync-push-v2.md`.

use sqlx::{SqliteConnection, SqlitePool};

use super::error::SyncError;

/// One deletion the server has not been told about.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Tombstone {
    pub table_name: String,
    pub record_id: String,
    /// Whether push may attempt it now. A refused tombstone gets the same
    /// backoff any other failure does; a permanent one is never attempted
    /// again. It is still listed, because whether the row came back is a
    /// question about the tables, not about the retry budget.
    pub ready: bool,
}

/// Record the deletion of `record_id` in `table_name`, if the delete is one the
/// server should hear about.
///
/// Returns whether an entry now exists. `false` is normal and not an error: the
/// row is a member's cached copy (`origin = 'remote'`), or the delete is running
/// under maintenance (the sign-out projection wipe, an authored archive) or
/// under remote writes (a tombstone arriving from the server), none of which are
/// this device deleting something of its own. The predicate is copied from the
/// `sync_delete_*` triggers this replaced, so the set of deletes that propagate
/// is unchanged.
///
/// Must run in the same transaction as the `DELETE`; the
/// `guard_unrecorded_delete_*` triggers refuse the delete otherwise.
pub async fn record(
    connection: &mut SqliteConnection,
    table_name: &str,
    record_id: &str,
    pk_values: &[String],
) -> Result<bool, SyncError> {
    let Some(table) = super::registry::get_table(table_name) else {
        return Err(SyncError::Parse(format!(
            "table {table_name:?} is not registered for relational sync"
        )));
    };
    if !super::registry::has_remote_tombstone(table_name) {
        return Ok(false);
    }
    let sql = format!(
        "INSERT INTO sync_tombstones (principal_key, table_name, record_id)
         SELECT CASE WHEN admission.active_uid IS NULL
                     THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
                ?, ?
         FROM auth_write_admission AS admission
         WHERE admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND EXISTS (SELECT 1 FROM {} WHERE {} AND origin = 'local')
         ON CONFLICT(principal_key, table_name, record_id)
         DO UPDATE SET deleted_at = CURRENT_TIMESTAMP",
        table.name,
        table.pk_where()
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(table_name)
        .bind(record_id);
    for value in pk_values {
        query = query.bind(value);
    }
    Ok(query.execute(connection).await?.rows_affected() == 1)
}

/// Every deletion this principal still owes the server, in the order push
/// should deliver them, each marked with whether it may be attempted now.
pub async fn pending(pool: &SqlitePool, principal_key: &str) -> Result<Vec<Tombstone>, SyncError> {
    let sql = format!(
        "SELECT tombstone.table_name, tombstone.record_id, {} AS ready
         FROM sync_tombstones AS tombstone
         JOIN auth_write_admission AS admission
           ON admission.singleton = 1 AND admission.armed = 1
          AND admission.accepting = 1 AND admission.maintenance = 0
          AND admission.remote_writes = 0
          AND tombstone.principal_key = CASE
                WHEN admission.active_uid IS NULL THEN 'signed-out'
                ELSE 'signed-in:' || admission.active_uid
              END
         LEFT JOIN sync_push_failures AS failure
                ON failure.principal_key = tombstone.principal_key
               AND failure.table_name = tombstone.table_name
               AND failure.record_id = tombstone.record_id
               AND failure.subject = 'tombstone'
         WHERE tombstone.principal_key = ?
         ORDER BY tombstone.deleted_at, tombstone.table_name, tombstone.record_id",
        super::push_state::TOMBSTONE_READY_PREDICATE
    );
    let mut rows = sqlx::query_as::<_, Tombstone>(sqlx::AssertSqlSafe(sql))
        .bind(principal_key)
        .fetch_all(pool)
        .await?;
    // Children before parents: a remote soft delete does not cascade, so the
    // server never sees a parent tombstoned while its children still look live.
    rows.sort_by_key(|tombstone| {
        std::cmp::Reverse(super::registry::topo_position(&tombstone.table_name).unwrap_or(0))
    });
    Ok(rows)
}

/// Forget a tombstone: the remote accepted it, or the row came back.
pub async fn clear(
    pool: &SqlitePool,
    principal_key: &str,
    table_name: &str,
    record_id: &str,
) -> Result<(), SyncError> {
    sqlx::query(
        "DELETE FROM sync_tombstones
         WHERE principal_key = ? AND table_name = ? AND record_id = ?",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether a delete of this identity is still waiting to be pushed. Pull asks,
/// so an incoming row cannot resurrect something deleted locally.
pub async fn exists(
    pool: &SqlitePool,
    principal_key: &str,
    table_name: &str,
    record_id: &str,
) -> Result<bool, SyncError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sync_tombstones
         WHERE principal_key = ? AND table_name = ? AND record_id = ?",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .fetch_optional(pool)
    .await?
    .is_some())
}

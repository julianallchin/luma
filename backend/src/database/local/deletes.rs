//! Deleting a row from a synced table.
//!
//! A delete is a delete: the row goes, its `ON DELETE CASCADE` children go with
//! it, and the change log records each one. Nothing is tombstoned — the sync
//! transport replicates the deletion itself.

use sqlx::SqliteConnection;

/// Delete every row of `table` matching `where_sql`, and answer how many.
///
/// `where_sql` is a fragment with `?` placeholders bound from `binds`, in
/// order. The table name is a caller-owned literal, never input.
///
/// # Errors
/// Local database failures.
pub async fn delete_where(
    connection: &mut SqliteConnection,
    table: &str,
    where_sql: &str,
    binds: &[&str],
) -> Result<usize, String> {
    let sql = format!("DELETE FROM {table} WHERE {where_sql}");
    let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    for bind in binds {
        statement = statement.bind(*bind);
    }
    let deleted = statement
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("Failed to delete from {table}: {error}"))?
        .rows_affected();
    Ok(deleted as usize)
}

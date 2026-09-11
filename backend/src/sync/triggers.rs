//! TEMP triggers that turn a local write into a PowerSync upload entry.
//!
//! These belong to application *writer* connections only. The SDK applies
//! downloads on its own rusqlite connection, which has no TEMP schema of its
//! own, so a downloaded row cannot re-enter the upload queue. The same is true
//! of the migration connection: schema changes are not user edits.
//!
//! `uid` is written into every entry. Postgres defaults it to `auth.uid()`, so
//! sending it is redundant for an insert — but an RLS `with check` on an
//! `UPDATE` is evaluated against the row as it will be, and a PATCH that never
//! mentions `uid` gives the policy nothing to check against on a row the client
//! believes it owns. Sending it makes ownership explicit in both directions and
//! costs one column.

use sqlx::SqliteConnection;

use super::schema::{BOOLEAN_COLUMNS, SYNCED_TABLES};

/// Install the upload-capture triggers for every synced table on `connection`.
///
/// Call this from the app pool's `after_connect` hook, next to the `changes`
/// log triggers. Both sets are TEMP and live exactly as long as the connection.
///
/// # Errors
///
/// If a trigger cannot be created — which means the table is missing, i.e. the
/// migration and [`SYNCED_TABLES`] have drifted.
pub async fn install_crud_triggers(connection: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    for (table, columns) in SYNCED_TABLES {
        for statement in table_triggers(table, columns) {
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
                .execute(&mut *connection)
                .await?;
        }
    }
    Ok(())
}

/// The three `CREATE TEMP TRIGGER` statements for one table.
///
/// An INSERT uploads the whole row as a `PUT`. An UPDATE uploads only the
/// columns whose value actually changed as a `PATCH` — the two row objects are
/// decomposed with `json_each` and joined on key, so "changed" is decided by
/// SQLite's own `IS NOT`, which treats NULL as a value rather than as unknown.
/// A DELETE uploads the id alone.
#[must_use]
pub fn table_triggers(table: &str, columns: &[&str]) -> [String; 3] {
    let new = row_object(columns, "NEW");
    let old = row_object(columns, "OLD");
    [
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{table}_insert
             AFTER INSERT ON main.{table}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data)
               VALUES ('PUT', NEW.id, '{table}', {new});
             END;"
        ),
        // The `HAVING` guard is load-bearing: `json_group_object` is an
        // aggregate, so the SELECT yields one row even when nothing changed,
        // and an empty PATCH would be an upload with no content.
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{table}_update
             AFTER UPDATE ON main.{table}
             WHEN {new} IS NOT {old}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data)
               SELECT 'PATCH', NEW.id, '{table}', json_group_object(n.key, CASE n.type
                        WHEN 'true' THEN json('true') WHEN 'false' THEN json('false')
                        ELSE n.value END)
                 FROM json_each({new}) AS n
                 JOIN json_each({old}) AS o ON o.key = n.key
                WHERE n.value IS NOT o.value
               HAVING COUNT(*) > 0;
             END;"
        ),
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{table}_delete
             AFTER DELETE ON main.{table}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data)
               VALUES ('DELETE', OLD.id, '{table}', NULL);
             END;"
        ),
    ]
}

/// `json_object('c1', ALIAS.c1, ...)` over every synced column.
///
/// The four boolean columns are 0/1 integers in SQLite and `boolean` in
/// Postgres, and PostgREST refuses a JSON `0` for a boolean — so they are
/// injected as real JSON `true`/`false` with `json()`. Re-aggregating a PATCH
/// preserves that: `json_each` reports the subtype in its `type` column, which
/// is why the update trigger reads it rather than taking `value` as it comes.
fn row_object(columns: &[&str], alias: &str) -> String {
    let pairs = columns
        .iter()
        .map(|column| {
            if BOOLEAN_COLUMNS.contains(column) {
                format!(
                    "'{column}', json(CASE WHEN {alias}.\"{column}\" THEN 'true' ELSE 'false' END)"
                )
            } else {
                format!("'{column}', {alias}.\"{column}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("json_object({pairs})")
}

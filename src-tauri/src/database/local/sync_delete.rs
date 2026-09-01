//! The one way to delete a row from a table the sync registry knows about.
//!
//! Under state-based push there is no queue to notice a delete after the fact:
//! the row is the payload, so a row that vanishes without a tombstone is a
//! divergence nothing can detect later. Every hard delete of a synced table
//! therefore goes through [`delete_synced_where`], and the
//! `guard_unrecorded_delete_*` triggers refuse any that does not.
//!
//! The child walk is driven by [`sync::registry`]'s foreign-key links rather
//! than by SQLite's `ON DELETE CASCADE`. That is deliberate: cascade behaviour
//! depends on `PRAGMA foreign_keys`, cascade-deleted rows do not fire delete
//! triggers unless `recursive_triggers` is on, and a connection with foreign
//! keys disabled used to leave orphaned children behind. Walking the registry
//! makes the result the same on every connection.

use sqlx::{Row, SqliteConnection};

use crate::sync::registry::{self, TableMeta};
use crate::sync::tombstone;

/// Delete every row of `table` matching `where_sql`, recording a tombstone for
/// each and removing its registered children first.
///
/// `where_sql` is a fragment with `?` placeholders bound from `binds`, in order.
/// Returns the number of rows deleted from `table` itself.
///
/// # Errors
/// Local database failures, and an unregistered table name.
pub async fn delete_synced_where(
    connection: &mut SqliteConnection,
    table: &str,
    where_sql: &str,
    binds: &[&str],
) -> Result<usize, String> {
    let meta = registry::get_table(table)
        .ok_or_else(|| format!("table {table:?} is not registered for relational sync"))?;
    let keys = matching_keys(connection, meta, where_sql, binds).await?;
    let mut deleted = 0;
    for key in &keys {
        delete_one(connection, meta, key).await?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Delete one row by its primary-key values.
///
/// # Errors
/// Local database failures, and an unregistered table name.
pub async fn delete_synced_row(
    connection: &mut SqliteConnection,
    table: &str,
    pk_values: &[&str],
) -> Result<bool, String> {
    let meta = registry::get_table(table)
        .ok_or_else(|| format!("table {table:?} is not registered for relational sync"))?;
    let key: Vec<String> = pk_values.iter().map(|value| (*value).to_owned()).collect();
    if !row_exists(connection, meta, &key).await? {
        return Ok(false);
    }
    delete_one(connection, meta, &key).await?;
    Ok(true)
}

async fn delete_one(
    connection: &mut SqliteConnection,
    meta: &'static TableMeta,
    key: &[String],
) -> Result<(), String> {
    delete_registered_children(connection, meta, key).await?;
    let record_id = registry::record_id(key.iter().map(String::as_str));
    tombstone::record(connection, meta.name, &record_id, key)
        .await
        .map_err(|error| format!("record tombstone for {}.{record_id}: {error}", meta.name))?;
    let sql = format!("DELETE FROM {} WHERE {}", meta.name, meta.pk_where());
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for value in key {
        query = query.bind(value);
    }
    query
        .execute(connection)
        .await
        .map_err(|error| format!("delete {}.{record_id}: {error}", meta.name))?;
    Ok(())
}

/// Remove the rows that point at this one, deepest first.
///
/// Only single-column parents can be followed — a child cannot hold a composite
/// key in one column, and no registered table has a composite-keyed parent with
/// children.
async fn delete_registered_children(
    connection: &mut SqliteConnection,
    meta: &'static TableMeta,
    key: &[String],
) -> Result<(), String> {
    if meta.pk_columns().len() != 1 {
        return Ok(());
    }
    let parent_key = key[0].as_str();
    for child in registry::TABLES {
        for parent in child.parents {
            let (Some(column), true) = (parent.via, parent.table == meta.name) else {
                continue;
            };
            Box::pin(delete_synced_where(
                connection,
                child.name,
                &format!("{column} = ?"),
                &[parent_key],
            ))
            .await?;
        }
    }
    Ok(())
}

async fn matching_keys(
    connection: &mut SqliteConnection,
    meta: &'static TableMeta,
    where_sql: &str,
    binds: &[&str],
) -> Result<Vec<Vec<String>>, String> {
    let columns = meta.pk_columns();
    let sql = format!(
        "SELECT {} FROM {} WHERE {where_sql}",
        columns.join(", "),
        meta.name
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for bind in binds {
        query = query.bind(*bind);
    }
    let rows = query
        .fetch_all(connection)
        .await
        .map_err(|error| format!("resolve {} rows to delete: {error}", meta.name))?;
    rows.iter()
        .map(|row| {
            (0..columns.len())
                .map(|index| {
                    row.try_get::<String, _>(index)
                        .map_err(|error| format!("read {} primary key: {error}", meta.name))
                })
                .collect()
        })
        .collect()
}

async fn row_exists(
    connection: &mut SqliteConnection,
    meta: &'static TableMeta,
    key: &[String],
) -> Result<bool, String> {
    let sql = format!("SELECT 1 FROM {} WHERE {}", meta.name, meta.pk_where());
    let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
    for value in key {
        query = query.bind(value);
    }
    query
        .fetch_optional(connection)
        .await
        .map(|row| row.is_some())
        .map_err(|error| format!("look up {} row: {error}", meta.name))
}

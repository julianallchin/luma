//! Pull protocol: discovery + delta pull + dynamic SQL materialization.
//!
//! On every full sync:
//! 1. Discovery — find venues the user owns or has joined via Supabase
//! 2. Delta pull — for each table in tier order, fetch rows modified since
//!    last_pulled_at and upsert into local SQLite
//! 3. FK resolution — if a pulled row references an FK not yet local,
//!    fetch the dependency on-demand

use serde_json::Value;
use sqlx::SqlitePool;

use super::error::SyncError;
use super::registry::{self, TableMeta};
use super::state;
use super::traits::RemoteClient;

/// Stats from a pull operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct PullStats {
    pub tables_pulled: usize,
    pub rows_pulled: usize,
    pub venues_discovered: usize,
    pub errors: Vec<String>,
}

// ============================================================================
// Discovery
// ============================================================================

/// Discover all venues this user should have (owned + joined) and ensure
/// they exist locally. Returns all venue IDs the user has access to.
pub async fn discover_venues(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    uid: &str,
    token: &str,
) -> Result<Vec<String>, SyncError> {
    let mut all_venue_ids: Vec<String> = Vec::new();

    // 1. Fetch venues I own from Supabase
    let owned_query =
        format!("uid=eq.{uid}&select=id,uid,name,description,share_code,created_at,updated_at");
    let owned: Vec<Value> = remote.select_json("venues", &owned_query, token).await?;

    for row in &owned {
        let id = row["id"].as_str().unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        all_venue_ids.push(id.to_string());

        // Upsert locally as owner
        sqlx::query(
            "INSERT INTO venues (id, uid, name, description, share_code, role, created_at, updated_at, synced_at, version)
             VALUES (?, ?, ?, ?, ?, 'owner', ?, ?, ?, 1)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               description = excluded.description,
               share_code = excluded.share_code,
               synced_at = excluded.synced_at,
               version = version + 1",
        )
        .bind(id)
        .bind(row["uid"].as_str())
        .bind(row["name"].as_str())
        .bind(row["description"].as_str())
        .bind(row["share_code"].as_str())
        .bind(row["created_at"].as_str())
        .bind(row["updated_at"].as_str())
        .bind(row["updated_at"].as_str()) // synced_at = updated_at
        .execute(pool)
        .await?;
    }

    // 2. Fetch venues I've joined from Supabase (venue_members table)
    let memberships: Vec<Value> = remote
        .select_json(
            "venue_members",
            &format!("user_id=eq.{uid}&select=venue_id"),
            token,
        )
        .await?;

    let member_venue_ids: Vec<String> = memberships
        .iter()
        .filter_map(|row| row["venue_id"].as_str().map(|s| s.to_string()))
        .filter(|id| !all_venue_ids.contains(id)) // exclude already-owned
        .collect();

    if !member_venue_ids.is_empty() {
        // Fetch full venue rows for joined venues
        let ids_csv = member_venue_ids.join(",");
        let joined_venues: Vec<Value> = remote
            .select_json(
                "venues",
                &format!("id=in.({ids_csv})&select=id,uid,name,description,share_code,created_at,updated_at"),
                token,
            )
            .await?;

        for row in &joined_venues {
            let id = row["id"].as_str().unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            all_venue_ids.push(id.to_string());

            // Upsert locally as member (uid = venue owner's uid)
            sqlx::query(
                "INSERT INTO venues (id, uid, name, description, share_code, role, created_at, updated_at, synced_at, version)
                 VALUES (?, ?, ?, ?, ?, 'member', ?, ?, ?, 1)
                 ON CONFLICT(id) DO UPDATE SET
                   uid = excluded.uid,
                   name = excluded.name,
                   description = excluded.description,
                   synced_at = excluded.synced_at,
                   version = version + 1",
            )
            .bind(id)
            .bind(row["uid"].as_str()) // owner's uid
            .bind(row["name"].as_str())
            .bind(row["description"].as_str())
            .bind(row["share_code"].as_str())
            .bind(row["created_at"].as_str())
            .bind(row["updated_at"].as_str())
            .bind(row["updated_at"].as_str())
            .execute(pool)
            .await?;

            // Ensure venue_memberships record exists locally
            sqlx::query(
                "INSERT INTO venue_memberships (venue_id, user_id, role) VALUES (?, ?, 'member')
                 ON CONFLICT(venue_id, user_id) DO NOTHING",
            )
            .bind(id)
            .bind(uid)
            .execute(pool)
            .await?;
        }
    }

    // 3. Remove local member venues that no longer exist remotely
    let local_member_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM venues WHERE role = 'member'")
            .fetch_all(pool)
            .await?;

    for local_id in &local_member_ids {
        if !all_venue_ids.contains(local_id) {
            sqlx::query("DELETE FROM venues WHERE id = ? AND role = 'member'")
                .bind(local_id)
                .execute(pool)
                .await?;
        }
    }

    Ok(all_venue_ids)
}

// ============================================================================
// Delta pull
// ============================================================================

/// Pull all tables in tier order using delta timestamps.
pub async fn pull_all(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    token: &str,
) -> Result<PullStats, SyncError> {
    let mut stats = PullStats::default();

    for (_, tables) in registry::tables_by_tier() {
        for table in tables {
            match pull_table(pool, remote, table, token).await {
                Ok(count) => {
                    if count > 0 {
                        stats.tables_pulled += 1;
                        stats.rows_pulled += count;
                    }
                }
                Err(e) => {
                    stats.errors.push(format!("{}: {}", table.name, e));
                }
            }
        }
    }

    Ok(stats)
}

/// Pull a single table using delta timestamps.
async fn pull_table(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    table: &TableMeta,
    token: &str,
) -> Result<usize, SyncError> {
    let last_pulled = state::get_last_pulled_at(pool, table.name).await?;
    let now = chrono::Utc::now().to_rfc3339();

    // SELECT only remote columns (exclude local_only)
    let remote_cols: Vec<&str> = table
        .columns
        .iter()
        .filter(|c| !table.local_only.contains(c))
        .copied()
        .collect();
    let cols = remote_cols.join(",");
    let query = format!(
        "updated_at=gt.{last_pulled}&{}&select={cols}&order=updated_at.asc",
        scope_filter()
    );

    let rows: Vec<Value> = remote.select_json(table.name, &query, token).await?;
    let count = rows.len();

    for row in &rows {
        materialize_row(pool, table, row).await?;
    }

    if count > 0 {
        state::set_last_pulled_at(pool, table.name, &now).await?;
    }

    Ok(count)
}

// ============================================================================
// Dynamic SQL materialization
// ============================================================================

/// Upsert a single JSON row into local SQLite using dynamic SQL built from
/// the table's column metadata.
async fn materialize_row(
    pool: &SqlitePool,
    table: &TableMeta,
    row: &Value,
) -> Result<(), SyncError> {
    // Inject defaults for local-only columns that don't come from the remote.
    let mut row = row.clone();
    for col in table.local_only {
        if row.get(*col).is_none() || row[*col].is_null() {
            let default = match (table.name, *col) {
                ("tracks", "file_path") => {
                    let hash = row["track_hash"].as_str().unwrap_or("unknown");
                    Value::String(format!("{hash}.stub"))
                }
                ("track_stems", "file_path") => Value::String(String::new()),
                ("venues", "role") => Value::String("owner".to_string()),
                _ => Value::Null,
            };
            row[*col] = default;
        }
    }

    let cols = table.columns;
    let conflict_cols: Vec<&str> = table.conflict_key.split(',').collect();

    // Build: INSERT INTO {table} (col1, col2, ..., synced_at, version)
    //        VALUES (?1, ?2, ..., ?N, ?N+1)
    //        ON CONFLICT({conflict_key}) DO UPDATE SET
    //          col = excluded.col, ...,
    //          synced_at = excluded.synced_at,
    //          version = version + 1
    let all_cols: Vec<&str> = if has_sync_columns(table.name) {
        let mut v: Vec<&str> = cols.to_vec();
        v.push("synced_at");
        v
    } else {
        cols.to_vec()
    };

    let placeholders: Vec<String> = (1..=all_cols.len()).map(|i| format!("?{i}")).collect();

    let update_cols: Vec<String> = all_cols
        .iter()
        .filter(|c| !conflict_cols.contains(c))
        .map(|c| format!("{c} = excluded.{c}"))
        .collect();

    let version_clause = if has_sync_columns(table.name) {
        ", version = version + 1"
    } else {
        ""
    };

    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}{}",
        table.name,
        all_cols.join(", "),
        placeholders.join(", "),
        table.conflict_key,
        update_cols.join(", "),
        version_clause,
    );

    // Collect bound values as enum so we own them (avoids lifetime issues with row clone)
    let mut values: Vec<BoundValue> = Vec::with_capacity(all_cols.len());
    for col in cols {
        values.push(extract_value(&row, col));
    }
    if has_sync_columns(table.name) {
        // synced_at = updated_at from remote, or current time if remote has no updated_at
        let synced_at = row["updated_at"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        values.push(BoundValue::Text(synced_at));
    }

    let mut query = sqlx::query(&sql);
    for val in &values {
        query = match val {
            BoundValue::Text(s) => query.bind(s.as_str()),
            BoundValue::Int(i) => query.bind(*i),
            BoundValue::Float(f) => query.bind(*f),
            BoundValue::Null => query.bind(None::<String>),
        };
    }

    query.execute(pool).await?;
    Ok(())
}

/// Owned value extracted from JSON for binding.
enum BoundValue {
    Text(String),
    Int(i64),
    Float(f64),
    Null,
}

/// Extract a value from a JSON row as an owned BoundValue.
fn extract_value(row: &Value, column: &str) -> BoundValue {
    match &row[column] {
        Value::String(s) => BoundValue::Text(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                BoundValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                BoundValue::Float(f)
            } else {
                BoundValue::Null
            }
        }
        Value::Bool(b) => BoundValue::Int(*b as i64),
        Value::Null => BoundValue::Null,
        other => BoundValue::Text(other.to_string()),
    }
}

/// Whether a table has synced_at/version columns locally.
fn has_sync_columns(_table_name: &str) -> bool {
    // All tables now have sync columns after the group_members migration
    true
}

// ============================================================================
// Scope filter building
// ============================================================================

/// Supabase RLS handles all visibility — no client-side filter needed.
fn scope_filter() -> &'static str {
    "id=not.is.null"
}

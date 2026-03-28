//! Pull protocol: discovery + delta pull + dynamic SQL materialization.

use serde_json::Value;
use sqlx::SqlitePool;

use super::error::SyncError;
use super::registry::{self, TableMeta};
use super::state;
use super::traits::RemoteClient;

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

pub async fn discover_venues(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    uid: &str,
    token: &str,
) -> Result<Vec<String>, SyncError> {
    let mut all_venue_ids: Vec<String> = Vec::new();

    let owned: Vec<Value> = remote
        .select_json(
            "venues",
            &format!(
                "uid=eq.{uid}&select=id,uid,name,description,share_code,created_at,updated_at"
            ),
            token,
        )
        .await?;

    for row in &owned {
        if let Some(id) = upsert_venue(pool, row, "owner").await? {
            all_venue_ids.push(id);
        }
    }

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
        .filter(|id| !all_venue_ids.contains(id))
        .collect();

    if !member_venue_ids.is_empty() {
        let ids_csv = member_venue_ids.join(",");
        let joined: Vec<Value> = remote
            .select_json(
                "venues",
                &format!("id=in.({ids_csv})&select=id,uid,name,description,share_code,created_at,updated_at"),
                token,
            )
            .await?;

        for row in &joined {
            if let Some(id) = upsert_venue(pool, row, "member").await? {
                all_venue_ids.push(id.clone());
                sqlx::query(
                    "INSERT INTO venue_memberships (venue_id, user_id, role) VALUES (?, ?, 'member')
                     ON CONFLICT(venue_id, user_id) DO NOTHING",
                )
                .bind(&id)
                .bind(uid)
                .execute(pool)
                .await?;
            }
        }
    }

    // Remove stale member venues
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

async fn upsert_venue(
    pool: &SqlitePool,
    row: &Value,
    role: &str,
) -> Result<Option<String>, SyncError> {
    let id = match row["id"].as_str() {
        Some(id) if !id.is_empty() => id,
        _ => return Ok(None),
    };

    sqlx::query(
        "INSERT INTO venues (id, uid, name, description, share_code, role, created_at, updated_at, synced_at, origin, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'remote', 1)
         ON CONFLICT(id) DO UPDATE SET
           uid = excluded.uid, name = excluded.name, description = excluded.description,
           share_code = excluded.share_code, synced_at = excluded.synced_at,
           origin = 'remote', version = version + 1",
    )
    .bind(id)
    .bind(row["uid"].as_str())
    .bind(row["name"].as_str())
    .bind(row["description"].as_str())
    .bind(row["share_code"].as_str())
    .bind(role)
    .bind(row["created_at"].as_str())
    .bind(row["updated_at"].as_str())
    .bind(row["updated_at"].as_str()) // synced_at
    .execute(pool)
    .await?;

    Ok(Some(id.to_string()))
}

// ============================================================================
// Delta pull
// ============================================================================

pub async fn pull_all(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    token: &str,
    current_uid: Option<&str>,
) -> Result<PullStats, SyncError> {
    let mut stats = PullStats::default();

    for (_, tables) in registry::tables_by_tier() {
        for table in tables {
            match pull_table(pool, remote, table, token, current_uid).await {
                Ok(count) if count > 0 => {
                    stats.tables_pulled += 1;
                    stats.rows_pulled += count;
                }
                Err(e) => stats.errors.push(format!("{}: {e}", table.name)),
                _ => {}
            }
        }
    }

    Ok(stats)
}

async fn pull_table(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    table: &TableMeta,
    token: &str,
    current_uid: Option<&str>,
) -> Result<usize, SyncError> {
    let last_pulled = state::get_last_pulled_at(pool, table.name).await?;
    let now = chrono::Utc::now().to_rfc3339();

    let cols = table.remote_columns().join(",");
    // URL-encode the timestamp (spaces become +, etc.) and use the actual
    // PK column for the not-null filter (not all tables have `id`).
    let pk_col = table.pk_columns()[0];
    // PostgREST query params: encode + as %2B so it's not interpreted as space
    let ts_encoded = last_pulled.replace('+', "%2B");
    let query = format!(
        "updated_at=gt.{ts_encoded}&{pk_col}=not.is.null&select={cols}&order=updated_at.asc"
    );

    let rows: Vec<Value> = remote.select_json(table.name, &query, token).await?;
    if rows.is_empty() {
        return Ok(0);
    }

    let sql = build_upsert_sql(table);
    let mut count = 0usize;
    for row in &rows {
        // Skip rows that have a pending delete — the user deleted this
        // locally and the push loop hasn't flushed it yet.
        let record_id = extract_record_id(table, row);
        if has_pending_delete(pool, table.name, &record_id).await {
            continue;
        }
        match execute_upsert(pool, table, &sql, row, current_uid).await {
            Ok(()) => count += 1,
            Err(e) => {
                // Log and skip rows that violate constraints (e.g. FK to a
                // venue that hasn't been discovered yet) instead of aborting
                // the entire table pull.
                eprintln!(
                    "[sync] Skipping {}.{}: {e}",
                    table.name,
                    row.get(table.pk_columns()[0])
                        .and_then(|v| v.as_str())
                        .unwrap_or("?"),
                );
            }
        }
    }

    // Always advance the cursor. Skipped rows (e.g. FK to an
    // unpublished pattern the user can't access) will never succeed on
    // retry and would otherwise block the cursor forever, re-fetching
    // the entire table every cycle.
    state::set_last_pulled_at(pool, table.name, &now).await?;

    Ok(count)
}

// ============================================================================
// Dynamic SQL materialization
// ============================================================================

fn build_upsert_sql(table: &TableMeta) -> String {
    let conflict_cols: Vec<&str> = table.pk_columns();
    let mut all_cols: Vec<&str> = table.columns.to_vec();
    all_cols.push("synced_at");
    all_cols.push("origin");

    let placeholders: Vec<String> = (1..=all_cols.len()).map(|i| format!("?{i}")).collect();

    let update_cols: Vec<String> = all_cols
        .iter()
        .filter(|c| !conflict_cols.contains(c) && !table.local_only.contains(c))
        .map(|c| format!("{c} = excluded.{c}"))
        .collect();

    format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}, version = version + 1",
        table.name,
        all_cols.join(", "),
        placeholders.join(", "),
        table.conflict_key,
        update_cols.join(", "),
    )
}

async fn execute_upsert(
    pool: &SqlitePool,
    table: &TableMeta,
    sql: &str,
    row: &Value,
    current_uid: Option<&str>,
) -> Result<(), SyncError> {
    // Only clone if we need to inject local-only defaults
    let row = if !table.local_only.is_empty() {
        let mut cloned = row.clone();
        for col in table.local_only {
            if cloned.get(*col).is_none() || cloned[*col].is_null() {
                cloned[*col] = match (table.name, *col) {
                    ("tracks", "file_path") => {
                        let hash = cloned["track_hash"].as_str().unwrap_or("unknown");
                        Value::String(format!("{hash}.stub"))
                    }
                    ("track_stems", "file_path") => Value::String(String::new()),
                    _ => Value::Null,
                };
            }
        }
        cloned
    } else {
        row.clone() // shallow — needed because we bind from it
    };

    let mut values: Vec<BoundValue> = Vec::with_capacity(table.columns.len() + 1);
    for col in table.columns {
        values.push(extract_value(&row, col));
    }
    // synced_at = updated_at (or now if no updated_at)
    let synced_at = row["updated_at"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    values.push(BoundValue::Text(synced_at));
    // Own rows get origin='local' so delete triggers fire.
    // Other users' rows are 'remote' to prevent cascade-delete sync.
    let is_own = current_uid
        .and_then(|uid| row.get("uid").and_then(|v| v.as_str()).map(|v| v == uid))
        .unwrap_or(false);
    values.push(BoundValue::Text(
        if is_own { "local" } else { "remote" }.to_string(),
    ));

    let mut query = sqlx::query(sql);
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

enum BoundValue {
    Text(String),
    Int(i64),
    Float(f64),
    Null,
}

fn extract_record_id(table: &TableMeta, row: &Value) -> String {
    let parts: Vec<&str> = table
        .pk_columns()
        .iter()
        .map(|col| row.get(*col).and_then(|v| v.as_str()).unwrap_or(""))
        .collect();
    parts.join(":")
}

async fn has_pending_delete(pool: &SqlitePool, table_name: &str, record_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM pending_ops WHERE table_name = ? AND record_id = ? AND op_type = 'delete'",
    )
    .bind(table_name)
    .bind(record_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .is_some()
}

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

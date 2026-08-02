//! Pull protocol: discovery + delta pull + dynamic SQL materialization.

use serde_json::Value;
use sqlx::SqlitePool;

use crate::services::authored_documents::AuthoredDocuments;

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

    let mut installed_member_venue_ids = Vec::new();
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
                installed_member_venue_ids.push(id);
            }
        }
    }

    // Membership discovery owns only membership routing. Keep the venue
    // catalog row: it may anchor local scores, threads, or Git history that a
    // remote membership change must never cascade away. The complete routing
    // set is replaced in one remote-admitted transaction because a membership
    // row is an access grant, not ordinary synced content.
    reconcile_venue_memberships(pool, uid, &installed_member_venue_ids).await?;

    Ok(all_venue_ids)
}

async fn reconcile_venue_memberships(
    pool: &SqlitePool,
    uid: &str,
    remote_venue_ids: &[String],
) -> Result<(), SyncError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;

    for venue_id in remote_venue_ids {
        sqlx::query(
            "INSERT INTO venue_memberships (venue_id, user_id, role)
             VALUES (?, ?, 'member')
             ON CONFLICT(venue_id, user_id) DO NOTHING",
        )
        .bind(venue_id)
        .bind(uid)
        .execute(&mut *transaction)
        .await?;
    }

    let local_venue_ids: Vec<String> =
        sqlx::query_scalar("SELECT venue_id FROM venue_memberships WHERE user_id = ?")
            .bind(uid)
            .fetch_all(&mut *transaction)
            .await?;
    for venue_id in local_venue_ids {
        if !remote_venue_ids.contains(&venue_id) {
            sqlx::query("DELETE FROM venue_memberships WHERE venue_id = ? AND user_id = ?")
                .bind(venue_id)
                .bind(uid)
                .execute(&mut *transaction)
                .await?;
        }
    }

    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    transaction.commit().await?;
    Ok(())
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

    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    sqlx::query(
        "INSERT INTO venues (id, uid, name, description, share_code, role, created_at, updated_at, synced_at, origin, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'remote', 1)
         ON CONFLICT(id) DO UPDATE SET
           uid = excluded.uid, name = excluded.name, description = excluded.description,
           share_code = excluded.share_code, role = excluded.role,
           synced_at = excluded.synced_at,
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
    .execute(&mut *transaction)
    .await?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    transaction.commit().await?;

    Ok(Some(id.to_string()))
}

// ============================================================================
// Delta pull
// ============================================================================

pub async fn pull_all(
    pool: &SqlitePool,
    authored: &AuthoredDocuments,
    remote: &dyn RemoteClient,
    token: &str,
    current_uid: Option<&str>,
) -> Result<PullStats, SyncError> {
    let mut stats = PullStats::default();

    for table in registry::tables_in_topo_order() {
        match pull_table(pool, authored, remote, table, token, current_uid).await {
            Ok(count) if count > 0 => {
                stats.tables_pulled += 1;
                stats.rows_pulled += count;
            }
            Err(e) => stats.errors.push(format!("{}: {e}", table.name)),
            _ => {}
        }
    }

    Ok(stats)
}

async fn pull_table(
    pool: &SqlitePool,
    authored: &AuthoredDocuments,
    remote: &dyn RemoteClient,
    table: &TableMeta,
    token: &str,
    current_uid: Option<&str>,
) -> Result<usize, SyncError> {
    let uid_for_state = current_uid.unwrap_or("anonymous");
    let last_pulled = state::get_last_pulled_at(pool, uid_for_state, table.name).await?;
    let now = chrono::Utc::now().to_rfc3339();

    let cols = table.remote_columns().join(",");
    // Use the first PK column for the not-null filter (not all tables have `id`).
    let pk_col = table.pk_columns()[0];
    // Tables with a standalone `id` column get (updated_at, id) keyset pagination
    // to handle same-second timestamp ties at page boundaries. Composite-PK tables
    // (track_beats, track_roots, track_stems) have at most one row per track so a
    // simple updated_at cursor is sufficient.
    let has_id = table.columns.contains(&"id");

    let sql = build_upsert_sql(table);
    let mut total_count = 0usize;
    let mut had_failures = false;
    // Per-page cursors for keyset pagination.
    let mut page_ts = last_pulled.clone();
    let mut page_id: Option<String> = None;
    let mut made_progress = false;

    loop {
        // PostgREST query params: encode + as %2B so it's not interpreted as space.
        let ts_enc = page_ts.replace('+', "%2B");
        let query = match (has_id, &page_id) {
            // Second and later pages: keyset filter on (updated_at, id) to avoid
            // re-fetching rows or skipping ties at page boundaries.
            (true, Some(lid)) => format!(
                "or=(updated_at.gt.{ts_enc},and(updated_at.eq.{ts_enc},id.gt.{lid}))&{pk_col}=not.is.null&select={cols},deleted_at&order=updated_at.asc,id.asc"
            ),
            // First page (or tables without id): plain timestamp cursor.
            _ => format!(
                "updated_at=gt.{ts_enc}&{pk_col}=not.is.null&select={cols},deleted_at&order=updated_at.asc"
            ),
        };

        let rows: Vec<Value> = remote.select_json(table.name, &query, token).await?;
        if rows.is_empty() {
            break;
        }
        made_progress = true;

        // Advance the page cursor to the last row before processing so that a
        // partial-failure rewind uses the failed row's ts, not the page ts.
        if let Some(last_row) = rows.last() {
            if let Some(ts) = last_row["updated_at"].as_str() {
                page_ts = ts.to_string();
            }
            if has_id {
                page_id = last_row["id"].as_str().map(|s| s.to_string());
            }
        }

        for row in &rows {
            let record_id = extract_record_id(table, row);

            // Skip rows the user has modified locally but not yet pushed —
            // like unstaged changes in git, local edits take precedence.
            if is_locally_dirty(pool, table, &record_id, current_uid).await {
                eprintln!(
                    "[sync] Skipping pull of {}.{record_id} (locally dirty)",
                    table.name
                );
                continue;
            }

            // A remote tombstone may only remove a leaf row or a provably
            // empty, unauthored container. Git-authored documents are mutated
            // exclusively through AuthoredDocuments so their history cannot
            // disappear through a relational cascade.
            if row.get("deleted_at").and_then(|v| v.as_str()).is_some() {
                match delete_local(pool, authored, current_uid, table, &record_id).await {
                    Ok(true) => {
                        total_count += 1;
                        eprintln!(
                            "[sync] Deleted {}.{record_id} (soft-delete from remote)",
                            table.name
                        );
                    }
                    Ok(false) => {}
                    Err(error) => {
                        had_failures = true;
                        eprintln!(
                            "[sync] Refusing remote delete of {}.{record_id}: {error}",
                            table.name
                        );
                    }
                }
                continue;
            }

            match execute_upsert(pool, table, &sql, row, current_uid).await {
                Ok(()) => total_count += 1,
                Err(e) => {
                    had_failures = true;
                    let pk_val = table
                        .pk_columns()
                        .first()
                        .and_then(|c| row.get(*c))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    eprintln!("[sync] Skipping {}.{pk_val}: {e}", table.name);
                }
            }
        }
    }

    if !made_progress {
        return Ok(0);
    }

    // A `gt` timestamp cursor cannot point at a failed row without skipping
    // that row forever. Keep the prior durable cursor on any failure; replaying
    // successful rows is idempotent and guarantees the refusal is retried.
    let cursor = if had_failures { &last_pulled } else { &now };
    state::set_last_pulled_at(pool, uid_for_state, table.name, cursor).await?;

    Ok(total_count)
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

    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()));
    for val in &values {
        query = match val {
            BoundValue::Text(s) => query.bind(s.as_str()),
            BoundValue::Int(i) => query.bind(*i),
            BoundValue::Float(f) => query.bind(*f),
            BoundValue::Null => query.bind(None::<String>),
        };
    }

    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    query.execute(&mut *transaction).await?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    transaction.commit().await?;
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

/// Apply a remote tombstone under an immediate SQLite transaction.
///
/// Aggregate rows are deleted only when their cascades are provably empty of
/// authored state. A refused tombstone is an error so the pull cursor remains
/// behind it and retries after child tombstones or an explicit authored-state
/// operation resolves the dependency.
async fn delete_local(
    pool: &SqlitePool,
    authored: &AuthoredDocuments,
    current_uid: Option<&str>,
    table: &TableMeta,
    record_id: &str,
) -> Result<bool, SyncError> {
    match table.name {
        "scores" => {
            let principal = current_uid.ok_or_else(|| {
                SyncError::Local("remote authored deletion requires a signed-in principal".into())
            })?;
            let owner: Option<Option<String>> =
                sqlx::query_scalar("SELECT uid FROM scores WHERE id = ?")
                    .bind(record_id)
                    .fetch_optional(pool)
                    .await?;
            let Some(owner) = owner else {
                return Ok(false);
            };
            if owner.as_deref() == Some(principal) {
                return authored
                    .archive_score_from_remote(pool, principal, record_id)
                    .await
                    .map_err(|error| SyncError::Local(error.to_string()));
            }
            if let Some(archived) = authored
                .archive_git_backed_score_from_server(pool, record_id)
                .await
                .map_err(|error| SyncError::Local(error.to_string()))?
            {
                return Ok(archived);
            }
            return delete_empty_foreign_authored_container(pool, table, record_id, principal)
                .await;
        }
        "patterns" => {
            let principal = current_uid.ok_or_else(|| {
                SyncError::Local("remote authored deletion requires a signed-in principal".into())
            })?;
            let owner: Option<Option<String>> =
                sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
                    .bind(record_id)
                    .fetch_optional(pool)
                    .await?;
            let Some(owner) = owner else {
                return Ok(false);
            };
            if owner.as_deref() == Some(principal) {
                return authored
                    .archive_pattern_from_remote(pool, principal, record_id)
                    .await
                    .map_err(|error| SyncError::Local(error.to_string()));
            }
            if let Some(archived) = authored
                .archive_git_backed_pattern_from_server(pool, record_id)
                .await
                .map_err(|error| SyncError::Local(error.to_string()))?
            {
                return Ok(archived);
            }
            return delete_empty_foreign_authored_container(pool, table, record_id, principal)
                .await;
        }
        _ => {}
    }
    let where_clause = table.pk_where();
    let pk_values = table.decode_record_id(record_id);
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;

    let exists_sql = format!("SELECT 1 FROM {} WHERE {where_clause}", table.name);
    let mut exists_query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(exists_sql));
    for val in &pk_values {
        exists_query = exists_query.bind(*val);
    }
    if exists_query
        .fetch_optional(&mut *transaction)
        .await?
        .is_none()
    {
        transaction.commit().await?;
        return Ok(false);
    }

    ensure_remote_delete_is_safe(&mut transaction, table.name, record_id).await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;

    // Incoming tombstones must not generate a redundant outgoing tombstone.
    // Every table in the relational sync registry carries `origin`. Authored
    // implementation rows may cascade from a safe pattern deletion, but their
    // retired delete trigger cannot enqueue a second authority's operation.
    let mark_remote_sql = format!(
        "UPDATE {} SET origin = 'remote' WHERE {where_clause}",
        table.name
    );
    let mut mark_remote = sqlx::query(sqlx::AssertSqlSafe(mark_remote_sql));
    for val in &pk_values {
        mark_remote = mark_remote.bind(*val);
    }
    mark_remote.execute(&mut *transaction).await?;

    let delete_sql = format!("DELETE FROM {} WHERE {where_clause}", table.name);
    let mut delete_query = sqlx::query(sqlx::AssertSqlSafe(delete_sql));
    for val in &pk_values {
        delete_query = delete_query.bind(*val);
    }
    let deleted = delete_query.execute(&mut *transaction).await?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    transaction.commit().await?;
    Ok(deleted.rows_affected() > 0)
}

/// A member-visible foreign score/pattern with no Git ledger is only remote
/// catalog cache. After proving it owns no durable/dependent state, mark its
/// provenance under remote-write admission, then use transaction-local
/// maintenance solely to cross the authored-container delete guard. The two
/// modes occur in one IMMEDIATE transaction, so no ordinary writer can enter
/// between proof and deletion and no outgoing tombstone is enqueued.
async fn delete_empty_foreign_authored_container(
    pool: &SqlitePool,
    table: &TableMeta,
    record_id: &str,
    active_principal: &str,
) -> Result<bool, SyncError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    ensure_remote_delete_is_safe(&mut transaction, table.name, record_id).await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    let marked = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {} SET origin = 'remote' WHERE id = ?",
        table.name
    )))
    .bind(record_id)
    .execute(&mut *transaction)
    .await?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    if marked.rows_affected() == 0 {
        transaction.commit().await?;
        return Ok(false);
    }
    crate::database::local::write_admission::enter_maintenance_writes(
        &mut transaction,
        Some(active_principal),
    )
    .await
    .map_err(SyncError::Local)?;
    let deleted = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM {} WHERE id = ?",
        table.name
    )))
    .bind(record_id)
    .execute(&mut *transaction)
    .await?;
    crate::database::local::write_admission::leave_maintenance_writes(
        &mut transaction,
        Some(active_principal),
    )
    .await
    .map_err(SyncError::Local)?;
    transaction.commit().await?;
    Ok(deleted.rows_affected() == 1)
}

async fn ensure_remote_delete_is_safe(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table_name: &str,
    record_id: &str,
) -> Result<(), SyncError> {
    let blocked = match table_name {
        "venues" => {
            sqlx::query_scalar::<_, i64>(
                "SELECT
                    EXISTS(SELECT 1 FROM fixtures WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM fixture_groups WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM venue_implementation_overrides WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM scores WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM cues WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM midi_modifiers WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM midi_bindings WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM stage_pieces WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM agent_threads WHERE venue_id = ?1)
                    OR EXISTS(SELECT 1 FROM authored_state_projections WHERE venue_id = ?1)",
            )
            .bind(record_id)
            .fetch_one(&mut **transaction)
            .await?
                != 0
        }
        "tracks" => {
            let (track_hash, file_path, album_art_path): (String, String, Option<String>) =
                sqlx::query_as(
                    "SELECT track_hash, file_path, album_art_path FROM tracks WHERE id = ?",
                )
                .bind(record_id)
                .fetch_one(&mut **transaction)
                .await?;
            let owns_local_files = file_path != format!("{track_hash}.stub")
                || album_art_path
                    .as_deref()
                    .is_some_and(|path| !path.is_empty());
            let owns_dependents = sqlx::query_scalar::<_, i64>(
                "SELECT
                    EXISTS(SELECT 1 FROM scores WHERE track_id = ?1)
                    OR EXISTS(
                        SELECT 1 FROM agent_threads
                        WHERE subject_kind = 'track' AND subject_id = ?1
                    )
                    OR EXISTS(SELECT 1 FROM authored_state_projections WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM track_beats WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM track_roots WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM track_waveforms WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM track_stems WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM track_mert WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM track_drum_onsets WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM track_bar_classifications WHERE track_id = ?1)
                    OR EXISTS(SELECT 1 FROM preprocessing_failures WHERE track_id = ?1)",
            )
            .bind(record_id)
            .fetch_one(&mut **transaction)
            .await?
                != 0;
            owns_local_files || owns_dependents
        }
        "scores" => {
            sqlx::query_scalar::<_, i64>(
                "SELECT
                    EXISTS(SELECT 1 FROM track_scores WHERE score_id = ?1)
                    OR EXISTS(SELECT 1 FROM agent_threads WHERE score_id = ?1)
                    OR EXISTS(
                        SELECT 1 FROM authored_state_projections
                        WHERE document_kind = 'track_score' AND score_id = ?1
                    )",
            )
            .bind(record_id)
            .fetch_one(&mut **transaction)
            .await?
                != 0
        }
        "patterns" => pattern_has_authored_or_dependent_state(transaction, record_id).await?,
        _ => false,
    };

    if blocked {
        return Err(refused_remote_delete(
            table_name,
            record_id,
            "the row owns authored state, durable history, local artifacts, or dependent rows",
        ));
    }
    Ok(())
}

async fn pattern_has_authored_or_dependent_state(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    pattern_id: &str,
) -> Result<bool, SyncError> {
    let has_dependents = sqlx::query_scalar::<_, i64>(
        "SELECT
            EXISTS(SELECT 1 FROM track_scores WHERE pattern_id = ?1)
            OR EXISTS(
                SELECT 1 FROM agent_threads
                WHERE subject_kind = 'pattern' AND subject_id = ?1
            )
            OR EXISTS(
                SELECT 1 FROM authored_state_projections
                WHERE document_kind = 'pattern_graph' AND subject_id = ?1
            )
            OR EXISTS(SELECT 1 FROM cues WHERE pattern_id = ?1)
            OR EXISTS(SELECT 1 FROM venue_implementation_overrides WHERE pattern_id = ?1)",
    )
    .bind(pattern_id)
    .fetch_one(&mut **transaction)
    .await?
        != 0;
    if has_dependents {
        return Ok(true);
    }

    let graphs: Vec<String> =
        sqlx::query_scalar("SELECT graph_json FROM implementations WHERE pattern_id = ?")
            .bind(pattern_id)
            .fetch_all(&mut **transaction)
            .await?;
    Ok(graphs.iter().any(|graph| !graph_json_is_empty(graph)))
}

fn graph_json_is_empty(raw: &str) -> bool {
    let Ok(Value::Object(graph)) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if graph
        .keys()
        .any(|key| !matches!(key.as_str(), "nodes" | "edges" | "args"))
    {
        return false;
    }
    ["nodes", "edges"].iter().all(|key| {
        graph
            .get(*key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    }) && graph
        .get("args")
        .is_none_or(|args| args.as_array().is_some_and(Vec::is_empty))
}

fn refused_remote_delete(table_name: &str, record_id: &str, reason: &str) -> SyncError {
    SyncError::Local(format!(
        "remote tombstone for {table_name}.{record_id} requires an authored-state deletion: {reason}"
    ))
}

/// Check if a record has unpushed local changes — either a pending op
/// in the queue or the source table's explicit dirty flag (`synced_at IS NULL`).
async fn is_locally_dirty(
    pool: &SqlitePool,
    table: &TableMeta,
    record_id: &str,
    current_uid: Option<&str>,
) -> bool {
    // 1. Pending ops (queued upsert or delete not yet flushed to remote)
    let principal_key = crate::database::local::auth::principal_key(current_uid);
    let has_pending = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM pending_ops
         WHERE principal_key = ? AND table_name = ? AND record_id = ?",
    )
    .bind(principal_key)
    .bind(table.name)
    .bind(record_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .is_some();

    if has_pending {
        return true;
    }

    // 2. Dirty in source table (edited locally, not yet enqueued for push)
    let pk_cols = table.pk_columns();
    let where_clause = if pk_cols.len() == 1 {
        format!("{} = ?1", pk_cols[0])
    } else {
        // Composite PK: record_id is "a:b", split on ':'
        pk_cols
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c} = ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    let sql = format!(
        "SELECT 1 FROM {} WHERE {where_clause} AND synced_at IS NULL",
        table.name
    );

    let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(&*sql));
    if pk_cols.len() == 1 {
        query = query.bind(record_id);
    } else {
        for part in record_id.split(':') {
            query = query.bind(part);
        }
    }

    query.fetch_optional(pool).await.ok().flatten().is_some()
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

#[cfg(test)]
mod remote_deletion_tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::*;
    use crate::storage::StorageRoot;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool, AuthoredDocuments) {
        let directory = tempfile::tempdir().expect("tempdir");
        let database_path = directory.path().join("luma-test.db");
        let migration_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .expect("migration pool");
        sqlx::migrate!("./migrations")
            .run(&migration_pool)
            .await
            .expect("migrations");
        migration_pool.close().await;

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .expect("test pool");
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .expect("arm test principal");
        let authored = AuthoredDocuments::new(StorageRoot::from_path(
            directory.path().join("authored-storage"),
        ));
        (directory, pool, authored)
    }

    async fn insert_score_fixture(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path)
             VALUES ('track', 'alice', 'hash', 'hash.stub')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'Venue')")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'Pattern')")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('implementation', 'alice', 'pattern',
                     '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('score', 'alice', 'track', 'venue', 'Score')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_foreign_score_cache(pool: &SqlitePool) {
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut transaction)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path, origin)
             VALUES ('foreign-track', 'alice', 'foreign-hash', 'foreign-hash.stub', 'remote')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venues (id, uid, name, role, origin)
             VALUES ('foreign-venue', 'alice', 'Foreign Venue', 'member', 'remote')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venue_memberships (venue_id, user_id, role)
             VALUES ('foreign-venue', 'bob', 'member')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name, origin)
             VALUES ('foreign-score', 'alice', 'foreign-track', 'foreign-venue',
                     'Foreign Score', 'remote')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }

    #[tokio::test]
    async fn remote_origin_is_data_not_write_authority() {
        let (_directory, pool, _authored) = test_pool().await;

        let forged = sqlx::query(
            "INSERT INTO venues (id, uid, name, origin)
             VALUES ('forged', 'bob', 'Forged', 'remote')",
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            forged.to_string().contains("venue write is not authorized"),
            "{forged}"
        );

        let remote = serde_json::json!({
            "id": "remote",
            "uid": "bob",
            "name": "Remote",
            "description": null,
            "share_code": null,
            "created_at": "2026-08-02T00:00:00Z",
            "updated_at": "2026-08-02T00:00:00Z"
        });
        assert_eq!(
            upsert_venue(&pool, &remote, "member").await.unwrap(),
            Some("remote".into())
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT remote_writes FROM auth_write_admission WHERE singleton = 1"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );

        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        assert!(upsert_venue(&pool, &remote, "member").await.is_err());
    }

    #[tokio::test]
    async fn git_authored_projection_and_routing_deletes_have_no_relational_sync_route() {
        let (_directory, pool, authored) = test_pool().await;
        insert_score_fixture(&pool).await;
        sqlx::query(
            "INSERT INTO track_scores
             (id, uid, score_id, pattern_id, start_time, end_time, args_json)
             VALUES ('clip', 'alice', 'score', 'pattern', 0, 1, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venue_implementation_overrides
             (venue_id, pattern_id, implementation_id, uid)
             VALUES ('venue', 'pattern', 'implementation', 'alice')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let triggers: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master
             WHERE type = 'trigger'
               AND name IN (
                   'sync_delete_implementations',
                   'sync_delete_track_scores',
                   'sync_delete_venue_impl_overrides'
               )",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(triggers.is_empty());

        sqlx::query("DELETE FROM track_scores WHERE id = 'clip'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM venue_implementation_overrides
             WHERE venue_id = 'venue' AND pattern_id = 'pattern'",
        )
        .execute(&pool)
        .await
        .unwrap();
        authored
            .archive_pattern(&pool, Some("alice"), "pattern")
            .await
            .unwrap();
        let queued: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pending_ops
             WHERE table_name IN (
                 'implementations',
                 'track_scores',
                 'venue_implementation_overrides'
             )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(queued, 0);
    }

    #[tokio::test]
    async fn authored_sync_retirement_purges_legacy_override_queue_and_cursor_state() {
        let (_directory, pool, _authored) = test_pool().await;
        sqlx::raw_sql(
            "CREATE TRIGGER sync_delete_venue_impl_overrides
             AFTER DELETE ON venue_implementation_overrides FOR EACH ROW
             BEGIN
                 INSERT OR REPLACE INTO pending_ops
                    (principal_key, op_type, table_name, record_id, next_retry_at)
                 VALUES (
                    'signed-in:alice',
                    'delete',
                    'venue_implementation_overrides',
                    OLD.venue_id || ':' || OLD.pattern_id,
                    CURRENT_TIMESTAMP
                 );
             END;
             INSERT INTO pending_ops
                (principal_key, op_type, table_name, record_id, conflict_key)
             VALUES (
                'signed-in:alice',
                'delete',
                'venue_implementation_overrides',
                'venue:pattern',
                'venue_id,pattern_id'
             );
             INSERT INTO sync_state (uid, table_name, last_pulled_at)
             VALUES (
                'alice',
                'venue_implementation_overrides',
                '2026-01-01T00:00:00Z'
             );",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../migrations/20260802000000_retire_relational_authored_sync.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();

        let trigger_exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'trigger' AND name = 'sync_delete_venue_impl_overrides'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(trigger_exists, 0);
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pending_ops
             WHERE table_name = 'venue_implementation_overrides'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, 0);
        let cursor: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sync_state
             WHERE table_name = 'venue_implementation_overrides'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cursor, 0);
    }

    #[tokio::test]
    async fn remote_tombstones_cannot_delete_score_clips_or_their_container() {
        let (_directory, pool, authored) = test_pool().await;
        insert_score_fixture(&pool).await;
        sqlx::query(
            "INSERT INTO track_scores
             (id, uid, score_id, pattern_id, start_time, end_time, args_json)
             VALUES ('clip', 'alice', 'score', 'pattern', 0, 1, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO authored_state_projections
             (repository_id, document_kind, principal_key, subject_id,
              track_id, venue_id, score_id, projected_commit)
             VALUES ('repo', 'track_score', 'signed-out', 'track',
                     'track', 'venue', 'score', 'commit')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let score_error = delete_local(
            &pool,
            &authored,
            Some("alice"),
            registry::get_table("scores").unwrap(),
            "score",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            score_error.contains("score deletion requires an archived authored projection"),
            "{score_error}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores WHERE id = 'clip'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = 'score'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn remote_tombstone_deletes_an_empty_score_without_echoing_it() {
        let (_directory, pool, authored) = test_pool().await;
        insert_score_fixture(&pool).await;

        assert!(delete_local(
            &pool,
            &authored,
            Some("alice"),
            registry::get_table("scores").unwrap(),
            "score",
        )
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = 'score'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE op_type = 'delete' AND table_name = 'scores'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn joined_member_tombstone_deletes_only_an_empty_foreign_score_cache() {
        let (_directory, pool, authored) = test_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        insert_foreign_score_cache(&pool).await;

        assert!(delete_local(
            &pool,
            &authored,
            Some("bob"),
            registry::get_table("scores").unwrap(),
            "foreign-score",
        )
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = 'foreign-score'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM authored_state_projections
                 WHERE document_kind = 'track_score' AND score_id = 'foreign-score'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "an empty foreign cache must not manufacture a Git document"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE table_name = 'scores' AND record_id = 'foreign-score'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "an incoming tombstone must not echo into Bob's outgoing queue"
        );
    }

    #[tokio::test]
    async fn foreign_git_backed_score_tombstone_archives_the_owner_repository() {
        let (_directory, pool, authored) = test_pool().await;
        insert_score_fixture(&pool).await;
        authored
            .reconcile_track_score_for_read(&pool, "score")
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        reconcile_venue_memberships(&pool, "bob", &["venue".into()])
            .await
            .unwrap();

        assert!(delete_local(
            &pool,
            &authored,
            Some("bob"),
            registry::get_table("scores").unwrap(),
            "score",
        )
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT materialization_state FROM authored_state_projections
                 WHERE document_kind = 'track_score' AND score_id = 'score'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "archived"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE table_name = 'scores' AND record_id = 'score'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn foreign_public_pattern_tombstone_deletes_empty_cache_without_git_import() {
        let (_directory, pool, authored) = test_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut transaction)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO patterns (id, uid, name, is_verified, origin)
             VALUES ('public-pattern', 'alice', 'Public Pattern', 1, 'remote')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        assert!(delete_local(
            &pool,
            &authored,
            Some("bob"),
            registry::get_table("patterns").unwrap(),
            "public-pattern",
        )
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM authored_state_projections
                 WHERE subject_id = 'public-pattern'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE table_name = 'patterns' AND record_id = 'public-pattern'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn foreign_git_backed_public_pattern_tombstone_archives_owner_history() {
        let (_directory, pool, authored) = test_pool().await;
        sqlx::query(
            "INSERT INTO patterns (id, uid, name, is_verified)
             VALUES ('public-pattern', 'alice', 'Public Pattern', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('public-implementation', 'alice', 'public-pattern',
                     '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        authored
            .reconcile_pattern_graphs_for_read(&pool, "public-pattern")
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();

        assert!(delete_local(
            &pool,
            &authored,
            Some("bob"),
            registry::get_table("patterns").unwrap(),
            "public-pattern",
        )
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT materialization_state FROM authored_state_projections
                 WHERE subject_id = 'public-pattern'
                   AND implementation_id = 'public-implementation'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "archived"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE table_name = 'patterns' AND record_id = 'public-pattern'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn generic_score_upserts_cannot_rebind_authored_repository_identity() {
        let (_directory, pool, _authored) = test_pool().await;
        insert_score_fixture(&pool).await;
        let table = registry::get_table("scores").unwrap();
        let sql = build_upsert_sql(table);
        let base = serde_json::json!({
            "id": "score",
            "uid": "alice",
            "track_id": "track",
            "venue_id": "venue",
            "name": "Remote Rename",
            "created_at": "2026-08-02T00:00:00Z",
            "updated_at": "2026-08-02T00:00:01Z"
        });
        for (field, value) in [
            ("uid", serde_json::json!("mallory")),
            ("track_id", serde_json::json!("another-track")),
            ("venue_id", serde_json::json!("another-venue")),
        ] {
            let mut rebound = base.clone();
            rebound[field] = value;
            let error = execute_upsert(&pool, table, &sql, &rebound, Some("alice"))
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("score authored identity is immutable"),
                "{field}: {error}"
            );
        }
        let identity: (Option<String>, String, String, Option<String>) =
            sqlx::query_as("SELECT uid, track_id, venue_id, name FROM scores WHERE id = 'score'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            identity,
            (
                Some("alice".into()),
                "track".into(),
                "venue".into(),
                Some("Score".into())
            )
        );
    }

    #[tokio::test]
    async fn pattern_tombstone_requires_empty_graphs_and_no_git_history() {
        let (_directory, pool, authored) = test_pool().await;
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'Pattern')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('implementation', 'alice', 'pattern',
                     '{\"nodes\":[{\"id\":\"pulse\"}],\"edges\":[],\"args\":[]}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(delete_local(
            &pool,
            &authored,
            Some("alice"),
            registry::get_table("patterns").unwrap(),
            "pattern",
        )
        .await
        .is_err());
        sqlx::query(
            "UPDATE implementations
             SET graph_json = '{\"nodes\":[],\"edges\":[],\"args\":[]}'
             WHERE id = 'implementation'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO authored_state_projections
             (repository_id, document_kind, principal_key, subject_id,
              implementation_id, projected_commit)
             VALUES ('repo', 'pattern_graph', 'signed-out', 'pattern',
                     'implementation', 'commit')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(delete_local(
            &pool,
            &authored,
            Some("alice"),
            registry::get_table("patterns").unwrap(),
            "pattern",
        )
        .await
        .is_err());

        sqlx::query("DELETE FROM authored_state_projections WHERE repository_id = 'repo'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(delete_local(
            &pool,
            &authored,
            Some("alice"),
            registry::get_table("patterns").unwrap(),
            "pattern",
        )
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM implementations WHERE id = 'implementation'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE op_type = 'delete' AND table_name = 'patterns'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn track_tombstone_only_deletes_a_dependency_free_undownloaded_stub() {
        let (_directory, pool, authored) = test_pool().await;
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES
             ('stub', 'alice', 'stub-hash', 'stub-hash.stub'),
             ('downloaded', 'alice', 'real-hash', '/managed/audio.mp3')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(delete_local(
            &pool,
            &authored,
            Some("alice"),
            registry::get_table("tracks").unwrap(),
            "downloaded",
        )
        .await
        .is_err());
        assert!(delete_local(
            &pool,
            &authored,
            Some("alice"),
            registry::get_table("tracks").unwrap(),
            "stub",
        )
        .await
        .unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks WHERE id = 'downloaded'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }
}

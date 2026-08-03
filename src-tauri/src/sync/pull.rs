//! Pull protocol: discovery + delta pull + dynamic SQL materialization.

use std::collections::HashSet;

use serde_json::Value;
use sqlx::SqlitePool;

use crate::agent_execution::{GraphRunStore, PythonWorkspaceService};
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
    let previously_owned: Vec<String> = sqlx::query_scalar("SELECT id FROM venues WHERE uid = ?")
        .bind(uid)
        .fetch_all(pool)
        .await?;

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
    let owned_visibility_expanded = all_venue_ids
        .iter()
        .any(|venue_id| !previously_owned.contains(venue_id));

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
    // catalog row: it may anchor local scores, threads, or authored history that a
    // remote membership change must never cascade away. The complete routing
    // set is replaced in one remote-admitted transaction because a membership
    // row is an access grant, not ordinary synced content.
    reconcile_venue_memberships(
        pool,
        uid,
        &installed_member_venue_ids,
        owned_visibility_expanded,
    )
    .await?;

    Ok(all_venue_ids)
}

async fn reconcile_venue_memberships(
    pool: &SqlitePool,
    uid: &str,
    remote_venue_ids: &[String],
    owned_visibility_expanded: bool,
) -> Result<(), SyncError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;

    let previously_visible: Vec<String> =
        sqlx::query_scalar("SELECT venue_id FROM venue_memberships WHERE user_id = ?")
            .bind(uid)
            .fetch_all(&mut *transaction)
            .await?;
    let visibility_expanded = owned_visibility_expanded
        || remote_venue_ids
            .iter()
            .any(|venue_id| !previously_visible.contains(venue_id));

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

    if visibility_expanded {
        // A global server sequence can predate a newly granted membership.
        // Reset this principal's cursors while installing the grant so the
        // next pull idempotently replays every now-visible row instead of
        // skipping old rows behind a newer cursor.
        sqlx::query("UPDATE sync_state SET last_pulled_at = 'seq:0' WHERE uid = ?")
            .bind(uid)
            .execute(&mut *transaction)
            .await?;
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
    workspaces: &PythonWorkspaceService,
    graph_runs: &GraphRunStore,
    remote: &dyn RemoteClient,
    token: &str,
    current_uid: Option<&str>,
) -> Result<PullStats, SyncError> {
    let mut stats = PullStats::default();
    let mut unavailable = HashSet::new();

    for table in registry::tables_in_topo_order() {
        let blocked_by = table
            .parents
            .iter()
            .find(|parent| unavailable.contains(**parent));
        if let Some(parent) = blocked_by {
            unavailable.insert(table.name);
            stats.errors.push(format!(
                "{}: deferred because dependency {parent} did not finish",
                table.name
            ));
            continue;
        }
        match pull_table(pool, authored, remote, table, token, current_uid).await {
            Ok(count) if count > 0 => {
                stats.tables_pulled += 1;
                stats.rows_pulled += count;
            }
            Err(e @ (SyncError::Network(_) | SyncError::AuthRequired))
            | Err(e @ SyncError::Api { status: 401, .. }) => return Err(e),
            Err(e) => {
                unavailable.insert(table.name);
                stats.errors.push(format!("{}: {e}", table.name));
            }
            _ => {}
        }
    }

    if let Some(uid) = current_uid {
        if let Err(error) = authored.bootstrap_live_projections(pool, Some(uid)).await {
            stats
                .errors
                .push(format!("authored projection bootstrap: {error}"));
        }
        // Archive facts are the durable terminal authority. Catalog
        // tombstones are only a projection, so every online owner client
        // finishes any pending local cleanup rather than depending on the
        // device that submitted the archive remaining online.
        // A partial authored-document/archive pull cannot prove that every
        // implementation sibling is known locally. Defer all projection
        // cleanup until both tables completed; the immutable facts make the
        // next complete cycle idempotent.
        if !archive_reconciliation_is_safe(&unavailable) {
            stats
                .errors
                .push("authored archive reconciliation: deferred after partial pull".into());
        } else if let Err(error) = authored.reconcile_remote_archives(pool, uid).await {
            stats
                .errors
                .push(format!("authored archive reconciliation: {error}"));
        }
        if let Err(error) = authored.enqueue_pending_head_integrations(pool, uid).await {
            stats
                .errors
                .push(format!("authored head integration scan: {error}"));
        }
    }

    // A server deletion is terminal as soon as its thread projection reaches
    // `deleting`. Finish the same resource-draining lifecycle used by local,
    // startup, and identity-activation deletion before this pull returns.
    // Failures remain durable and retry on every later pull, even when there
    // are no new remote rows.
    if let Err(error) = crate::agent_execution::thread_cleanup::recover_deleting_agent_threads(
        pool, authored, workspaces, graph_runs,
    )
    .await
    {
        stats
            .errors
            .push(format!("agent thread deletion recovery: {error}"));
    }

    Ok(stats)
}

fn archive_reconciliation_is_safe(unavailable: &HashSet<&str>) -> bool {
    !unavailable.contains("authored_documents")
        && !unavailable.contains("authored_document_archives")
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
    let durable_cursor = state::get_last_pulled_seq(pool, uid_for_state, table.name).await?;

    let cols = table.remote_columns().join(",");
    // Use the first PK column for the not-null filter (not all tables have `id`).
    let pk_col = table.pk_columns()[0];
    let sql = build_upsert_sql(table);
    let mut total_count = 0usize;
    let mut page_cursor = durable_cursor;
    let mut last_successful_cursor = durable_cursor;
    let mut stopped_at_failure: Option<SyncError> = None;

    'pages: loop {
        // `sync_seq` is assigned by one server sequence on every remotely
        // visible change. It is therefore the complete keyset: unique,
        // monotonic, independent of client clocks, and valid for every PK
        // shape in the registry.
        let tombstone_column = if registry::has_remote_tombstone(table.name) {
            ",deleted_at"
        } else {
            ""
        };
        let query = format!(
            "{}=gt.{page_cursor}&{pk_col}=not.is.null&select={cols}{tombstone_column},{}&order={}.asc&limit={}",
            registry::SERVER_CURSOR_COLUMN,
            registry::SERVER_CURSOR_COLUMN,
            registry::SERVER_CURSOR_COLUMN,
            registry::PULL_PAGE_LIMIT,
        );

        let rows: Vec<Value> = remote.select_json(table.name, &query, token).await?;
        if rows.is_empty() {
            break;
        }

        for row in &rows {
            let sequence = extract_sync_seq(table, row)?;
            if sequence <= page_cursor {
                return Err(SyncError::Parse(format!(
                    "server returned non-advancing sync_seq {sequence} for {} after {page_cursor}",
                    table.name
                )));
            }
            let record_id = extract_record_id(table, row);

            // Skip rows the user has modified locally but not yet pushed —
            // Local pending writes take precedence until their durable push completes.
            if is_locally_dirty(pool, table, &record_id, current_uid).await {
                eprintln!(
                    "[sync] Skipping pull of {}.{record_id} (locally dirty)",
                    table.name
                );
                page_cursor = sequence;
                last_successful_cursor = sequence;
                continue;
            }

            // A remote tombstone may only remove a leaf row or a provably
            // empty, unauthored container. Authored documents are mutated
            // exclusively through AuthoredDocuments so their history cannot
            // disappear through a relational cascade.
            if registry::has_remote_tombstone(table.name)
                && row.get("deleted_at").and_then(|v| v.as_str()).is_some()
            {
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
                        eprintln!(
                            "[sync] Refusing remote delete of {}.{record_id}: {error}",
                            table.name
                        );
                        stopped_at_failure = Some(error);
                        break 'pages;
                    }
                }
                page_cursor = sequence;
                last_successful_cursor = sequence;
                continue;
            }

            if table.name == "authored_document_heads" {
                let uid = current_uid.ok_or_else(|| {
                    SyncError::Local(
                        "authored server-head projection requires a signed-in principal".into(),
                    )
                })?;
                let document_id = row
                    .get("document_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SyncError::MissingField("document_id".into()))?;
                let revision_id = row
                    .get("revision_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SyncError::MissingField("revision_id".into()))?;
                let generation = row
                    .get("generation")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| SyncError::MissingField("generation".into()))?;
                let updated_at = row
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SyncError::MissingField("updated_at".into()))?;
                match authored
                    .apply_server_head(pool, uid, document_id, revision_id, generation, updated_at)
                    .await
                {
                    Ok(()) => {
                        total_count += 1;
                        page_cursor = sequence;
                        last_successful_cursor = sequence;
                        continue;
                    }
                    Err(error) => {
                        eprintln!(
                            "[sync] Cannot project server head {document_id}.{revision_id}: {error}"
                        );
                        stopped_at_failure = Some(SyncError::Local(error.to_string()));
                        break 'pages;
                    }
                }
            }

            match execute_upsert(pool, table, &sql, row, current_uid).await {
                Ok(()) => {
                    if table.name == "authored_head_integrations" {
                        if let (Some(uid), Some(result_revision_id)) = (
                            current_uid,
                            row.get("result_revision_id").and_then(Value::as_str),
                        ) {
                            let document_id = row
                                .get("document_id")
                                .and_then(Value::as_str)
                                .ok_or_else(|| SyncError::MissingField("document_id".into()))?;
                            authored
                                .apply_integrated_server_head(
                                    pool,
                                    uid,
                                    document_id,
                                    result_revision_id,
                                )
                                .await
                                .map_err(|error| SyncError::Local(error.to_string()))?;
                        }
                    }
                    total_count += 1;
                    page_cursor = sequence;
                    last_successful_cursor = sequence;
                }
                Err(e) => {
                    let pk_val = table
                        .pk_columns()
                        .first()
                        .and_then(|c| row.get(*c))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    eprintln!("[sync] Skipping {}.{pk_val}: {e}", table.name);
                    stopped_at_failure = Some(e);
                    break 'pages;
                }
            }
        }
    }

    if last_successful_cursor > durable_cursor {
        state::advance_last_pulled_seq(pool, uid_for_state, table.name, last_successful_cursor)
            .await?;
    }
    if let Some(error) = stopped_at_failure {
        eprintln!(
            "[sync] Pull of {} stopped at server sequence after {}; the failing row will retry",
            table.name, last_successful_cursor
        );
        return Err(error);
    }

    Ok(total_count)
}

// ============================================================================
// Dynamic SQL materialization
// ============================================================================

fn build_upsert_sql(table: &TableMeta) -> String {
    let conflict_cols: Vec<&str> = table.pk_columns();
    if registry::pull_policy(table.name) != registry::PullPolicy::DirtyUpsert {
        let placeholders: Vec<String> = (1..=table.columns.len())
            .map(|index| format!("?{index}"))
            .collect();
        if registry::pull_policy(table.name) == registry::PullPolicy::ProjectionUpsert {
            let update_cols = table
                .columns
                .iter()
                .filter(|column| !conflict_cols.contains(column))
                .map(|column| format!("{column} = excluded.{column}"))
                .collect::<Vec<_>>()
                .join(", ");
            return format!(
                "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}",
                table.name,
                table.columns.join(", "),
                placeholders.join(", "),
                table.conflict_key,
                update_cols,
            );
        }
        return format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO NOTHING",
            table.name,
            table.columns.join(", "),
            placeholders.join(", "),
            table.conflict_key,
        );
    }
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

/// Apply one exact transcript-head observation obtained immediately after an
/// immutable append receipt is accepted. This intentionally does not advance
/// the table pull cursor: the next ordered pull replays the same server row
/// and preserves the global `sync_seq` authority.
pub(super) async fn apply_agent_transcript_head_observation(
    pool: &SqlitePool,
    row: &Value,
    admitted_user_id: &str,
    expected_thread_id: &str,
) -> Result<(), SyncError> {
    let thread_id = row
        .get("thread_id")
        .and_then(Value::as_str)
        .ok_or_else(|| SyncError::MissingField("thread_id".into()))?;
    if thread_id != expected_thread_id {
        return Err(SyncError::Parse(
            "server transcript head belongs to a different thread".into(),
        ));
    }
    if row.get("owner_user_id").and_then(Value::as_str) != Some(admitted_user_id) {
        return Err(SyncError::Parse(
            "server transcript head belongs to a different principal".into(),
        ));
    }
    let table = registry::get_table("agent_thread_transcript_heads")
        .expect("transcript heads are registered for pull");
    let sql = build_upsert_sql(table);
    execute_upsert(pool, table, &sql, row, Some(admitted_user_id)).await
}

async fn execute_upsert(
    pool: &SqlitePool,
    table: &TableMeta,
    sql: &str,
    row: &Value,
    current_uid: Option<&str>,
) -> Result<(), SyncError> {
    if matches!(
        table.name,
        "agent_threads" | "agent_thread_transcript_heads"
    ) {
        let thread_id = row
            .get(if table.name == "agent_threads" {
                "id"
            } else {
                "thread_id"
            })
            .and_then(Value::as_str)
            .ok_or_else(|| SyncError::MissingField("thread id".into()))?;
        let principal_key = crate::database::local::auth::principal_key(current_uid);
        let terminal_receipt_exists = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM agent_thread_deletions
             WHERE thread_id = ? AND principal_key = ?",
        )
        .bind(thread_id)
        .bind(principal_key)
        .fetch_optional(pool)
        .await?
        .is_some();
        if terminal_receipt_exists {
            // The immutable deletion fact outranks every mutable lifecycle or
            // head snapshot. Consuming the server row without materializing it
            // prevents a completed cleanup from being recreated on pull.
            return Ok(());
        }
    }

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

    let pull_policy = registry::pull_policy(table.name);
    let has_delivery_columns = pull_policy == registry::PullPolicy::DirtyUpsert;
    let mut values: Vec<BoundValue> = Vec::with_capacity(table.columns.len() + 2);
    for col in table.columns {
        values.push(extract_value(table, &row, col)?);
    }
    if has_delivery_columns {
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
    }

    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()));
    for val in &values {
        query = match val {
            BoundValue::Text(s) => query.bind(s.as_str()),
            BoundValue::Int(i) => query.bind(*i),
            BoundValue::Float(f) => query.bind(*f),
            BoundValue::Bytes(bytes) => query.bind(bytes.as_slice()),
            BoundValue::Null => query.bind(None::<String>),
        };
    }

    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    let result = query.execute(&mut *transaction).await?;
    if result.rows_affected() == 0 {
        match pull_policy {
            registry::PullPolicy::Immutable => {
                verify_row_except(&mut transaction, table, &values, &[]).await?;
            }
            registry::PullPolicy::ServerEnriched => {
                apply_server_enrichment(&mut transaction, table, &values).await?;
            }
            registry::PullPolicy::TerminalArchive => {
                apply_terminal_archive(&mut transaction, table, &values).await?;
            }
            registry::PullPolicy::ThreadProjection => {
                apply_thread_projection(&mut transaction, table, &values).await?;
            }
            registry::PullPolicy::DirtyUpsert | registry::PullPolicy::ProjectionUpsert => {}
        }
    }
    if table.name == "agent_thread_deletions" {
        apply_thread_deletion_projection(&mut transaction, table, &values).await?;
    }
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    transaction.commit().await?;
    Ok(())
}

/// Mirror the server's terminal-deletion projection locally in the same
/// trusted transaction that accepts its immutable receipt. This transition
/// must outrank a locally dirty thread snapshot; otherwise pull could consume
/// the receipt cursor while leaving the lifecycle row active forever.
async fn apply_thread_deletion_projection(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &TableMeta,
    values: &[BoundValue],
) -> Result<(), SyncError> {
    let thread_id = value_for_column(table, values, "thread_id");
    let owner_user_id = value_for_column(table, values, "owner_user_id");
    let deleted_at = value_for_column(table, values, "deleted_at");
    let mut query = sqlx::query(
        "UPDATE agent_threads
         SET lifecycle_state = 'deleting', updated_at = ?
         WHERE id IS ? AND owner_user_id IS ?",
    );
    for value in [deleted_at, thread_id, owner_user_id] {
        query = bind_value(query, value);
    }
    query.execute(&mut **transaction).await?;
    Ok(())
}

enum BoundValue {
    Text(String),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
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

fn extract_sync_seq(table: &TableMeta, row: &Value) -> Result<u64, SyncError> {
    let sequence = row
        .get(registry::SERVER_CURSOR_COLUMN)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        })
        .ok_or_else(|| {
            SyncError::Parse(format!(
                "remote row from {} is missing a valid sync_seq",
                table.name
            ))
        })?;
    if sequence == 0 {
        return Err(SyncError::Parse(format!(
            "remote row from {} has invalid sync_seq 0",
            table.name
        )));
    }
    Ok(sequence)
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
                let authored_exists = sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS(
                         SELECT 1 FROM authored_documents
                         WHERE document_kind = 'track_score' AND score_id = ?
                     )",
                )
                .bind(record_id)
                .fetch_one(pool)
                .await?
                    != 0;
                if authored_exists {
                    return authored
                        .archive_score_from_remote(pool, principal, record_id)
                        .await
                        .map_err(|error| SyncError::Local(error.to_string()));
                }
            }
            return delete_empty_authored_container_cache(pool, table, record_id, principal).await;
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
                let authored_exists = sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS(
                         SELECT 1 FROM authored_documents
                         WHERE document_kind = 'pattern_graph' AND subject_id = ?
                     )",
                )
                .bind(record_id)
                .fetch_one(pool)
                .await?
                    != 0;
                if authored_exists {
                    return authored
                        .archive_pattern_from_remote(pool, principal, record_id)
                        .await
                        .map_err(|error| SyncError::Local(error.to_string()));
                }
            }
            return delete_empty_authored_container_cache(pool, table, record_id, principal).await;
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

/// A score/pattern with no authored document is only remote catalog cache,
/// whether owned or member-visible. After proving it owns no durable/dependent
/// state, mark its provenance under remote-write admission, then use
/// transaction-local maintenance solely to cross the authored-container delete
/// guard. The two modes occur in one IMMEDIATE transaction, so no ordinary
/// writer can enter between proof and deletion and no outgoing tombstone is
/// enqueued.
async fn delete_empty_authored_container_cache(
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
                    OR EXISTS(SELECT 1 FROM authored_documents WHERE venue_id = ?1)",
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
                    OR EXISTS(SELECT 1 FROM authored_documents WHERE track_id = ?1)
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
                        SELECT 1 FROM authored_documents
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
                SELECT 1 FROM authored_documents
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
    let push_policy = registry::push_policy(table.name);
    if !matches!(
        push_policy,
        registry::PushPolicy::DirtyUpsert | registry::PushPolicy::ExplicitUpsert
    ) {
        // Immutable rows can be verified while their delivery op is pending,
        // and server projections/enrichments must never be hidden by a local
        // authority wake-up. In particular, a terminal authored archive must
        // beat an offline document-create replay.
        return false;
    }

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

    // Immutable product traces carry no mutable delivery flag. Their creating
    // transaction explicitly enqueues the row, so absence from pending_ops is
    // the only delivery state.
    if push_policy != registry::PushPolicy::DirtyUpsert {
        return false;
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

fn extract_value(table: &TableMeta, row: &Value, column: &str) -> Result<BoundValue, SyncError> {
    if registry::is_binary_column(table.name, column) {
        return match &row[column] {
            Value::String(value) => decode_postgres_bytea(value).map(BoundValue::Bytes),
            Value::Null => Ok(BoundValue::Null),
            _ => Err(SyncError::Parse(format!(
                "{}.{} is not Postgres bytea text",
                table.name, column
            ))),
        };
    }
    match &row[column] {
        Value::String(s) => Ok(BoundValue::Text(s.clone())),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(BoundValue::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(BoundValue::Float(f))
            } else {
                Ok(BoundValue::Null)
            }
        }
        Value::Bool(b) => Ok(BoundValue::Int(*b as i64)),
        Value::Null => Ok(BoundValue::Null),
        other => Ok(BoundValue::Text(other.to_string())),
    }
}

async fn verify_row_except(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &TableMeta,
    values: &[BoundValue],
    excluded_columns: &[&str],
) -> Result<(), SyncError> {
    let compared = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| !excluded_columns.contains(column))
        .collect::<Vec<_>>();
    let equality = compared
        .iter()
        .enumerate()
        .map(|(parameter_index, (_, column))| format!("{column} IS ?{}", parameter_index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!("SELECT 1 FROM {} WHERE {equality}", table.name);
    let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
    for (index, _) in compared {
        let value = &values[index];
        query = match value {
            BoundValue::Text(value) => query.bind(value.as_str()),
            BoundValue::Int(value) => query.bind(*value),
            BoundValue::Float(value) => query.bind(*value),
            BoundValue::Bytes(value) => query.bind(value.as_slice()),
            BoundValue::Null => query.bind(None::<String>),
        };
    }
    if query.fetch_optional(&mut **transaction).await?.is_none() {
        return Err(SyncError::Local(format!(
            "immutable remote row collided with different local content in {}",
            table.name
        )));
    }
    Ok(())
}

async fn apply_server_enrichment(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &TableMeta,
    values: &[BoundValue],
) -> Result<(), SyncError> {
    let mutable = match table.name {
        "authored_head_proposals" => &["server_proposal_seq"][..],
        "authored_document_archives" => &["final_revision_id", "server_archive_seq"][..],
        other => {
            return Err(SyncError::Parse(format!(
                "missing server-enrichment policy for {other}"
            )))
        }
    };
    verify_row_except(transaction, table, values, mutable).await?;
    update_selected_columns(transaction, table, values, mutable).await
}

async fn apply_terminal_archive(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &TableMeta,
    values: &[BoundValue],
) -> Result<(), SyncError> {
    if table.name != "authored_documents" {
        return Err(SyncError::Parse(format!(
            "missing terminal-archive policy for {}",
            table.name
        )));
    }
    verify_row_except(transaction, table, values, &["archived_at"]).await?;
    // The server is the only archive authority. Its nullable value may leave a
    // locally pending request untouched, while a non-null value is terminal.
    if matches!(
        value_for_column(table, values, "archived_at"),
        BoundValue::Null
    ) {
        return Ok(());
    }
    update_selected_columns(transaction, table, values, &["archived_at"]).await
}

async fn apply_thread_projection(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &TableMeta,
    values: &[BoundValue],
) -> Result<(), SyncError> {
    if table.name != "agent_threads" {
        return Err(SyncError::Parse(format!(
            "missing terminal thread policy for {}",
            table.name
        )));
    }
    let mutable = &["title", "lifecycle_state", "updated_at"];
    verify_row_except(transaction, table, values, mutable).await?;
    let remote_title = value_for_column(table, values, "title");
    let remote_lifecycle = value_for_column(table, values, "lifecycle_state");
    let remote_updated_at = value_for_column(table, values, "updated_at");
    let id = value_for_column(table, values, "id");
    let mut query = sqlx::query(
        "UPDATE agent_threads
         SET title = ?,
             lifecycle_state = CASE
                 WHEN lifecycle_state = 'deleting' OR ? = 'deleting' THEN 'deleting'
                 ELSE 'active'
             END,
             updated_at = ?
         WHERE id IS ?",
    );
    for value in [remote_title, remote_lifecycle, remote_updated_at, id] {
        query = bind_value(query, value);
    }
    if query.execute(&mut **transaction).await?.rows_affected() != 1 {
        return Err(SyncError::Local(
            "failed to apply terminal agent-thread projection".into(),
        ));
    }
    Ok(())
}

async fn update_selected_columns(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &TableMeta,
    values: &[BoundValue],
    columns: &[&str],
) -> Result<(), SyncError> {
    let assignments = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{column} = ?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let pk_columns = table.pk_columns();
    let where_clause = pk_columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{column} IS ?{}", columns.len() + index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "UPDATE {} SET {assignments} WHERE {where_clause}",
        table.name
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for column in columns.iter().chain(pk_columns.iter()) {
        let value = value_for_column(table, values, column);
        query = bind_value(query, value);
    }
    if query.execute(&mut **transaction).await?.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "failed to apply server-owned fields to {}",
            table.name
        )));
    }
    Ok(())
}

fn value_for_column<'a>(
    table: &TableMeta,
    values: &'a [BoundValue],
    column: &str,
) -> &'a BoundValue {
    let index = table
        .columns
        .iter()
        .position(|candidate| *candidate == column)
        .expect("sync policy named a registered column");
    &values[index]
}

fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    value: &'q BoundValue,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments> {
    match value {
        BoundValue::Text(value) => query.bind(value.as_str()),
        BoundValue::Int(value) => query.bind(*value),
        BoundValue::Float(value) => query.bind(*value),
        BoundValue::Bytes(value) => query.bind(value.as_slice()),
        BoundValue::Null => query.bind(None::<String>),
    }
}

fn decode_postgres_bytea(value: &str) -> Result<Vec<u8>, SyncError> {
    let hex = value.strip_prefix("\\x").ok_or_else(|| {
        SyncError::Parse("Postgres bytea value is not in canonical hex format".into())
    })?;
    if hex.len() % 2 != 0 {
        return Err(SyncError::Parse(
            "Postgres bytea hex has an odd number of digits".into(),
        ));
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .map_err(|error| SyncError::Parse(format!("invalid Postgres bytea hex: {error}")))
        })
        .collect()
}

#[cfg(test)]
mod remote_deletion_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::*;
    use crate::agent_execution::{GraphRunStore, PythonWorkspaceService};
    use crate::eval::graph_run::GraphEvaluation;
    use crate::eval::{Plan, ResidentContext};
    use crate::models::agent_threads::CreateAgentThreadInput;
    use crate::services::authored_state::{
        AuthoredRevisionStore, NewAuthoredDocument, RevisionMetadata,
    };
    use crate::storage::StorageRoot;
    use crate::sync::authored_remote::{
        HeadProposalReceipt, HeadProposalStatus, SubmitHeadProposalInput,
    };
    use crate::sync::traits::RemoteClient;

    struct SequenceRowsRemote {
        rows: HashMap<&'static str, Vec<Value>>,
    }

    #[async_trait]
    impl RemoteClient for SequenceRowsRemote {
        async fn select_json(
            &self,
            table: &str,
            query: &str,
            _token: &str,
        ) -> Result<Vec<Value>, SyncError> {
            let cursor = query
                .split_once("sync_seq=gt.")
                .and_then(|(_, remainder)| remainder.split_once('&'))
                .and_then(|(cursor, _)| cursor.parse::<i64>().ok())
                .ok_or_else(|| SyncError::Parse("test pull query has no cursor".into()))?;
            let mut rows = self.rows.get(table).cloned().unwrap_or_default();
            rows.retain(|row| row["sync_seq"].as_i64().is_some_and(|seq| seq > cursor));
            rows.sort_by_key(|row| row["sync_seq"].as_i64().unwrap_or_default());
            Ok(rows)
        }

        async fn upsert_json(
            &self,
            _table: &str,
            _payload: &Value,
            _conflict_key: &str,
            _token: &str,
        ) -> Result<(), SyncError> {
            Err(SyncError::Parse("test remote is read-only".into()))
        }

        async fn patch_json(
            &self,
            _table: &str,
            _filter: &str,
            _payload: &Value,
            _token: &str,
        ) -> Result<(), SyncError> {
            Err(SyncError::Parse("test remote is read-only".into()))
        }

        async fn upload_file(
            &self,
            _bucket: &str,
            _path: &str,
            _bytes: Vec<u8>,
            _content_type: &str,
            _token: &str,
        ) -> Result<String, SyncError> {
            Err(SyncError::Parse("test remote is read-only".into()))
        }

        async fn download_file(
            &self,
            _bucket: &str,
            _path: &str,
            _token: &str,
        ) -> Result<Vec<u8>, SyncError> {
            Err(SyncError::Parse("test remote has no files".into()))
        }
    }

    #[test]
    fn archive_projection_waits_for_a_complete_document_and_fact_pull() {
        let mut unavailable = HashSet::new();
        assert!(archive_reconciliation_is_safe(&unavailable));

        unavailable.insert("authored_documents");
        assert!(!archive_reconciliation_is_safe(&unavailable));

        unavailable.clear();
        unavailable.insert("authored_document_archives");
        assert!(!archive_reconciliation_is_safe(&unavailable));
    }

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

    fn thread_resources(directory: &tempfile::TempDir) -> (PythonWorkspaceService, GraphRunStore) {
        (
            PythonWorkspaceService::new(
                directory.path().join("python-workspaces"),
                Arc::new(|| Err("python is not used by sync pull tests".into())),
            ),
            GraphRunStore::new(),
        )
    }

    fn graph_evaluation() -> Arc<GraphEvaluation> {
        Arc::new(GraphEvaluation {
            plan: Arc::new(Plan {
                ops: Vec::new(),
                slots: Vec::new(),
                slot_channels: Vec::new(),
                n: 0,
                primitive_ids: Vec::new(),
                outputs: Default::default(),
                ctx: ResidentContext {
                    span: (0.0, 1.0),
                    ..Default::default()
                },
                prologue_baked: Vec::new(),
                views: Vec::new(),
            }),
            views: HashMap::new(),
            mel_views: None,
            times_s: Vec::new(),
            primitive_ids: Vec::new(),
            positions: Vec::new(),
            span: (0.0, 1.0),
            graph_hash: "graph".into(),
            arg_hash: "args".into(),
            selection_hash: "selection".into(),
            track_id: "track".into(),
            venue_id: "venue".into(),
            universe_state: None,
        })
    }

    #[tokio::test]
    async fn deleting_projection_before_receipt_recovers_with_the_canonical_timestamp() {
        let (directory, pool, authored) = test_pool().await;
        insert_score_fixture(&pool).await;
        let thread = authored
            .create_thread_with_authored_state(
                &pool,
                CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: "track_copilot".into(),
                    subject_kind: Some("track".into()),
                    subject_id: Some("track".into()),
                    venue_id: Some("venue".into()),
                    score_id: Some("score".into()),
                    ..Default::default()
                },
                Some("alice"),
            )
            .await
            .unwrap();
        let document_id: String = sqlx::query_scalar(
            "SELECT document_id FROM authored_documents
             WHERE principal_key = 'signed-in:alice'
               AND document_kind = 'track_score' AND track_id = 'track'
               AND venue_id = 'venue' AND score_id = 'score'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "DELETE FROM pending_ops
             WHERE table_name = 'agent_threads' AND record_id = ?",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .unwrap();

        let deleted_at = "2026-08-02T00:00:02Z";
        let projection_only = SequenceRowsRemote {
            rows: HashMap::from([(
                "agent_threads",
                vec![json!({
                    "id": thread.id,
                    "owner_user_id": "alice",
                    "agent_kind": "track_copilot",
                    "subject_kind": "track",
                    "subject_id": "track",
                    "implementation_id": null,
                    "venue_id": "venue",
                    "score_id": "score",
                    "title": null,
                    "lifecycle_state": "deleting",
                    "forked_from_thread_id": null,
                    "forked_at_message_id": null,
                    "created_at": thread.created_at,
                    "updated_at": deleted_at,
                    "sync_seq": 100
                })],
            )]),
        };
        let (workspaces, graph_runs) = thread_resources(&directory);
        let first = pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &projection_only,
            "token",
            Some("alice"),
        )
        .await
        .unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT deleted_at FROM agent_thread_deletions WHERE thread_id = ?",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            deleted_at
        );

        let canonical_receipt = SequenceRowsRemote {
            rows: HashMap::from([(
                "agent_thread_deletions",
                vec![json!({
                    "thread_id": thread.id,
                    "owner_user_id": "alice",
                    "principal_key": "signed-in:alice",
                    "document_id": document_id,
                    "deleted_at": deleted_at,
                    "sync_seq": 120
                })],
            )]),
        };
        let second = pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &canonical_receipt,
            "token",
            Some("alice"),
        )
        .await
        .unwrap();
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(second.rows_pulled, 1);
    }

    #[tokio::test]
    async fn pulled_thread_deletion_finishes_cleanup_and_preserves_immutable_trace() {
        let (directory, pool, authored) = test_pool().await;
        insert_score_fixture(&pool).await;
        let thread = authored
            .create_thread_with_authored_state(
                &pool,
                CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: "track_copilot".into(),
                    subject_kind: Some("track".into()),
                    subject_id: Some("track".into()),
                    venue_id: Some("venue".into()),
                    score_id: Some("score".into()),
                    ..Default::default()
                },
                Some("alice"),
            )
            .await
            .unwrap();
        let document_id: String = sqlx::query_scalar(
            "SELECT document_id FROM authored_documents
             WHERE principal_key = 'signed-in:alice'
               AND document_kind = 'track_score' AND track_id = 'track'
               AND venue_id = 'venue' AND score_id = 'score'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let (workspaces, graph_runs) = thread_resources(&directory);
        let python_workspace = workspaces.workspace_for_test(&thread.id).unwrap();
        let python_workspace_path = python_workspace.dir().to_owned();
        drop(python_workspace);
        graph_runs.publish_for_test(&thread.id, graph_evaluation());
        assert!(python_workspace_path.is_dir());
        assert!(graph_runs.latest(&thread.id).is_some());

        // Leave the local thread snapshot dirty on purpose. The immutable
        // deletion receipt must still outrank it and cannot be skipped by the
        // ordinary local-dirty pull rule.
        let remote_message_id = "remote-terminal-trace";
        let remote = SequenceRowsRemote {
            rows: HashMap::from([
                (
                    "agent_threads",
                    vec![json!({
                        "id": thread.id,
                        "owner_user_id": "alice",
                        "agent_kind": "track_copilot",
                        "subject_kind": "track",
                        "subject_id": "track",
                        "implementation_id": null,
                        "venue_id": "venue",
                        "score_id": "score",
                        "title": null,
                        "lifecycle_state": "deleting",
                        "forked_from_thread_id": null,
                        "forked_at_message_id": null,
                        "created_at": thread.created_at,
                        "updated_at": "2026-08-02T00:00:02Z",
                        "sync_seq": 100
                    })],
                ),
                (
                    "agent_thread_messages",
                    vec![json!({
                        "id": remote_message_id,
                        "owner_user_id": "alice",
                        "principal_key": "signed-in:alice",
                        "created_in_thread_id": thread.id,
                        "parent_message_id": null,
                        "depth": 0,
                        "role": "user",
                        "parts_json": json!([{
                            "type": "text",
                            "text": "preserved remote trace"
                        }]).to_string(),
                        "created_at": "2026-08-02T00:00:01Z",
                        "sync_seq": 110
                    })],
                ),
                (
                    "agent_thread_deletions",
                    vec![json!({
                        "thread_id": thread.id,
                        "owner_user_id": "alice",
                        "principal_key": "signed-in:alice",
                        "document_id": document_id,
                        "deleted_at": "2026-08-02T00:00:02Z",
                        "sync_seq": 120
                    })],
                ),
            ]),
        };

        let stats = pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &remote,
            "token",
            Some("alice"),
        )
        .await
        .unwrap();
        assert!(stats.errors.is_empty(), "{:?}", stats.errors);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_threads WHERE id = ?")
                .bind(&thread.id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_deletions WHERE thread_id = ?",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT parts_json FROM agent_thread_messages WHERE id = ?",
            )
            .bind(remote_message_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            json!([{
                "type": "text",
                "text": "preserved remote trace"
            }])
            .to_string()
        );
        assert!(!python_workspace_path.exists());
        assert!(workspaces.workspace_for_test(&thread.id).is_err());
        assert!(graph_runs.latest(&thread.id).is_none());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE table_name = 'agent_threads' AND record_id = ?",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "the pulled terminal receipt supersedes a dirty thread projection"
        );

        let replay = pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &remote,
            "token",
            Some("alice"),
        )
        .await
        .unwrap();
        assert!(replay.errors.is_empty(), "{:?}", replay.errors);
        assert_eq!(replay.rows_pulled, 0);
    }

    #[tokio::test]
    async fn remote_archive_can_converge_a_losing_timestamp_but_never_clear_it() {
        let (_directory, pool, _authored) = test_pool().await;
        let document_id = format!("ad-{}", "a".repeat(64));
        sqlx::query(
            "INSERT INTO authored_documents
             (document_id, document_kind, principal_key, subject_id, track_id,
              venue_id, score_id, archived_at)
             VALUES (?, 'track_score', 'signed-in:alice', 'track', 'track',
                     'venue', 'score', '2026-08-02T00:00:01Z')",
        )
        .bind(&document_id)
        .execute(&pool)
        .await
        .unwrap();

        assert!(sqlx::query(
            "UPDATE authored_documents SET archived_at = '2026-08-02T00:00:02Z'
             WHERE document_id = ?",
        )
        .bind(&document_id)
        .execute(&pool)
        .await
        .is_err());

        let mut remote = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut remote)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE authored_documents SET archived_at = '2026-08-02T00:00:02Z'
             WHERE document_id = ?",
        )
        .bind(&document_id)
        .execute(&mut *remote)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut remote)
            .await
            .unwrap();
        remote.commit().await.unwrap();

        let mut forbidden = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut forbidden)
            .await
            .unwrap();
        assert!(sqlx::query(
            "UPDATE authored_documents SET archived_at = NULL WHERE document_id = ?",
        )
        .bind(&document_id)
        .execute(&mut *forbidden)
        .await
        .is_err());
        forbidden.rollback().await.unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT archived_at FROM authored_documents WHERE document_id = ?",
            )
            .bind(&document_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "2026-08-02T00:00:02Z"
        );
    }

    #[tokio::test]
    async fn proposal_receipt_then_pull_replay_is_idempotent_and_advances_the_cursor() {
        let (_directory, pool, authored) = test_pool().await;
        insert_score_fixture(&pool).await;
        let document =
            NewAuthoredDocument::track_score("signed-in:alice", "track", "venue", "score").unwrap();
        let store = AuthoredRevisionStore;
        let mut connection = pool.acquire().await.unwrap();
        store
            .insert_document(&mut connection, &document)
            .await
            .unwrap();
        let revision = store
            .insert_revision(
                &mut connection,
                &document.id,
                &[],
                &std::collections::BTreeMap::from([(
                    "score.luma".to_owned(),
                    b"version = 1\n".to_vec(),
                )]),
                &RevisionMetadata {
                    operation_kind: "initial_import".into(),
                    operation_id: None,
                    message: "Import".into(),
                    author_name: "Luma".into(),
                    author_email: "test@luma.local".into(),
                    authored_at: "2026-08-02T00:00:00Z".into(),
                    thread_id: None,
                    assistant_message_id: None,
                    restored_revision_id: None,
                },
            )
            .await
            .unwrap();
        store
            .create_head(&mut connection, &document.id, &revision.id)
            .await
            .unwrap();
        let input = SubmitHeadProposalInput {
            proposal_id: "proposal-receipt-replay".into(),
            document_id: document.id.to_string(),
            device_id: "device-a".into(),
            operation_id: "operation-a".into(),
            base_revision_id: Some(revision.id.to_string()),
            proposed_revision_id: revision.id.to_string(),
            created_at: "2026-08-02T00:00:01Z".into(),
        };
        sqlx::query(
            "INSERT INTO authored_head_proposals
             (proposal_id, principal_key, document_id, device_id, operation_id,
              base_revision_id, proposed_revision_id, created_at)
             VALUES (?, 'signed-in:alice', ?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.proposal_id)
        .bind(&input.document_id)
        .bind(&input.device_id)
        .bind(&input.operation_id)
        .bind(&input.base_revision_id)
        .bind(&input.proposed_revision_id)
        .bind(&input.created_at)
        .execute(&mut *connection)
        .await
        .unwrap();
        drop(connection);

        let receipt = HeadProposalReceipt {
            proposal_id: input.proposal_id.clone(),
            document_id: input.document_id.clone(),
            proposal_seq: 41,
            status: HeadProposalStatus::Pending,
            base_revision_id: input.base_revision_id.clone(),
            proposed_revision_id: input.proposed_revision_id.clone(),
            current_head_revision_id: Some(revision.id.to_string()),
            is_earliest_pending: true,
        };
        crate::sync::push::apply_proposal_receipt(&pool, "alice", &input, &receipt)
            .await
            .unwrap();

        let remote = SequenceRowsRemote {
            rows: HashMap::from([(
                "authored_head_proposals",
                vec![json!({
                    "proposal_id": input.proposal_id,
                    "principal_key": "signed-in:alice",
                    "document_id": input.document_id,
                    "device_id": input.device_id,
                    "operation_id": input.operation_id,
                    "base_revision_id": input.base_revision_id,
                    "proposed_revision_id": input.proposed_revision_id,
                    "server_proposal_seq": 41,
                    "created_at": input.created_at,
                    "sync_seq": 77
                })],
            )]),
        };
        let table = registry::get_table("authored_head_proposals").unwrap();
        assert_eq!(
            pull_table(&pool, &authored, &remote, table, "token", Some("alice"))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            state::get_last_pulled_seq(&pool, "alice", "authored_head_proposals")
                .await
                .unwrap(),
            77
        );
        assert_eq!(
            pull_table(&pool, &authored, &remote, table, "token", Some("alice"))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT server_proposal_seq FROM authored_head_proposals
                 WHERE proposal_id = 'proposal-receipt-replay'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            41
        );
    }

    #[tokio::test]
    async fn archived_proposal_trace_cannot_block_a_later_live_document_proposal() {
        let (directory, pool, authored) = test_pool().await;
        let (workspaces, graph_runs) = thread_resources(&directory);
        insert_score_fixture(&pool).await;
        let store = AuthoredRevisionStore;
        let archived_document =
            NewAuthoredDocument::track_score("signed-in:alice", "track", "venue", "score").unwrap();
        let live_document =
            NewAuthoredDocument::pattern_graph("signed-in:alice", "pattern", "implementation")
                .unwrap();
        let metadata = |operation_id: &str, authored_at: &str| RevisionMetadata {
            operation_kind: "score_edit".into(),
            operation_id: Some(operation_id.into()),
            message: operation_id.into(),
            author_name: "Luma".into(),
            author_email: "test@luma.local".into(),
            authored_at: authored_at.into(),
            thread_id: None,
            assistant_message_id: None,
            restored_revision_id: None,
        };
        let mut connection = pool.acquire().await.unwrap();
        store
            .insert_document(&mut connection, &archived_document)
            .await
            .unwrap();
        store
            .insert_document(&mut connection, &live_document)
            .await
            .unwrap();
        let archived_root = store
            .insert_revision(
                &mut connection,
                &archived_document.id,
                &[],
                &std::collections::BTreeMap::from([(
                    "score.luma".to_owned(),
                    b"version = 1\n".to_vec(),
                )]),
                &metadata("archived-root", "2026-08-02T00:00:00Z"),
            )
            .await
            .unwrap();
        let archived_tip = store
            .insert_revision(
                &mut connection,
                &archived_document.id,
                std::slice::from_ref(&archived_root.id),
                &std::collections::BTreeMap::from([(
                    "score.luma".to_owned(),
                    b"version = 1\nclip = 1\n".to_vec(),
                )]),
                &metadata("archived-tip", "2026-08-02T00:00:01Z"),
            )
            .await
            .unwrap();
        store
            .create_head(&mut connection, &archived_document.id, &archived_root.id)
            .await
            .unwrap();
        store
            .archive_document(
                &mut connection,
                &archived_document.id,
                &archived_root.id,
                "2026-08-02T00:00:02Z",
            )
            .await
            .unwrap();

        let live_root = store
            .insert_revision(
                &mut connection,
                &live_document.id,
                &[],
                &std::collections::BTreeMap::from([(
                    "graph.json".to_owned(),
                    br#"{"nodes":[],"edges":[],"args":[]}"#.to_vec(),
                )]),
                &metadata("live-root", "2026-08-02T00:00:00Z"),
            )
            .await
            .unwrap();
        let live_tip = store
            .insert_revision(
                &mut connection,
                &live_document.id,
                std::slice::from_ref(&live_root.id),
                &std::collections::BTreeMap::from([(
                    "graph.json".to_owned(),
                    br#"{"nodes":[{"id":"pulse"}],"edges":[],"args":[]}"#.to_vec(),
                )]),
                &metadata("live-tip", "2026-08-02T00:00:01Z"),
            )
            .await
            .unwrap();
        store
            .create_head(&mut connection, &live_document.id, &live_root.id)
            .await
            .unwrap();
        drop(connection);

        let archived_proposal_id = "archived-proposal";
        let live_proposal_id = "later-live-proposal";
        let remote = SequenceRowsRemote {
            rows: HashMap::from([
                (
                    "authored_head_proposals",
                    vec![
                        json!({
                            "proposal_id": archived_proposal_id,
                            "principal_key": "signed-in:alice",
                            "document_id": archived_document.id.to_string(),
                            "device_id": "offline-device",
                            "operation_id": "archived-operation",
                            "base_revision_id": archived_root.id.to_string(),
                            "proposed_revision_id": archived_tip.id.to_string(),
                            "server_proposal_seq": 10,
                            "created_at": "2026-08-02T00:00:01Z",
                            "sync_seq": 50
                        }),
                        json!({
                            "proposal_id": live_proposal_id,
                            "principal_key": "signed-in:alice",
                            "document_id": live_document.id.to_string(),
                            "device_id": "online-device",
                            "operation_id": "live-operation",
                            "base_revision_id": live_root.id.to_string(),
                            "proposed_revision_id": live_tip.id.to_string(),
                            "server_proposal_seq": 11,
                            "created_at": "2026-08-02T00:00:03Z",
                            "sync_seq": 60
                        }),
                    ],
                ),
                (
                    "authored_head_integrations",
                    vec![json!({
                        "proposal_id": archived_proposal_id,
                        "principal_key": "signed-in:alice",
                        "document_id": archived_document.id.to_string(),
                        "prior_revision_id": null,
                        "result_revision_id": null,
                        "resolution_kind": "cancelled_archived",
                        "server_integration_seq": 12,
                        "integrated_at": "2026-08-02T00:00:04Z",
                        "sync_seq": 70
                    })],
                ),
            ]),
        };

        let first = pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &remote,
            "token",
            Some("alice"),
        )
        .await
        .unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM authored_head_proposals
                 WHERE proposal_id IN ('archived-proposal', 'later-live-proposal')",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT resolution_kind FROM authored_head_integrations
                 WHERE proposal_id = 'archived-proposal'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "cancelled_archived"
        );
        assert_eq!(
            state::get_last_pulled_seq(&pool, "alice", "authored_head_proposals")
                .await
                .unwrap(),
            60
        );
        assert_eq!(
            state::get_last_pulled_seq(&pool, "alice", "authored_head_integrations")
                .await
                .unwrap(),
            70
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE op_type = 'integrate_authored_head_proposal'
                   AND record_id = 'later-live-proposal'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1,
            "any online client must schedule the later live proposal"
        );

        let replay = pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &remote,
            "token",
            Some("alice"),
        )
        .await
        .unwrap();
        assert!(replay.errors.is_empty(), "{:?}", replay.errors);
        assert_eq!(replay.rows_pulled, 0);
        let mut history_connection = pool.acquire().await.unwrap();
        assert_eq!(
            store
                .read_revision(
                    &mut history_connection,
                    &archived_document.id,
                    &archived_tip.id,
                )
                .await
                .unwrap()
                .1["score.luma"],
            b"version = 1\nclip = 1\n"
        );
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
    async fn legacy_projection_and_routing_rows_have_no_sync_route() {
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
            "INSERT INTO authored_documents
             (document_id, document_kind, principal_key, subject_id,
              track_id, venue_id, score_id)
             VALUES ('ad-score', 'track_score', 'signed-in:alice', 'track',
                     'track', 'venue', 'score')",
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
        assert!(!score_error.is_empty());
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
                "SELECT COUNT(*) FROM authored_documents
                 WHERE document_kind = 'track_score' AND score_id = 'foreign-score'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "an empty foreign cache must not manufacture an authored document"
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
    async fn foreign_public_pattern_tombstone_deletes_empty_unauthored_cache() {
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
                "SELECT COUNT(*) FROM authored_documents
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
    async fn generic_score_upserts_cannot_rebind_authored_document_identity() {
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
    async fn pattern_tombstone_requires_empty_graphs_and_no_authored_history() {
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
            "INSERT INTO authored_documents
             (document_id, document_kind, principal_key, subject_id,
              implementation_id)
             VALUES ('ad-pattern', 'pattern_graph', 'signed-in:alice', 'pattern',
                     'implementation')",
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

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM implementations WHERE id = 'implementation'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
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

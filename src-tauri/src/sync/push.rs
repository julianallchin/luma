//! Push protocol: background sync loop with exponential backoff.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::{watch, Mutex, Notify};

use crate::services::authored_documents::AuthoredDocuments;

use super::authored_remote::{
    self, ArchiveAuthoredDocumentInput, HeadProposalIntegrator, SubmitHeadProposalInput,
    ARCHIVE_AUTHORED_DOCUMENT_OP, INTEGRATE_HEAD_PROPOSAL_OP, SUBMIT_HEAD_PROPOSAL_OP,
};
use super::error::SyncError;
use super::host::SyncHost;
use super::pending::{self, PendingOp};
use super::pull;
use super::registry;
use super::traits::RemoteClient;

/// Flush ready pending ops to the remote. Returns count flushed.
#[cfg(test)]
pub async fn flush_pending(
    pool: &SqlitePool,
    state_pool: &SqlitePool,
    remote: &dyn RemoteClient,
) -> Result<usize, SyncError> {
    flush_pending_with_integrator(pool, state_pool, remote, None).await
}

/// Flush pending operations with the domain-aware authored-head integration
/// bridge installed. Production callers always provide `AuthoredDocuments`;
/// tests may omit it to exercise ordinary row delivery in isolation.
pub async fn flush_pending_with_integrator(
    pool: &SqlitePool,
    state_pool: &SqlitePool,
    remote: &dyn RemoteClient,
    integrator: Option<&dyn HeadProposalIntegrator>,
) -> Result<usize, SyncError> {
    let admitted_user_id = crate::database::local::auth::admitted_principal(pool)
        .await
        .map_err(SyncError::Local)?
        .ok_or(SyncError::AuthRequired)?;
    let (token, token_user_id) = get_auth(state_pool).await?;
    if token_user_id != admitted_user_id {
        return Err(SyncError::Local(
            "verified remote session does not match the active app-database principal".into(),
        ));
    }
    let principal_key = crate::database::local::auth::principal_key(Some(&admitted_user_id));
    let ops = pending::fetch_ready_ops(pool, &principal_key).await?;
    let mut flushed = 0;

    for op in &ops {
        eprintln!(
            "[sync] push {} {}.{} (attempt {})",
            op.op_type, op.table_name, op.record_id, op.attempts
        );
        match execute_op(pool, remote, op, &token, &admitted_user_id, integrator).await {
            Ok(()) => {
                if op.op_type == "upsert" {
                    // Mark synced first so if remove_op fails the record is
                    // at least marked clean and won't be re-pushed.
                    mark_synced(pool, op, &admitted_user_id).await?;
                }
                pending::remove_op(pool, op).await?;
                flushed += 1;
            }
            Err(SyncError::Api { status: 401, .. }) => {
                eprintln!("[sync] 401 — stopping batch for token refresh");
                return Err(SyncError::Api {
                    status: 401,
                    message: "token expired".into(),
                });
            }
            Err(SyncError::Api {
                status: 409,
                ref message,
            }) => {
                // A conflict is not proof that the remote row is identical to
                // the local payload. Treating arbitrary 409s as success loses
                // data on unique-key or immutable-row violations. Keep the op
                // queued; FK conflicts and content conflicts both need either
                // their dependency or the underlying divergence resolved.
                let kind = if message.contains("23503") {
                    "FK conflict"
                } else {
                    "conflict"
                };
                eprintln!(
                    "[sync] 409 {kind} {}.{} — requeueing for retry: {message}",
                    op.table_name, op.record_id
                );
                if op.op_type == INTEGRATE_HEAD_PROPOSAL_OP {
                    pending::record_integration_retry(pool, op, message).await?;
                } else {
                    pending::record_failure(pool, op, op.attempts + 1, message).await?;
                }
            }
            Err(e @ SyncError::Network(_)) => {
                // Offline — propagate immediately so the loop can back off.
                // Do not touch attempts: network errors never count against MAX_ATTEMPTS.
                return Err(e);
            }
            Err(e) => {
                let msg = format!("{e:?}");
                eprintln!(
                    "[sync] Push failed {}.{}: {msg}",
                    op.table_name, op.record_id
                );
                if op.op_type == INTEGRATE_HEAD_PROPOSAL_OP {
                    pending::record_integration_retry(pool, op, &msg).await?;
                } else {
                    pending::record_failure(pool, op, op.attempts + 1, &msg).await?;
                }
            }
        }
    }

    Ok(flushed)
}

async fn execute_op(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    op: &PendingOp,
    token: &str,
    admitted_user_id: &str,
    integrator: Option<&dyn HeadProposalIntegrator>,
) -> Result<(), SyncError> {
    match op.op_type.as_str() {
        SUBMIT_HEAD_PROPOSAL_OP => {
            let input: SubmitHeadProposalInput = parse_pending_payload(op)?;
            let receipt = authored_remote::submit_head_proposal(remote, &input, token).await?;
            apply_proposal_receipt(pool, admitted_user_id, &input, &receipt).await?;
            return Ok(());
        }
        ARCHIVE_AUTHORED_DOCUMENT_OP => {
            let input: ArchiveAuthoredDocumentInput = parse_pending_payload(op)?;
            let receipt = authored_remote::archive_authored_document(remote, &input, token).await?;
            apply_archive_receipt(pool, admitted_user_id, &input, &receipt).await?;
            return Ok(());
        }
        INTEGRATE_HEAD_PROPOSAL_OP => {
            let payload: IntegrateWakeup = parse_pending_payload(op)?;
            if payload.proposal_id != op.record_id {
                return Err(SyncError::Parse(
                    "authored integration wake-up identity does not match its payload".into(),
                ));
            }
            let integrator = integrator.ok_or_else(|| {
                SyncError::Local(format!(
                    "authored proposal {} requires the domain integration hook",
                    payload.proposal_id
                ))
            })?;
            let receipt = integrator
                .integrate_pending_proposal(
                    pool,
                    remote,
                    token,
                    admitted_user_id,
                    &payload.proposal_id,
                )
                .await?;
            if !receipt.is_terminal() {
                return Err(SyncError::Local(format!(
                    "authored proposal {} was not terminal after integration ({:?}); recompute against the latest server head",
                    payload.proposal_id, receipt.outcome
                )));
            }
            return Ok(());
        }
        _ => {}
    }
    let table = registry::get_table(&op.table_name).ok_or_else(|| {
        SyncError::Parse(format!(
            "table {:?} is not registered for relational sync",
            op.table_name
        ))
    })?;
    match op.op_type.as_str() {
        "upsert" | pending::INSERT_IMMUTABLE_OP | pending::EXPLICIT_UPSERT_OP => {
            if op.conflict_key != table.conflict_key {
                return Err(SyncError::Parse(format!(
                    "queued conflict key {:?} does not match {} for table {:?}",
                    op.conflict_key, table.conflict_key, op.table_name
                )));
            }
            let payload: Value = serde_json::from_str(
                op.payload_json
                    .as_deref()
                    .ok_or_else(|| SyncError::MissingField("payload_json".into()))?,
            )
            .map_err(|e| SyncError::Parse(e.to_string()))?;
            if !table.payload_principal_matches(&payload, admitted_user_id) {
                return Err(SyncError::Local(format!(
                    "queued payload principal does not match the active app principal {admitted_user_id:?}"
                )));
            }
            let expected_policy = match op.op_type.as_str() {
                "upsert" => registry::PushPolicy::DirtyUpsert,
                pending::INSERT_IMMUTABLE_OP => registry::PushPolicy::ExplicitImmutable,
                pending::EXPLICIT_UPSERT_OP => registry::PushPolicy::ExplicitUpsert,
                _ => unreachable!("matched above"),
            };
            if registry::push_policy(&op.table_name) != expected_policy {
                return Err(SyncError::Parse(format!(
                    "pending operation policy does not match sync registry for {:?}",
                    op.table_name
                )));
            }
            if expected_policy == registry::PushPolicy::ExplicitImmutable {
                remote
                    .insert_immutable_json(&op.table_name, &payload, &op.conflict_key, token)
                    .await?;
                if table.name == "agent_thread_message_appends" {
                    reconcile_transcript_head_after_append(
                        pool,
                        remote,
                        token,
                        admitted_user_id,
                        &payload,
                    )
                    .await?;
                }
                Ok(())
            } else {
                remote
                    .upsert_json(&op.table_name, &payload, &op.conflict_key, token)
                    .await
            }
        }
        "delete" => {
            // Soft-delete: PATCH deleted_at on the existing remote row.
            // Uses PATCH (not upsert) because upsert's INSERT half fails
            // NOT NULL constraints when sending only PK + deleted_at.
            let pk_cols = table.pk_columns();
            let pk_values = table.decode_record_id(&op.record_id);

            // Build PostgREST filter: "col1=eq.val1&col2=eq.val2"
            let filter: Vec<String> = pk_cols
                .iter()
                .zip(pk_values.iter())
                .map(|(col, val)| format!("{col}=eq.{val}"))
                .collect();
            let filter = filter.join("&");

            let payload = serde_json::json!({
                "deleted_at": chrono::Utc::now().to_rfc3339(),
            });
            remote
                .patch_json(&op.table_name, &filter, &payload, token)
                .await
        }
        other => Err(SyncError::Parse(format!("unknown op_type: {other}"))),
    }
}

async fn reconcile_transcript_head_after_append(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    token: &str,
    admitted_user_id: &str,
    append: &Value,
) -> Result<(), SyncError> {
    let thread_id = append
        .get("thread_id")
        .and_then(Value::as_str)
        .ok_or_else(|| SyncError::MissingField("thread_id".into()))?;
    let encoded_thread_id = percent_encode_filter_value(thread_id);
    let rows = remote
        .select_json(
            "agent_thread_transcript_heads",
            &format!(
                "thread_id=eq.{encoded_thread_id}&select=thread_id,owner_user_id,head_message_id,message_count,updated_at&limit=2"
            ),
            token,
        )
        .await?;
    if rows.len() != 1 {
        return Err(SyncError::Parse(format!(
            "server returned {} transcript heads for accepted append on thread {thread_id}",
            rows.len()
        )));
    }
    pull::apply_agent_transcript_head_observation(pool, &rows[0], admitted_user_id, thread_id).await
}

fn percent_encode_filter_value(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn parse_pending_payload<T: serde::de::DeserializeOwned>(op: &PendingOp) -> Result<T, SyncError> {
    serde_json::from_str(
        op.payload_json
            .as_deref()
            .ok_or_else(|| SyncError::MissingField("payload_json".into()))?,
    )
    .map_err(|error| {
        SyncError::Parse(format!(
            "invalid payload for pending operation {}.{}: {error}",
            op.op_type, op.record_id
        ))
    })
}

#[derive(serde::Deserialize)]
struct IntegrateWakeup {
    proposal_id: String,
}

pub(super) async fn apply_proposal_receipt(
    pool: &SqlitePool,
    admitted_user_id: &str,
    input: &SubmitHeadProposalInput,
    receipt: &authored_remote::HeadProposalReceipt,
) -> Result<(), SyncError> {
    if receipt.proposal_id != input.proposal_id || receipt.document_id != input.document_id {
        return Err(SyncError::Parse(
            "head-proposal RPC returned a receipt for a different identity".into(),
        ));
    }
    let principal_key = crate::database::local::auth::principal_key(Some(admitted_user_id));
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT server_proposal_seq FROM authored_head_proposals
         WHERE proposal_id = ? AND principal_key = ? AND document_id = ?",
    )
    .bind(&input.proposal_id)
    .bind(&principal_key)
    .bind(&input.document_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    if existing == Some(receipt.proposal_seq) {
        return Ok(());
    }
    if existing.is_some() {
        return Err(SyncError::Local(format!(
            "proposal {} is bound to a different server sequence",
            input.proposal_id
        )));
    }
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    let updated = sqlx::query(
        "UPDATE authored_head_proposals SET server_proposal_seq = ?
         WHERE proposal_id = ? AND principal_key = ? AND document_id = ?
           AND device_id = ? AND operation_id = ?
           AND base_revision_id IS ? AND proposed_revision_id = ?
           AND created_at = ? AND server_proposal_seq IS NULL",
    )
    .bind(receipt.proposal_seq)
    .bind(&input.proposal_id)
    .bind(&principal_key)
    .bind(&input.document_id)
    .bind(&input.device_id)
    .bind(&input.operation_id)
    .bind(&input.base_revision_id)
    .bind(&input.proposed_revision_id)
    .bind(&input.created_at)
    .execute(&mut *transaction)
    .await?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    if updated.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "proposal {} receipt does not match its local immutable input",
            input.proposal_id
        )));
    }
    transaction.commit().await?;
    // Submission is not integration. Queue the same content-free wake-up an
    // unrelated owner device would create after pulling this proposal so a
    // permanently offline author can never be required for progress.
    pending::enqueue_head_integration(pool, admitted_user_id, &input.proposal_id).await?;
    Ok(())
}

pub(super) async fn apply_archive_receipt(
    pool: &SqlitePool,
    admitted_user_id: &str,
    input: &ArchiveAuthoredDocumentInput,
    receipt: &authored_remote::ArchiveAuthoredDocumentReceipt,
) -> Result<(), SyncError> {
    if receipt.archive_id != input.archive_id || receipt.document_id != input.document_id {
        return Err(SyncError::Parse(
            "archive RPC returned a receipt for a different identity".into(),
        ));
    }
    let principal_key = crate::database::local::auth::principal_key(Some(admitted_user_id));
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    let current: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT final_revision_id, server_archive_seq
         FROM authored_document_archives
         WHERE archive_id = ? AND principal_key = ? AND document_id = ?",
    )
    .bind(&input.archive_id)
    .bind(&principal_key)
    .bind(&input.document_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let local_final_revision = match receipt.final_revision_id.as_deref() {
        None => None,
        Some(revision_id) => {
            let exists: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM authored_revisions
                 WHERE revision_id = ? AND document_id = ? AND principal_key = ?",
            )
            .bind(revision_id)
            .bind(&input.document_id)
            .bind(&principal_key)
            .fetch_optional(&mut *transaction)
            .await?;
            exists.map(|_| revision_id.to_owned())
        }
    };
    match current {
        Some((final_revision, Some(sequence))) if sequence == receipt.archive_seq => {
            if final_revision.is_some() && final_revision != receipt.final_revision_id {
                return Err(SyncError::Local(format!(
                    "archive {} is bound to a different server outcome",
                    input.archive_id
                )));
            }
            if final_revision.is_none() {
                if let Some(local_final_revision) = &local_final_revision {
                    sqlx::query(
                        "UPDATE authored_document_archives SET final_revision_id = ?
                         WHERE archive_id = ? AND principal_key = ? AND document_id = ?
                           AND server_archive_seq = ? AND final_revision_id IS NULL",
                    )
                    .bind(local_final_revision)
                    .bind(&input.archive_id)
                    .bind(&principal_key)
                    .bind(&input.document_id)
                    .bind(receipt.archive_seq)
                    .execute(&mut *transaction)
                    .await?;
                }
            }
        }
        Some((None, None)) => {
            let updated = sqlx::query(
                "UPDATE authored_document_archives
                 SET final_revision_id = ?, server_archive_seq = ?
                 WHERE archive_id = ? AND principal_key = ? AND document_id = ?
                   AND device_id = ? AND operation_id = ?
                   AND requested_revision_id IS ? AND archived_at = ?
                   AND final_revision_id IS NULL AND server_archive_seq IS NULL",
            )
            .bind(&local_final_revision)
            .bind(receipt.archive_seq)
            .bind(&input.archive_id)
            .bind(&principal_key)
            .bind(&input.document_id)
            .bind(&input.device_id)
            .bind(&input.operation_id)
            .bind(&input.requested_revision_id)
            .bind(&input.archived_at)
            .execute(&mut *transaction)
            .await?;
            if updated.rows_affected() != 1 {
                return Err(SyncError::Local(format!(
                    "archive {} receipt does not match its local immutable input",
                    input.archive_id
                )));
            }
        }
        _ => {
            return Err(SyncError::Local(format!(
                "archive {} is bound to a different server outcome",
                input.archive_id
            )))
        }
    }
    let archived = sqlx::query(
        "UPDATE authored_documents SET archived_at = ?
         WHERE document_id = ? AND principal_key = ?",
    )
    .bind(&receipt.document_archived_at)
    .bind(&input.document_id)
    .bind(&principal_key)
    .execute(&mut *transaction)
    .await?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    if archived.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "archive {} cannot apply to its local document",
            input.archive_id
        )));
    }
    transaction.commit().await?;
    Ok(())
}

/// Mark a record as synced using TableMeta-derived SQL.
async fn mark_synced(
    pool: &SqlitePool,
    op: &PendingOp,
    admitted_user_id: &str,
) -> Result<(), SyncError> {
    let Some(table) = registry::get_table(&op.table_name) else {
        return Err(SyncError::Parse(format!(
            "table {:?} is not registered for relational sync",
            op.table_name
        )));
    };
    if !table.has_principal() {
        return Err(SyncError::Local(format!(
            "sync table {:?} has no principal column",
            op.table_name
        )));
    }
    let principal_guard = if table.columns.contains(&"uid") {
        "uid = ?"
    } else {
        "principal_key = 'signed-in:' || ?"
    };
    let sql = format!(
        "{} AND {principal_guard} AND EXISTS (
             SELECT 1 FROM auth_write_admission AS admission
             WHERE admission.singleton = 1
               AND admission.armed = 1
               AND admission.accepting = 1
               AND admission.maintenance = 0
               AND admission.remote_writes = 0
               AND admission.active_uid = ?
         )",
        table.mark_synced_sql()
    );
    let pk_values = table.decode_record_id(&op.record_id);
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for val in &pk_values {
        query = query.bind(*val);
    }
    query = query.bind(admitted_user_id).bind(admitted_user_id);
    let result = query.execute(pool).await?;
    if result.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "refusing to mark {}.{} synced: the row is not owned by the active app principal",
            op.table_name, op.record_id
        )));
    }
    Ok(())
}

/// Background sync loop: push dirty every 10s, full pull+files every 60s.
/// Accepts a shutdown receiver for graceful termination.
pub async fn run_sync_loop(
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<dyn RemoteClient>,
    notify: Arc<Notify>,
    sync_lock: Arc<Mutex<()>>,
    authored: AuthoredDocuments,
    host: SyncHost,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut pull_interval = tokio::time::interval(Duration::from_secs(60));
    pull_interval.tick().await; // skip immediate first tick
    let mut auth_backoff: Option<tokio::time::Instant> = None;
    let mut offline_until: Option<tokio::time::Instant> = None;

    loop {
        let is_pull_tick;
        tokio::select! {
            _ = shutdown.changed() => {
                println!("[sync] Shutting down sync loop");
                return;
            }
            _ = notify.notified() => { is_pull_tick = false; }
            _ = pull_interval.tick() => { is_pull_tick = true; }
            _ = tokio::time::sleep(Duration::from_secs(10)) => { is_pull_tick = false; }
        }

        // If we recently got a 401, back off before retrying.
        if let Some(until) = auth_backoff {
            if tokio::time::Instant::now() < until {
                continue;
            }
            auth_backoff = None;
        }

        // If we recently hit a network error, pause until the check window expires.
        if let Some(until) = offline_until {
            if tokio::time::Instant::now() < until {
                continue;
            }
            offline_until = None;
        }

        // Acquire the sync lock so we don't collide with sync_full.
        let _guard = sync_lock.lock().await;

        if is_pull_tick {
            if let Err(SyncError::Network(msg)) =
                run_pull_cycle(&pool, &state_pool, remote.as_ref(), &authored, &host).await
            {
                eprintln!("[sync] Offline — retrying in 30s ({msg})");
                offline_until = Some(tokio::time::Instant::now() + Duration::from_secs(30));
                continue;
            }
        }

        let uid = match crate::database::local::auth::admitted_principal(&pool).await {
            Ok(Some(uid)) => uid,
            Ok(None) | Err(_) => continue,
        };

        if let Err(error) = authored.bootstrap_live_projections(&pool, Some(&uid)).await {
            eprintln!("[sync] Authored projection bootstrap blocked push: {error}");
            continue;
        }

        if let Err(e) = super::orchestrator::enqueue_dirty(&pool, &uid).await {
            eprintln!("[sync] Enqueue error: {e}");
        }

        match flush_pending_with_integrator(&pool, &state_pool, remote.as_ref(), Some(&authored))
            .await
        {
            Ok(n) if n > 0 => println!("[sync] Pushed {n} ops"),
            Err(SyncError::AuthRequired) => {}
            Err(SyncError::Api { status: 401, .. }) => {
                auth_backoff = Some(tokio::time::Instant::now() + Duration::from_secs(30));
            }
            Err(SyncError::Network(msg)) => {
                eprintln!("[sync] Offline — retrying in 30s ({msg})");
                offline_until = Some(tokio::time::Instant::now() + Duration::from_secs(30));
            }
            Err(e) => eprintln!("[sync] Push error: {e}"),
            _ => {}
        }
    }
}

/// Full pull cycle: discovery → pull → files → emit library-changed.
/// Returns `Err(SyncError::Network(_))` when the remote is unreachable so the
/// caller can engage offline backoff.
async fn run_pull_cycle(
    pool: &SqlitePool,
    state_pool: &SqlitePool,
    remote: &dyn RemoteClient,
    authored: &AuthoredDocuments,
    host: &SyncHost,
) -> Result<(), SyncError> {
    let (token, uid) = match get_auth(state_pool).await {
        Ok(auth) => auth,
        Err(_) => return Ok(()),
    };

    // Discovery — find new/removed venues
    if let Err(e) = super::pull::discover_venues(pool, remote, &uid, &token).await {
        if matches!(&e, SyncError::Network(_)) {
            eprintln!("[sync] Discovery error (offline): {e}");
            return Err(e);
        }
        eprintln!("[sync] Discovery error: {e}");
    }

    // Delta pull
    let mut data_changed = false;
    match super::pull::pull_all(
        pool,
        authored,
        &host.workspaces,
        &host.graph_runs,
        &host.subagents,
        remote,
        &token,
        Some(&uid),
    )
    .await
    {
        Ok(stats) => {
            if stats.rows_pulled > 0 {
                println!(
                    "[sync] Pulled {} rows across {} tables",
                    stats.rows_pulled, stats.tables_pulled
                );
                data_changed = true;
            }
        }
        Err(e) if matches!(&e, SyncError::Network(_)) => {
            eprintln!("[sync] Pull error (offline): {e}");
            return Err(e);
        }
        Err(e) => eprintln!("[sync] Pull error: {e}"),
    }

    // Emit early so the UI sees pulled data before file downloads.
    if data_changed {
        host.events.emit("library-changed", ());
    }

    // File sync (upload pending, download stubs)
    let engine_auth = async {
        let auth = crate::database::local::auth::get_current_auth(state_pool)
            .await
            .map_err(SyncError::Local)?
            .ok_or(SyncError::AuthRequired)?;
        Ok::<_, SyncError>((auth.access_token, auth.principal.user_id))
    };

    if let Ok((token, uid)) = engine_auth.await {
        let mut stats = super::files::FileSyncStats::default();
        let _ =
            super::files::upload_pending_audio(pool, remote, &uid, &token, &mut stats, host).await;
        let _ =
            super::files::upload_pending_stems(pool, remote, &uid, &token, &mut stats, host).await;
        let _ =
            super::files::upload_pending_album_art(pool, remote, &uid, &token, &mut stats, host)
                .await;
        let _ = super::files::download_pending_audio(pool, remote, host, &token, &mut stats).await;
        let _ = super::files::download_pending_stems(pool, remote, host, &token, &mut stats).await;
        let _ =
            super::files::download_pending_album_art(pool, remote, host, &token, &mut stats).await;

        let files_changed = stats.audio_downloaded + stats.stems_downloaded + stats.art_downloaded;
        if files_changed > 0 {
            println!(
                "[sync] Files: {}↑ {}↓ audio, {}↑ {}↓ stems, {}↑ {}↓ art",
                stats.audio_uploaded,
                stats.audio_downloaded,
                stats.stems_uploaded,
                stats.stems_downloaded,
                stats.art_uploaded,
                stats.art_downloaded,
            );
            host.events.emit("library-changed", ());
        }
    }

    Ok(())
}

async fn get_auth(state_pool: &SqlitePool) -> Result<(String, String), SyncError> {
    let auth = crate::database::local::auth::get_current_auth(state_pool)
        .await
        .map_err(SyncError::Local)?
        .ok_or(SyncError::AuthRequired)?;
    Ok((auth.access_token, auth.principal.user_id))
}

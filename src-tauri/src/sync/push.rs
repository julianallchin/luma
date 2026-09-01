//! Push protocol: deliver what the local tables say the server is owed.
//!
//! There is no queue. Every cycle re-derives the work from ground truth: rows
//! whose delivery marker is behind their content, deletions recorded in
//! `sync_tombstones`, and the authored authority operations whose
//! server-assigned sequence is still NULL. The payload is built from the row at
//! the moment it is sent, so an edit that lands during the remote call leaves
//! the row dirty instead of being overwritten by a stale snapshot.
//!
//! See `docs/design/sync-push-v2.md`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::{watch, Mutex, Notify};

use crate::services::authored_documents::AuthoredDocuments;

use super::authored_remote::{
    self, ArchiveAuthoredDocumentInput, HeadProposalIntegrator, SubmitHeadProposalInput,
};
use super::error::SyncError;
use super::host::SyncHost;
use super::orchestrator::read_record_as_json;
use super::pull;
use super::push_state::{self, Subject, Verdict};
use super::registry::{self, PushPolicy, TableMeta};
use super::tombstone;
use super::traits::RemoteClient;

/// Rows and tombstones delivered in one flush before the loop gets another
/// turn. A backlog drains over several cycles rather than holding the sync lock
/// for minutes.
const FLUSH_BUDGET: usize = 200;

/// Most dirty rows of one table considered per cycle.
const SCAN_LIMIT: u32 = 1000;

/// One thing push owes the server, resolved from the tables at flush time.
struct Owed {
    table: &'static TableMeta,
    record_id: String,
    pk_values: Vec<String>,
    /// The row `version` the decision was made against, where the table has
    /// one. Retry state remembers it so a later edit restarts the budget, and
    /// the delivery receipt refuses to land on a different one.
    version: Option<i64>,
    /// `updated_at` as read by the scan, for the one mutable table with no
    /// version column. Same purpose as `version`: prove the row did not move
    /// while the request was in flight.
    stamp: Option<String>,
}

/// Flush everything the local tables owe the remote. Returns the count
/// delivered.
#[cfg(test)]
pub async fn flush_pending(
    pool: &SqlitePool,
    state_pool: &SqlitePool,
    remote: &dyn RemoteClient,
) -> Result<usize, SyncError> {
    flush_pending_with_integrator(pool, state_pool, remote, None).await
}

/// Flush with the domain-aware authored-head integration bridge installed.
/// Production callers always provide `AuthoredDocuments`; tests may omit it to
/// exercise ordinary row delivery in isolation.
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
    super::transition::drain_legacy_push_queue(pool).await?;
    let principal_key = crate::database::local::auth::principal_key(Some(&admitted_user_id));
    let mut delivered = 0usize;

    // Parents before children: a child whose parent has not landed is skipped
    // by the scan's reachability clause, so this order is what makes the skip
    // temporary rather than permanent.
    for table in registry::tables_in_topo_order() {
        if delivered >= FLUSH_BUDGET {
            return Ok(delivered);
        }
        for subject in scan_dirty(pool, table, &principal_key, &admitted_user_id).await? {
            if delivered >= FLUSH_BUDGET {
                return Ok(delivered);
            }
            let outcome = deliver_row(pool, remote, &subject, &token, &admitted_user_id).await;
            match settle(pool, &principal_key, &subject, Subject::Row, outcome).await? {
                Settlement::Delivered => delivered += 1,
                Settlement::Recorded => {}
                Settlement::AbortBatch(error) => return Err(error),
            }
        }
    }

    // Authored integration wake-ups are derived the same way: a proposal the
    // server has ordered, with no terminal integration yet, is work regardless
    // of which device created it.
    if let Some(integrator) = integrator {
        for proposal_id in pending_integrations(pool, &principal_key).await? {
            if delivered >= FLUSH_BUDGET {
                return Ok(delivered);
            }
            match integrate_one(
                pool,
                remote,
                integrator,
                &token,
                &admitted_user_id,
                &proposal_id,
            )
            .await
            {
                Ok(()) => {
                    push_state::clear(
                        pool,
                        &principal_key,
                        "authored_head_integrations",
                        &proposal_id,
                        Subject::Row,
                    )
                    .await?;
                    delivered += 1;
                }
                Err(error @ (SyncError::Network(_) | SyncError::Api { status: 401, .. })) => {
                    return Err(error)
                }
                Err(error) => {
                    // Never terminal, never dead-lettered: a stale head or an
                    // earlier pending proposal resolves itself once the device
                    // that owns it makes progress.
                    push_state::defer_retry(
                        pool,
                        &principal_key,
                        "authored_head_integrations",
                        &proposal_id,
                        &format!("{error}"),
                    )
                    .await?;
                }
            }
        }
    }

    // Children before parents, and after every upsert: the remote's soft delete
    // does not cascade, so the order it hears about a subtree is the order this
    // device deleted it.
    for tombstone in tombstone::pending(pool, &principal_key).await? {
        if delivered >= FLUSH_BUDGET {
            return Ok(delivered);
        }
        let Some(table) = registry::get_table(&tombstone.table_name) else {
            // A registry entry disappeared under a tombstone. Nothing can
            // deliver it; say so once and stop asking.
            push_state::record_failure(
                pool,
                &principal_key,
                &tombstone.table_name,
                &tombstone.record_id,
                Subject::Tombstone,
                None,
                Verdict::Permanent,
                "table is not registered for relational sync",
            )
            .await?;
            continue;
        };
        let Some(pk_values) = table.decode_record_id(&tombstone.record_id) else {
            push_state::record_failure(
                pool,
                &principal_key,
                &tombstone.table_name,
                &tombstone.record_id,
                Subject::Tombstone,
                None,
                Verdict::Permanent,
                "tombstone does not name every primary-key column",
            )
            .await?;
            continue;
        };
        let subject = Owed {
            table,
            record_id: tombstone.record_id.clone(),
            pk_values: pk_values.iter().map(|value| (*value).to_owned()).collect(),
            version: None,
            stamp: None,
        };
        // A row that exists again outranks the tombstone: the local table is
        // the truth, and "present" is a later statement than "deleted". This is
        // asked before the retry gate on purpose — a tombstone push has given
        // up on must still be retracted when its identity comes back, or the
        // recreated row would carry a deletion nobody can cancel.
        if row_exists(pool, table, &subject.pk_values).await? {
            tombstone::clear(pool, &principal_key, table.name, &subject.record_id).await?;
            push_state::clear(
                pool,
                &principal_key,
                table.name,
                &subject.record_id,
                Subject::Tombstone,
            )
            .await?;
            continue;
        }
        if !tombstone.ready {
            continue;
        }
        let outcome = deliver_tombstone(remote, &subject, &token).await;
        match settle(pool, &principal_key, &subject, Subject::Tombstone, outcome).await? {
            Settlement::Delivered => {
                tombstone::clear(pool, &principal_key, table.name, &subject.record_id).await?;
                delivered += 1;
            }
            Settlement::Recorded => {}
            Settlement::AbortBatch(error) => return Err(error),
        }
    }

    Ok(delivered)
}

/// What to do with the local state after one delivery attempt.
enum Settlement {
    Delivered,
    /// The failure is recorded; the batch continues.
    Recorded,
    AbortBatch(SyncError),
}

/// Turn one delivery outcome into local state: clear the retry row on success,
/// classify and record it otherwise.
async fn settle(
    pool: &SqlitePool,
    principal_key: &str,
    subject: &Owed,
    kind: Subject,
    outcome: Result<(), SyncError>,
) -> Result<Settlement, SyncError> {
    match outcome {
        Ok(()) => {
            push_state::clear(
                pool,
                principal_key,
                subject.table.name,
                &subject.record_id,
                kind,
            )
            .await?;
            Ok(Settlement::Delivered)
        }
        // The session is the batch's problem, not this row's.
        Err(SyncError::Api { status: 401, .. }) => {
            eprintln!("[sync] 401 — stopping batch for token refresh");
            Ok(Settlement::AbortBatch(SyncError::Api {
                status: 401,
                message: "token expired".into(),
            }))
        }
        // Offline. Attempts are untouched: being unreachable is not the row's
        // fault and must not consume its budget.
        Err(error @ SyncError::Network(_)) => Ok(Settlement::AbortBatch(error)),
        Err(error) => {
            let verdict = classify(subject.table, &error);
            eprintln!(
                "[sync] push failed {}.{}: {error}",
                subject.table.name, subject.record_id
            );
            push_state::record_failure(
                pool,
                principal_key,
                subject.table.name,
                &subject.record_id,
                kind,
                subject.version,
                verdict,
                &format!("{error}"),
            )
            .await?;
            Ok(Settlement::Recorded)
        }
    }
}

/// Whether retrying this failure can ever produce a different answer.
///
/// Three cases can be answered, and only three (audit T2.8, for the push side):
///
/// - an identity the remote column type cannot hold, decided before the request
///   went out;
/// - a unique-key violation on an immutable table — the server saying "this
///   identity already exists with different bytes", a permanent divergence
///   rather than a conflict that resolves itself;
/// - a 403, which is row-level security refusing *this principal* for *this
///   row*. Reachability already removes the case where such a refusal was
///   really a missing parent, so what is left is a disagreement about ownership
///   that the same token loses again every ten seconds.
///
/// Everything else is transient, including a 400 from a client ahead of the
/// server's schema: a deploy, or a parent landing, does change that answer.
fn classify(table: &TableMeta, error: &SyncError) -> Verdict {
    match error {
        SyncError::Unpushable(_) | SyncError::Api { status: 403, .. } => Verdict::Permanent,
        SyncError::Api {
            status: 409,
            message,
        } if message.contains("23505") && registry::is_immutable_table(table.name) => {
            Verdict::Permanent
        }
        _ => Verdict::Transient,
    }
}

/// Identities the remote cannot hold, whatever the payload says.
///
/// `venues.id` is `TEXT` locally and `uuid` remotely, so a scratch venue named
/// `djtable-scratch-1` fails every push forever — and, before reachability, so
/// did everything under it. Naming the mismatch here turns an infinite retry
/// into one recorded, quiet fact; the subtree goes quiet on its own because its
/// parent never becomes reachable.
///
/// This is deliberately not a per-table list of remote column types. Such a
/// list is a fourth copy of the schema (audit T3.1) that nothing checks; one
/// named poison with a reason is honest.
fn unpushable_reason(table: &TableMeta, pk_values: &[String]) -> Option<String> {
    let first = pk_values.first()?;
    if table.name == "venues" && uuid::Uuid::parse_str(first).is_err() {
        return Some(format!(
            "venue id {first:?} is not a uuid and the remote column cannot hold it"
        ));
    }
    None
}

/// Everything of `table` this principal owes the server, decided by the row.
async fn scan_dirty(
    pool: &SqlitePool,
    table: &'static TableMeta,
    principal_key: &str,
    uid: &str,
) -> Result<Vec<Owed>, SyncError> {
    if !table.has_principal() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(table.dirty_scan_sql(SCAN_LIMIT)))
        .bind(principal_key)
        .bind(uid)
        .fetch_all(pool)
        .await?;
    use sqlx::Row;
    let key_count = table.pk_columns().len();
    rows.iter()
        .map(|row| {
            let pk_values: Vec<String> = (0..key_count)
                .map(|index| row.try_get::<String, _>(index))
                .collect::<Result<_, _>>()?;
            Ok(Owed {
                table,
                record_id: registry::record_id(pk_values.iter().map(String::as_str)),
                version: row.try_get::<Option<i64>, _>(key_count)?,
                stamp: row.try_get::<Option<String>, _>(key_count + 1)?,
                pk_values,
            })
        })
        .collect()
}

/// Deliver one dirty row and write its receipt.
async fn deliver_row(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    subject: &Owed,
    token: &str,
    admitted_user_id: &str,
) -> Result<(), SyncError> {
    if let Some(reason) = unpushable_reason(subject.table, &subject.pk_values) {
        return Err(SyncError::Unpushable(reason));
    }
    let table = subject.table;
    let payload = read_record_as_json(pool, table, &subject.record_id).await?;
    if !table.payload_principal_matches(&payload, admitted_user_id) {
        return Err(SyncError::Local(format!(
            "{}.{} is not owned by the active app principal {admitted_user_id:?}",
            table.name, subject.record_id
        )));
    }
    match registry::push_policy(table.name) {
        PushPolicy::DirtyUpsert | PushPolicy::ExplicitUpsert => {
            remote
                .upsert_json(table.name, &payload, table.conflict_key, token)
                .await?;
        }
        PushPolicy::ExplicitImmutable => {
            remote
                .insert_immutable_json(table.name, &payload, table.conflict_key, token)
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
        }
        PushPolicy::ServerAuthority => {
            return deliver_authority(pool, remote, subject, &payload, token, admitted_user_id)
                .await;
        }
    }
    mark_delivered(pool, subject, admitted_user_id).await
}

/// The two authority operations whose local row is the RPC's input. Their
/// receipt is the server sequence the RPC assigns, written by the existing
/// receipt appliers, so there is no delivery marker to set here.
async fn deliver_authority(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    subject: &Owed,
    payload: &Value,
    token: &str,
    admitted_user_id: &str,
) -> Result<(), SyncError> {
    match subject.table.name {
        "authored_head_proposals" => {
            let input: SubmitHeadProposalInput = serde_json::from_value(payload.clone())
                .map_err(|error| SyncError::Parse(error.to_string()))?;
            let receipt = authored_remote::submit_head_proposal(remote, &input, token).await?;
            apply_proposal_receipt(pool, admitted_user_id, &input, &receipt).await
        }
        "authored_document_archives" => {
            let input: ArchiveAuthoredDocumentInput = serde_json::from_value(payload.clone())
                .map_err(|error| SyncError::Parse(error.to_string()))?;
            let receipt = authored_remote::archive_authored_document(remote, &input, token).await?;
            apply_archive_receipt(pool, admitted_user_id, &input, &receipt).await
        }
        other => Err(SyncError::Parse(format!(
            "{other} is server-authoritative and has no client delivery"
        ))),
    }
}

/// Tell the remote a row is gone: PATCH `deleted_at` on it.
///
/// PATCH rather than upsert because an upsert's INSERT half fails the remote's
/// NOT NULL constraints when the payload is only a key and a timestamp — and
/// the row's columns are no longer available to send, which is the point.
async fn deliver_tombstone(
    remote: &dyn RemoteClient,
    subject: &Owed,
    token: &str,
) -> Result<(), SyncError> {
    let filter = subject
        .table
        .pk_columns()
        .iter()
        .zip(subject.pk_values.iter())
        .map(|(column, value)| format!("{column}=eq.{}", percent_encode_filter_value(value)))
        .collect::<Vec<_>>()
        .join("&");
    let payload = serde_json::json!({ "deleted_at": chrono::Utc::now().to_rfc3339() });
    remote
        .patch_json(subject.table.name, &filter, &payload, token)
        .await
}

/// Write the delivery receipt onto the row, under the same admission the write
/// itself required.
///
/// The receipt is a sync-owned write: the immutable tables' `RAISE(ABORT)`
/// triggers and the thread projection's `updated_at` trigger both stand down
/// inside `enter_remote_writes`, which is what lets a row that may not be
/// edited still record that it was delivered.
async fn mark_delivered(
    pool: &SqlitePool,
    subject: &Owed,
    admitted_user_id: &str,
) -> Result<(), SyncError> {
    let table = subject.table;
    let principal_guard = if table.columns.contains(&"uid") {
        "uid = ?"
    } else if table.columns.contains(&"principal_key") {
        "principal_key = 'signed-in:' || ?"
    } else {
        "owner_user_id = ?"
    };
    // The receipt names the row the request was built from. A local edit during
    // the remote call moves `version` (or `updated_at` where there is no
    // version), the receipt matches nothing, and the row stays dirty instead of
    // being marked clean over content the server never saw — audit T1.2.
    let stamp_guard = if subject.version.is_some() {
        " AND version = ?"
    } else if subject.stamp.is_some() {
        " AND updated_at IS ?"
    } else {
        ""
    };
    let sql = format!(
        "{}{stamp_guard} AND {principal_guard} AND EXISTS (
             SELECT 1 FROM auth_write_admission AS admission
             WHERE admission.singleton = 1
               AND admission.armed = 1
               AND admission.accepting = 1
               AND admission.maintenance = 0
               AND admission.active_uid = ?
         )",
        table.mark_delivered_sql()
    );
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for value in &subject.pk_values {
        query = query.bind(value);
    }
    if let Some(version) = subject.version {
        query = query.bind(version);
    } else if let Some(stamp) = &subject.stamp {
        query = query.bind(stamp.as_str());
    }
    query = query.bind(admitted_user_id).bind(admitted_user_id);
    let result = query.execute(&mut *transaction).await?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    transaction.commit().await?;
    if result.rows_affected() == 1 {
        return Ok(());
    }
    // The delivery succeeded and the row will not take the receipt. That is
    // this row's problem and not the batch's: it was deleted underneath the
    // call (its tombstone is the successor state and needs no help), or the
    // principal changed. Only the second is worth reporting.
    if !row_exists(pool, table, &subject.pk_values).await? {
        return Ok(());
    }
    if row_moved(pool, subject).await? {
        // The delivery stands; the row simply owes the server more. The next
        // scan sends the newer content.
        return Ok(());
    }
    Err(SyncError::Local(format!(
        "{}.{} would not accept its delivery receipt",
        table.name, subject.record_id
    )))
}

/// Server-ordered proposals with no terminal integration, whatever device made
/// them. Any online owner device can advance any of them, so an offline author
/// never blocks a document.
async fn pending_integrations(
    pool: &SqlitePool,
    principal_key: &str,
) -> Result<Vec<String>, SyncError> {
    Ok(sqlx::query_scalar(
        "SELECT proposal.proposal_id
         FROM authored_head_proposals proposal
         LEFT JOIN authored_head_integrations integration
           ON integration.proposal_id = proposal.proposal_id
         JOIN authored_documents document
           ON document.document_id = proposal.document_id
         LEFT JOIN sync_push_failures failure
           ON failure.principal_key = proposal.principal_key
          AND failure.table_name = 'authored_head_integrations'
          AND failure.record_id = proposal.proposal_id
         WHERE proposal.principal_key = ?
           AND proposal.server_proposal_seq IS NOT NULL
           AND integration.proposal_id IS NULL
           AND document.archived_at IS NULL
           AND (failure.record_id IS NULL OR failure.next_retry_at <= CURRENT_TIMESTAMP)
         ORDER BY proposal.server_proposal_seq, proposal.proposal_id",
    )
    .bind(principal_key)
    .fetch_all(pool)
    .await?)
}

async fn integrate_one(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    integrator: &dyn HeadProposalIntegrator,
    token: &str,
    admitted_user_id: &str,
    proposal_id: &str,
) -> Result<(), SyncError> {
    let receipt = integrator
        .integrate_pending_proposal(pool, remote, token, admitted_user_id, proposal_id)
        .await?;
    if receipt.is_terminal() {
        return Ok(());
    }
    Err(SyncError::Local(format!(
        "authored proposal {proposal_id} was not terminal after integration ({:?}); recompute against the latest server head",
        receipt.outcome
    )))
}

/// Whether the row moved between the scan and the receipt.
///
/// `false` for a row with no stamp — an immutable row cannot move.
async fn row_moved(pool: &SqlitePool, subject: &Owed) -> Result<bool, SyncError> {
    let sql = format!(
        "SELECT 1 FROM {} WHERE {} AND {}",
        subject.table.name,
        subject.table.pk_where(),
        if subject.version.is_some() {
            "version = ?"
        } else {
            "updated_at IS ?"
        }
    );
    let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
    for value in &subject.pk_values {
        query = query.bind(value);
    }
    query = match (subject.version, &subject.stamp) {
        (Some(version), _) => query.bind(version),
        (None, Some(stamp)) => query.bind(stamp.clone()),
        (None, None) => return Ok(false),
    };
    Ok(query.fetch_optional(pool).await?.is_none())
}

/// Whether the row those primary-key values name exists at all.
async fn row_exists(
    pool: &SqlitePool,
    table: &TableMeta,
    pk_values: &[String],
) -> Result<bool, SyncError> {
    let sql = format!("SELECT 1 FROM {} WHERE {}", table.name, table.pk_where());
    let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
    for value in pk_values {
        query = query.bind(value);
    }
    Ok(query.fetch_optional(pool).await?.is_some())
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
    // Submission is not integration, and integration is not queued: the same
    // scan that any other owner device runs will see this proposal has a server
    // sequence and no terminal integration. A permanently offline author can
    // never be required for progress.
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

        match flush_pending_with_integrator(&pool, &state_pool, remote.as_ref(), Some(&authored))
            .await
        {
            Ok(n) if n > 0 => println!("[sync] Pushed {n} ops"),
            Err(SyncError::AuthRequired) => {}
            // The session is gone for good; the app needs a person to sign in.
            // Say so once per backoff window rather than once per tick.
            Err(SyncError::SessionRevoked) => {
                eprintln!("[sync] Session revoked — sign in again to resume");
                host.events.emit("session-revoked", ());
                auth_backoff = Some(tokio::time::Instant::now() + Duration::from_secs(30));
            }
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
            .map_err(SyncError::from)?
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
        .map_err(SyncError::from)?
        .ok_or(SyncError::AuthRequired)?;
    Ok((auth.access_token, auth.principal.user_id))
}

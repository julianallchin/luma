//! One-shot translation of the retired `pending_ops` queue.
//!
//! A database upgraded into state-based push can be holding edits that were
//! enqueued but never delivered. The migration renames the queue rather than
//! dropping it, because translating it needs write admission a migration does
//! not have — a venue row cannot be marked dirty from a migration, its
//! admission triggers refuse the write. So the translation happens here, at the
//! first push under the new engine, and drops the table afterwards: its absence
//! is the completion flag, which makes this idempotent without a second one.
//!
//! Nothing is lost. Every legacy operation becomes either a dirty row or a
//! tombstone, and an operation whose row is already gone was either a tombstone
//! (translated) or the orphaned-upsert wedge this design exists to remove.
//!
//! The mirror of this runs in the migration, not here: every row that existed
//! at migration time is stamped delivered, because under the old engine the
//! receipt *was* the removal of the queue entry. This module clears the marker
//! again for exactly the rows an operation still names. The split is what makes
//! the boundary the migration itself — a row created after it, but before the
//! first flush, is genuinely new and is never presumed delivered.

use sqlx::SqlitePool;

use super::error::SyncError;
use super::push_state;
use super::registry;

const LEGACY_TABLE: &str = "pending_ops_drain";

/// Translate and remove the legacy queue, if it is still there.
pub async fn drain_legacy_push_queue(pool: &SqlitePool) -> Result<(), SyncError> {
    if !legacy_queue_exists(pool).await? {
        return Ok(());
    }

    let ops: Vec<LegacyOp> = sqlx::query_as(
        "SELECT principal_key, op_type, table_name, record_id, attempts, last_error
         FROM pending_ops_drain
         ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    // Marking a row dirty or delivered is a sync-owned write, and it is the
    // only way to reach a venue table's `synced_at` — or an immutable row's at
    // all, since their triggers refuse every other update.
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;

    let mut redirtied = 0usize;
    let mut tombstoned = 0usize;
    let mut carried = 0usize;
    for op in &ops {
        let Some(table) = registry::get_table(&op.table_name) else {
            // `pattern_categories`, `authored_head_authority`, and anything
            // else unregistered: these could never be delivered under the old
            // engine either.
            continue;
        };
        let subject = match op.op_type.as_str() {
            "delete" => {
                if !registry::has_remote_tombstone(&op.table_name) {
                    continue;
                }
                sqlx::query(
                    "INSERT INTO sync_tombstones (principal_key, table_name, record_id)
                     VALUES (?, ?, ?)
                     ON CONFLICT(principal_key, table_name, record_id) DO NOTHING",
                )
                .bind(&op.principal_key)
                .bind(&op.table_name)
                .bind(&op.record_id)
                .execute(&mut *transaction)
                .await?;
                tombstoned += 1;
                push_state::Subject::Tombstone
            }
            // The row is the payload now, so re-offering it is just clearing
            // its delivery marker. The three authority operations need nothing:
            // push re-derives them from a NULL server sequence.
            "upsert" | "upsert_explicit" | "insert_immutable" => {
                let Some(pk_values) = table.decode_record_id(&op.record_id) else {
                    continue;
                };
                let sql = format!(
                    "UPDATE {} SET synced_at = NULL WHERE {}",
                    table.name,
                    table.pk_where()
                );
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
                for value in &pk_values {
                    query = query.bind(*value);
                }
                redirtied +=
                    usize::try_from(query.execute(&mut *transaction).await?.rows_affected())
                        .unwrap_or(0);
                push_state::Subject::Row
            }
            _ => continue,
        };
        if op.attempts > 0 {
            carry_failure(&mut transaction, op, table, subject).await?;
            carried += 1;
        }
    }

    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {LEGACY_TABLE}")))
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    println!(
        "[sync] Retired the push queue: {} operation(s) → {redirtied} dirty row(s), \
         {tombstoned} tombstone(s), {carried} carried failure(s)",
        ops.len()
    );
    Ok(())
}

/// Carry an operation's retry history across, so a dead-lettered operation
/// stays dead-lettered instead of arriving with a fresh budget of twenty.
///
/// `seen_version` is read from the row rather than left NULL: it is what lets a
/// later edit clear an inherited verdict, and a verdict nothing can clear is
/// exactly the trap this design removed elsewhere.
async fn carry_failure(
    transaction: &mut sqlx::SqliteConnection,
    op: &LegacyOp,
    table: &'static registry::TableMeta,
    subject: push_state::Subject,
) -> Result<(), SyncError> {
    let seen_version = match (
        subject,
        registry::push_policy(table.name),
        table.decode_record_id(&op.record_id),
    ) {
        (push_state::Subject::Row, registry::PushPolicy::DirtyUpsert, Some(pk_values)) => {
            let sql = format!(
                "SELECT version FROM {} WHERE {}",
                table.name,
                table.pk_where()
            );
            let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
            for value in &pk_values {
                query = query.bind(*value);
            }
            query.fetch_optional(&mut *transaction).await?
        }
        _ => None,
    };
    sqlx::query(
        "INSERT INTO sync_push_failures
            (principal_key, table_name, record_id, subject, attempts, last_error,
             next_retry_at, seen_version, permanent)
         VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, ?, ?)
         ON CONFLICT(principal_key, table_name, record_id, subject) DO NOTHING",
    )
    .bind(&op.principal_key)
    .bind(&op.table_name)
    .bind(&op.record_id)
    .bind(subject.as_str())
    .bind(op.attempts)
    .bind(&op.last_error)
    .bind(seen_version)
    .bind(i64::from(op.attempts >= LEGACY_MAX_ATTEMPTS))
    .execute(transaction)
    .await?;
    Ok(())
}

/// One row of the retired queue.
#[derive(sqlx::FromRow)]
struct LegacyOp {
    principal_key: String,
    op_type: String,
    table_name: String,
    record_id: String,
    attempts: i64,
    last_error: Option<String>,
}

/// The dead-letter threshold the queue used. An operation that reached it had
/// stopped being retried, and translating it into a fresh row would restart a
/// budget the old engine had already spent.
const LEGACY_MAX_ATTEMPTS: i64 = 20;

async fn legacy_queue_exists(pool: &SqlitePool) -> Result<bool, SyncError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
    )
    .bind(LEGACY_TABLE)
    .fetch_optional(pool)
    .await?
    .is_some())
}

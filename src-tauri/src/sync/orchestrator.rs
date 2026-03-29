//! Sync orchestrator: the single entry point for all sync operations.

use std::sync::Arc;

use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Notify};

use super::error::SyncError;
use super::files::{self, FileSyncStats};
use super::pending;
use super::pull::{self, PullStats};
use super::push;
use super::registry;
use super::traits::RemoteClient;

/// Maximum dirty records to enqueue per table in a single pass.
const DIRTY_BATCH_LIMIT: u32 = 1000;

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub pull: PullStats,
    pub pushed: usize,
    pub files: FileSyncStats,
    pub errors: Vec<String>,
}

pub struct SyncEngine {
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<dyn RemoteClient>,
    pub(crate) push_notify: Arc<Notify>,
    /// Prevents concurrent sync operations (full_sync vs background loop).
    pub(crate) sync_lock: Arc<Mutex<()>>,
}

impl SyncEngine {
    pub fn new(pool: SqlitePool, state_pool: SqlitePool, remote: Arc<dyn RemoteClient>) -> Self {
        Self {
            pool,
            state_pool,
            remote,
            push_notify: Arc::new(Notify::new()),
            sync_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn state_pool(&self) -> &SqlitePool {
        &self.state_pool
    }

    pub fn remote(&self) -> &Arc<dyn RemoteClient> {
        &self.remote
    }

    pub(crate) async fn require_auth(&self) -> Result<(String, String), SyncError> {
        let token = crate::database::local::auth::get_current_access_token(&self.state_pool)
            .await
            .map_err(SyncError::Local)?
            .ok_or(SyncError::AuthRequired)?;
        let uid = crate::database::local::auth::get_current_user_id(&self.state_pool)
            .await
            .map_err(SyncError::Local)?
            .ok_or(SyncError::AuthRequired)?;
        Ok((token, uid))
    }

    /// Full sync: discovery → pull → stamp → files → push.
    pub async fn full_sync(&self, app_handle: &AppHandle) -> Result<SyncReport, SyncError> {
        let _guard = self.sync_lock.lock().await;
        println!("[sync] Starting full sync...");
        let (token, uid) = self.require_auth().await?;
        let mut report = SyncReport::default();

        // 1. Discovery
        match pull::discover_venues(&self.pool, self.remote.as_ref(), &uid, &token).await {
            Ok(ids) => {
                println!("[sync] Discovered {} venues (owned + joined)", ids.len());
                report.pull.venues_discovered = ids.len();
            }
            Err(e) => {
                eprintln!("[sync] Discovery failed: {e}");
                report.errors.push(format!("discovery: {e}"));
            }
        };

        // 2. Delta pull
        let discovered_count = report.pull.venues_discovered;
        match pull::pull_all(&self.pool, self.remote.as_ref(), &token, Some(&uid)).await {
            Ok(mut stats) => {
                if stats.rows_pulled > 0 {
                    println!(
                        "[sync] Pulled {} rows across {} tables",
                        stats.rows_pulled, stats.tables_pulled
                    );
                } else {
                    println!("[sync] Pull: everything up to date");
                }
                for e in &stats.errors {
                    eprintln!("[sync] Pull error: {e}");
                }
                stats.venues_discovered = discovered_count;
                report.pull = stats;
            }
            Err(e) => {
                eprintln!("[sync] Pull failed: {e}");
                report.errors.push(format!("pull: {e}"));
            }
        }

        // Emit early so the UI refreshes with pulled data while files download.
        if report.pull.rows_pulled > 0 {
            let _ = app_handle.emit("library-changed", ());
        }

        // 3. Stamp records that already exist remotely as clean
        let stamped = stamp_already_synced(&self.pool).await;
        if stamped > 0 {
            println!("[sync] Marked {stamped} pre-existing records as synced (skipping re-push)");
        }

        // 4. File sync — runs before push so storage_path updates are
        //    included when dirty records are flushed to remote.
        match self.sync_files(app_handle).await {
            Ok(ref stats)
                if stats.audio_uploaded
                    + stats.stems_uploaded
                    + stats.art_uploaded
                    + stats.audio_downloaded
                    + stats.stems_downloaded
                    + stats.art_downloaded
                    > 0 =>
            {
                println!(
                    "[sync] Files: {}↑ {}↓ audio, {}↑ {}↓ stems, {}↑ {}↓ art",
                    stats.audio_uploaded,
                    stats.audio_downloaded,
                    stats.stems_uploaded,
                    stats.stems_downloaded,
                    stats.art_uploaded,
                    stats.art_downloaded,
                );
                report.files = stats.clone();
            }
            Ok(stats) => report.files = stats,
            Err(e) => {
                eprintln!("[sync] File sync failed: {e}");
                report.errors.push(format!("files: {e}"));
            }
        }

        // 5. Push — single pass catches local edits + storage_path updates
        report.pushed += self.run_push(&uid).await.unwrap_or_else(|e| {
            report.errors.push(format!("push: {e}"));
            0
        });

        // Notify the UI if any data changed so lists/status refresh.
        let data_changed = report.pull.rows_pulled > 0
            || report.pushed > 0
            || report.files.audio_downloaded
                + report.files.stems_downloaded
                + report.files.art_downloaded
                > 0;
        if data_changed {
            let _ = app_handle.emit("library-changed", ());
        }

        println!("[sync] Full sync complete");
        Ok(report)
    }

    /// Enqueue dirty records and flush pending ops. Returns count pushed.
    pub(crate) async fn run_push(&self, uid: &str) -> Result<usize, SyncError> {
        if let Err(e) = enqueue_dirty(&self.pool, uid).await {
            eprintln!("[sync] Enqueue failed: {e}");
        }
        let n = push::flush_pending(&self.pool, &self.state_pool, self.remote.as_ref()).await?;
        if n > 0 {
            println!("[sync] Pushed {n} records to remote");
        }
        Ok(n)
    }

    /// Pull only (manual refresh).
    pub async fn pull(&self) -> Result<PullStats, SyncError> {
        let (token, uid) = self.require_auth().await?;
        pull::discover_venues(&self.pool, self.remote.as_ref(), &uid, &token).await?;
        pull::pull_all(&self.pool, self.remote.as_ref(), &token, Some(&uid)).await
    }

    /// File sync: upload pending, download stubs.
    pub async fn sync_files(&self, app_handle: &AppHandle) -> Result<FileSyncStats, SyncError> {
        let (token, uid) = self.require_auth().await?;
        let mut stats = FileSyncStats::default();
        files::upload_pending_audio(&self.pool, self.remote.as_ref(), &uid, &token, &mut stats)
            .await?;
        files::upload_pending_stems(&self.pool, self.remote.as_ref(), &uid, &token, &mut stats)
            .await?;
        files::upload_pending_album_art(&self.pool, self.remote.as_ref(), &uid, &token, &mut stats)
            .await?;
        files::download_pending_audio(
            &self.pool,
            self.remote.as_ref(),
            app_handle,
            &token,
            &mut stats,
        )
        .await?;
        files::download_pending_stems(
            &self.pool,
            self.remote.as_ref(),
            app_handle,
            &token,
            &mut stats,
        )
        .await?;
        files::download_pending_album_art(
            &self.pool,
            self.remote.as_ref(),
            app_handle,
            &token,
            &mut stats,
        )
        .await?;
        Ok(stats)
    }
}

/// After a pull, mark records that have never been synced as clean.
/// These already exist remotely — pushing them would be redundant.
async fn stamp_already_synced(pool: &SqlitePool) -> u64 {
    let mut total = 0u64;
    for table in registry::TABLES {
        let sql = format!(
            "UPDATE {} SET synced_at = updated_at, version = version + 1 WHERE synced_at IS NULL",
            table.name
        );
        if let Ok(result) = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await {
            let n = result.rows_affected();
            if n > 0 {
                eprintln!("[sync] stamped {n} unsynced in {}", table.name);
            }
            total += n;
        }
    }
    total
}

/// Scan all tables for dirty records and enqueue them into pending_ops.
/// Single implementation used by both full_sync and the background loop.
/// Batches in groups of DIRTY_BATCH_LIMIT to bound memory usage.
pub async fn enqueue_dirty(pool: &SqlitePool, uid: &str) -> Result<usize, SyncError> {
    let mut count = 0;
    for table in registry::TABLES {
        let base_sql = table.dirty_query();
        let has_uid = table.columns.contains(&"uid");
        let sql = format!("{base_sql} LIMIT {DIRTY_BATCH_LIMIT}");

        if table.is_composite_pk() {
            let rows: Vec<(String, String)> = if has_uid {
                sqlx::query_as(sqlx::AssertSqlSafe(&*sql))
                    .bind(uid)
                    .fetch_all(pool)
                    .await?
            } else {
                sqlx::query_as(sqlx::AssertSqlSafe(&*sql))
                    .fetch_all(pool)
                    .await?
            };
            if !rows.is_empty() {
                eprintln!("[sync] {} dirty in {}", rows.len(), table.name);
            }
            for (a, b) in &rows {
                let record_id = format!("{a}:{b}");
                if let Ok(payload) = read_record_as_json(pool, table, &record_id).await {
                    let json = serde_json::to_string(&payload)
                        .map_err(|e| SyncError::Parse(e.to_string()))?;
                    pending::enqueue_upsert(
                        pool,
                        table.name,
                        &record_id,
                        &json,
                        table.conflict_key,
                        table.tier,
                    )
                    .await?;
                    count += 1;
                }
            }
        } else {
            let ids: Vec<String> = if has_uid {
                sqlx::query_scalar(sqlx::AssertSqlSafe(&*sql))
                    .bind(uid)
                    .fetch_all(pool)
                    .await?
            } else {
                sqlx::query_scalar(sqlx::AssertSqlSafe(&*sql))
                    .fetch_all(pool)
                    .await?
            };
            if !ids.is_empty() {
                eprintln!("[sync] {} dirty in {}", ids.len(), table.name);
            }
            for record_id in &ids {
                if let Ok(payload) = read_record_as_json(pool, table, record_id).await {
                    let json = serde_json::to_string(&payload)
                        .map_err(|e| SyncError::Parse(e.to_string()))?;
                    pending::enqueue_upsert(
                        pool,
                        table.name,
                        record_id,
                        &json,
                        table.conflict_key,
                        table.tier,
                    )
                    .await?;
                    count += 1;
                }
            }
        }
    }
    Ok(count)
}

/// Read a record from local SQLite as JSON, excluding local_only columns.
pub async fn read_record_as_json(
    pool: &SqlitePool,
    table: &registry::TableMeta,
    record_id: &str,
) -> Result<serde_json::Value, SyncError> {
    let cols = table.columns.join(", ");
    let pk_values = table.decode_record_id(record_id);
    let where_clause = table.pk_where();

    let sql = format!("SELECT {cols} FROM {} WHERE {where_clause}", table.name);
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for val in &pk_values {
        query = query.bind(*val);
    }

    let row = query
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SyncError::NotFound {
            table: table.name.to_string(),
            id: record_id.to_string(),
        })?;

    use sqlx::Row;
    let mut map = serde_json::Map::new();
    for col in table.remote_columns() {
        let val: serde_json::Value = if let Ok(s) = row.try_get::<Option<String>, _>(col) {
            match s {
                Some(s) => serde_json::Value::String(s),
                None => serde_json::Value::Null,
            }
        } else if let Ok(i) = row.try_get::<i64, _>(col) {
            serde_json::Value::Number(i.into())
        } else if let Ok(f) = row.try_get::<f64, _>(col) {
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };
        map.insert(col.to_string(), val);
    }

    Ok(serde_json::Value::Object(map))
}

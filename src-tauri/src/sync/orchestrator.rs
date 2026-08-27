//! Sync orchestrator: the single entry point for all sync operations.

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify};

use crate::services::authored_documents::AuthoredDocuments;

use super::error::SyncError;
use super::files::{self, FileSyncStats};
use super::host::SyncHost;
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

/// Every field is a shared handle, so a clone is another view of the same
/// engine — same notify, same lock — not an independent one.
#[derive(Clone)]
pub struct SyncEngine {
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<dyn RemoteClient>,
    authored: AuthoredDocuments,
    pub(crate) push_notify: Arc<Notify>,
    /// Prevents concurrent sync operations (sync_full vs background loop).
    pub(crate) sync_lock: Arc<Mutex<()>>,
}

impl SyncEngine {
    pub fn new(
        pool: SqlitePool,
        state_pool: SqlitePool,
        remote: Arc<dyn RemoteClient>,
        authored: AuthoredDocuments,
    ) -> Self {
        Self {
            pool,
            state_pool,
            remote,
            authored,
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

    pub fn authored(&self) -> &AuthoredDocuments {
        &self.authored
    }

    pub(crate) async fn require_auth(&self) -> Result<(String, String), SyncError> {
        let auth = crate::database::local::auth::get_current_auth(&self.state_pool)
            .await
            .map_err(SyncError::Local)?
            .ok_or(SyncError::AuthRequired)?;
        Ok((auth.access_token, auth.principal.user_id))
    }

    /// Full sync: discovery → pull → files → push.
    pub async fn sync_full(&self, host: &SyncHost) -> Result<SyncReport, SyncError> {
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
        match pull::pull_all(
            &self.pool,
            &self.authored,
            &host.workspaces,
            &host.graph_runs,
            &host.subagents,
            self.remote.as_ref(),
            &token,
            Some(&uid),
        )
        .await
        {
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
            host.events.emit("library-changed", ());
        }

        // 3. File sync — runs before push so storage_path updates are
        //    included when dirty records are flushed to remote.
        match self.sync_files_unlocked(host).await {
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

        // 4. Push — single pass catches local edits + storage_path updates
        report.pushed += self.run_push_unlocked(&uid).await.unwrap_or_else(|e| {
            report.errors.push(format!("push: {e}"));
            0
        });

        // Notify the UI if incoming data changed (pull or downloads).
        // Push-only cycles are not emitted — the UI already has that state.
        let incoming_changed = report.pull.rows_pulled > 0
            || report.files.audio_downloaded
                + report.files.stems_downloaded
                + report.files.art_downloaded
                > 0;
        if incoming_changed {
            host.events.emit("library-changed", ());
        }

        println!("[sync] Full sync complete");
        Ok(report)
    }

    /// Enqueue dirty records and flush pending ops. Returns count pushed.
    pub async fn run_push(&self, uid: &str) -> Result<usize, SyncError> {
        let _guard = self.sync_lock.lock().await;
        self.run_push_unlocked(uid).await
    }

    async fn run_push_unlocked(&self, uid: &str) -> Result<usize, SyncError> {
        self.authored
            .bootstrap_live_projections(&self.pool, Some(uid))
            .await
            .map_err(|error| {
                SyncError::Local(format!(
                    "authored projection bootstrap blocked push: {error}"
                ))
            })?;
        if let Err(e) = enqueue_dirty(&self.pool, uid).await {
            eprintln!("[sync] Enqueue failed: {e}");
        }
        let n = push::flush_pending_with_integrator(
            &self.pool,
            &self.state_pool,
            self.remote.as_ref(),
            Some(&self.authored),
        )
        .await?;
        if n > 0 {
            println!("[sync] Pushed {n} records to remote");
        }
        Ok(n)
    }

    pub(crate) async fn sync_files_unlocked(
        &self,
        host: &SyncHost,
    ) -> Result<FileSyncStats, SyncError> {
        let (token, uid) = self.require_auth().await?;
        let mut stats = FileSyncStats::default();
        files::upload_pending_audio(
            &self.pool,
            self.remote.as_ref(),
            &uid,
            &token,
            &mut stats,
            host,
        )
        .await?;
        files::upload_pending_stems(
            &self.pool,
            self.remote.as_ref(),
            &uid,
            &token,
            &mut stats,
            host,
        )
        .await?;
        files::upload_pending_album_art(
            &self.pool,
            self.remote.as_ref(),
            &uid,
            &token,
            &mut stats,
            host,
        )
        .await?;
        files::download_pending_audio(&self.pool, self.remote.as_ref(), host, &token, &mut stats)
            .await?;
        files::download_pending_stems(&self.pool, self.remote.as_ref(), host, &token, &mut stats)
            .await?;
        files::download_pending_album_art(
            &self.pool,
            self.remote.as_ref(),
            host,
            &token,
            &mut stats,
        )
        .await?;
        Ok(stats)
    }
}

/// Scan all tables for dirty records and enqueue them into pending_ops.
/// Single implementation used by both sync_full and the background loop.
/// Batches in groups of DIRTY_BATCH_LIMIT to bound memory usage.
pub async fn enqueue_dirty(pool: &SqlitePool, uid: &str) -> Result<usize, SyncError> {
    let mut count = 0;
    for table in registry::TABLES {
        if registry::push_policy(table.name) != registry::PushPolicy::DirtyUpsert {
            continue;
        }
        let base_sql = table.dirty_query();
        let has_principal = table.has_principal();
        let sql = format!("{base_sql} LIMIT {DIRTY_BATCH_LIMIT}");

        if table.is_composite_pk() {
            let rows: Vec<(String, String)> = if has_principal {
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
                        uid,
                        table.name,
                        &record_id,
                        &json,
                        table.conflict_key,
                    )
                    .await?;
                    count += 1;
                }
            }
        } else {
            let ids: Vec<String> = if has_principal {
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
                        uid,
                        table.name,
                        record_id,
                        &json,
                        table.conflict_key,
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
        let val: serde_json::Value = if registry::is_binary_column(table.name, col) {
            match row.try_get::<Vec<u8>, _>(col) {
                Ok(bytes) => serde_json::Value::String(encode_postgres_bytea(&bytes)),
                Err(error) => {
                    return Err(SyncError::Parse(format!(
                        "failed to read binary column {}.{col}: {error}",
                        table.name
                    )));
                }
            }
        } else if let Ok(s) = row.try_get::<Option<String>, _>(col) {
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

fn encode_postgres_bytea(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(2 + bytes.len() * 2);
    encoded.push_str("\\x");
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

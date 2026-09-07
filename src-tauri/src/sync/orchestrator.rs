//! Sync orchestrator: the single entry point for all sync operations.

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify};

use crate::services::authored_documents::AuthoredDocuments;

use super::error::SyncError;
use super::files::{self, FileSyncStats};
use super::host::SyncHost;
use super::pull::{self, PullStats};
use super::push;
use super::registry;
use super::traits::RemoteClient;

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
    progress: Arc<super::progress::Progress>,
    audio_download_lock: Arc<Mutex<()>>,
    state_pool: SqlitePool,
    remote: Arc<dyn RemoteClient>,
    authored: AuthoredDocuments,
    pub(crate) push_notify: Arc<Notify>,
    // Failures are scoped to the account and operation that observed them.
    last_errors:
        Arc<std::sync::Mutex<std::collections::BTreeMap<(String, &'static str), Vec<String>>>>,
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
            progress: Arc::default(),
            audio_download_lock: Arc::default(),
            state_pool,
            remote,
            authored,
            push_notify: Arc::new(Notify::new()),
            sync_lock: Arc::new(Mutex::new(())),
            last_errors: Arc::default(),
        }
    }

    pub async fn status(&self) -> Result<crate::models::sync::SyncStatus, SyncError> {
        let Some(uid) = crate::database::local::auth::admitted_principal(&self.pool)
            .await
            .map_err(SyncError::Local)?
        else {
            return Ok(crate::models::sync::SyncStatus::default());
        };
        let principal = crate::database::local::auth::principal_key(Some(&uid));
        let failures = sqlx::query_as::<_, crate::models::sync::SyncFailure>(
            "SELECT table_name, record_id, subject, attempts, permanent, last_error
             FROM sync_push_failures WHERE principal_key = ?
             ORDER BY permanent DESC, table_name, record_id, subject",
        )
        .bind(principal)
        .fetch_all(&self.pool)
        .await?;
        // Use the same durability predicates as sign-out: include backoff,
        // exclude permanently blocked rows (which are listed as failures).
        let mut counts: Vec<String> = registry::TABLES
            .iter()
            .filter_map(|table| table.undelivered_count_sql())
            .map(|sql| format!("SELECT ({sql}) AS pending"))
            .collect();
        counts.push(
            "SELECT COUNT(*) AS pending FROM sync_tombstones AS tombstone
            LEFT JOIN sync_push_failures AS failure
              ON failure.principal_key = tombstone.principal_key
             AND failure.table_name = tombstone.table_name
             AND failure.record_id = tombstone.record_id AND failure.subject = 'tombstone'
            WHERE tombstone.principal_key = 'signed-in:' || ?1
              AND COALESCE(failure.permanent, 0) = 0"
                .into(),
        );
        let pending_changes: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COALESCE(SUM(pending), 0) FROM ({})",
            counts.join(" UNION ALL ")
        )))
        .bind(&uid)
        .fetch_one(&self.pool)
        .await?;
        Ok(crate::models::sync::SyncStatus {
            syncing: self.sync_lock.try_lock().is_err(),
            progress: self.progress.snapshot(&uid),
            pending_changes: pending_changes as usize,
            errors: self
                .last_errors
                .lock()
                .unwrap()
                .iter()
                .filter(|((owner, _), _)| owner == &uid)
                .flat_map(|(_, errors)| errors.iter().cloned())
                .collect(),
            failures,
        })
    }

    pub async fn retry(&self) -> Result<(), SyncError> {
        let _guard = self.sync_lock.lock().await;
        let uid = crate::database::local::auth::admitted_principal(&self.pool)
            .await
            .map_err(SyncError::Local)?
            .ok_or(SyncError::AuthRequired)?;
        let principal = crate::database::local::auth::principal_key(Some(&uid));
        sqlx::query("DELETE FROM sync_push_failures WHERE principal_key = ?")
            .bind(principal)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Playback and waveform requests share a download; sync never fetches the
    /// whole audio library. Local hits do not require a signed-in session.
    pub(crate) async fn ensure_track_audio(
        &self,
        storage: &crate::storage::StorageRoot,
        track_id: &str,
    ) -> Result<(), SyncError> {
        let _guard = self.audio_download_lock.lock().await;
        files::ensure_track_audio(
            &self.pool,
            &self.state_pool,
            self.remote.as_ref(),
            storage,
            track_id,
        )
        .await
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
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
            .map_err(SyncError::from)?
            .ok_or(SyncError::AuthRequired)?;
        Ok((auth.access_token, auth.principal.user_id))
    }

    /// Full sync: send local changes, then discovery → pull → files → push.
    pub async fn sync_full(&self, host: &SyncHost) -> Result<SyncReport, SyncError> {
        let _guard = self.sync_lock.lock().await;
        println!("[sync] Starting full sync...");
        let (token, uid) = self.require_auth().await?;
        let _activity = self.progress.start(&uid);
        self.progress
            .phase("Finding shared libraries", None, "libraries");
        let mut report = SyncReport::default();
        // Existing edits must not sit behind the cloud refresh or media work.
        report.pushed += self.run_push_unlocked(&uid).await.unwrap_or_else(|error| {
            report.errors.push(format!("push: {error}"));
            0
        });
        self.progress
            .phase("Finding shared libraries", None, "libraries");

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
            &self.progress,
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
        // Unconditionally, unlike `library-changed`: this says the *pull* is
        // over, which a host holding its door shut until the library is
        // current needs to hear even when nothing changed.
        host.events.emit("sync-pulled", ());

        // 3. File sync — runs before push so storage_path updates are
        //    included when dirty records are flushed to remote.
        match self.sync_files_unlocked(host).await {
            Ok(ref stats)
                if stats.audio_uploaded
                    + stats.stems_uploaded
                    + stats.art_uploaded
                    + stats.stems_downloaded
                    + stats.art_downloaded
                    > 0 =>
            {
                println!(
                    "[sync] Files: {}↑ audio, {}↑ {}↓ stems, {}↑ {}↓ art",
                    stats.audio_uploaded,
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
            || report.files.stems_downloaded + report.files.art_downloaded > 0;
        if incoming_changed {
            host.events.emit("library-changed", ());
        }

        let mut errors = report.errors.clone();
        errors.extend(report.pull.errors.iter().cloned());
        errors.extend(report.files.errors.iter().cloned());
        self.last_errors
            .lock()
            .unwrap()
            .insert((uid.clone(), "full"), errors);
        println!("[sync] Full sync complete");
        Ok(report)
    }

    /// Enqueue dirty records and flush pending ops. Returns count pushed.
    pub async fn run_push(&self, uid: &str) -> Result<usize, SyncError> {
        let _guard = self.sync_lock.lock().await;
        let _activity = self.progress.start(uid);
        let result = self.run_push_unlocked(uid).await;
        self.last_errors.lock().unwrap().insert(
            (uid.to_string(), "push"),
            result
                .as_ref()
                .err()
                .map(|error| vec![error.to_string()])
                .unwrap_or_default(),
        );
        result
    }

    async fn run_push_unlocked(&self, uid: &str) -> Result<usize, SyncError> {
        self.progress
            .phase("Preparing local changes", None, "changes");
        self.authored
            .bootstrap_live_projections(&self.pool, Some(uid))
            .await
            .map_err(|error| {
                SyncError::Local(format!(
                    "authored projection bootstrap blocked push: {error}"
                ))
            })?;
        let n = push::flush_pending_with_integrator(
            &self.pool,
            &self.state_pool,
            self.remote.as_ref(),
            Some(&self.authored),
            &self.progress,
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
            &self.progress,
        )
        .await?;
        files::upload_pending_stems(
            &self.pool,
            self.remote.as_ref(),
            &uid,
            &token,
            &mut stats,
            host,
            &self.progress,
        )
        .await?;
        files::upload_pending_album_art(
            &self.pool,
            self.remote.as_ref(),
            &uid,
            &token,
            &mut stats,
            host,
            &self.progress,
        )
        .await?;
        files::download_pending_stems(
            &self.pool,
            self.remote.as_ref(),
            host,
            &token,
            &mut stats,
            &self.progress,
        )
        .await?;
        files::download_pending_album_art(
            &self.pool,
            self.remote.as_ref(),
            host,
            &token,
            &mut stats,
            &self.progress,
        )
        .await?;
        Ok(stats)
    }
}

/// Read a record from local SQLite as JSON, excluding local_only columns.
pub async fn read_record_as_json(
    pool: &SqlitePool,
    table: &registry::TableMeta,
    record_id: &str,
) -> Result<serde_json::Value, SyncError> {
    let cols = table.columns.join(", ");
    let pk_values = table
        .decode_record_id(record_id)
        .ok_or_else(|| SyncError::NotFound {
            table: table.name.to_string(),
            id: record_id.to_string(),
        })?;
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

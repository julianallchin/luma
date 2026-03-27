//! Sync orchestrator: the single entry point for all sync operations.
//!
//! `SyncEngine` coordinates discovery, pull, push, and file sync.
//! It is stored as Tauri managed state and shared across commands.

use std::sync::Arc;

use sqlx::SqlitePool;
use tauri::AppHandle;
use tokio::sync::Notify;

use super::error::SyncError;
use super::files::{self, FileSyncStats};
use super::pending;
use super::pull::{self, PullStats};
use super::push;
use super::registry;
use super::traits::RemoteClient;

/// Report from a full sync cycle.
#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub pull: PullStats,
    pub pushed: usize,
    pub files: FileSyncStats,
    pub errors: Vec<String>,
}

/// The sync engine. Create once at app startup, store as Tauri state.
pub struct SyncEngine {
    /// Main app database pool.
    pub pool: SqlitePool,
    /// State database pool (for auth tokens).
    pub state_pool: SqlitePool,
    /// Remote client (Supabase).
    pub remote: Arc<dyn RemoteClient>,
    /// Notify handle to wake the background sync loop.
    pub push_notify: Arc<Notify>,
}

impl SyncEngine {
    pub fn new(pool: SqlitePool, state_pool: SqlitePool, remote: Arc<dyn RemoteClient>) -> Self {
        Self {
            pool,
            state_pool,
            remote,
            push_notify: Arc::new(Notify::new()),
        }
    }

    /// Get the current auth token and user ID, or return AuthRequired.
    async fn require_auth(&self) -> Result<(String, String), SyncError> {
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

    /// Full sync cycle: discovery → pull → push → files.
    pub async fn full_sync(&self, app_handle: &AppHandle) -> Result<SyncReport, SyncError> {
        let (token, uid) = self.require_auth().await?;
        let mut report = SyncReport::default();

        // 1. Discovery: find all venues this user owns or has joined
        match pull::discover_venues(&self.pool, self.remote.as_ref(), &uid, &token).await {
            Ok(ids) => report.pull.venues_discovered = ids.len(),
            Err(e) => report.errors.push(format!("discovery: {e}")),
        };

        // 2. Delta pull in tier order
        let discovered_count = report.pull.venues_discovered;
        match pull::pull_all(&self.pool, self.remote.as_ref(), &token).await {
            Ok(mut stats) => {
                stats.venues_discovered = discovered_count;
                report.pull = stats;
            }
            Err(e) => report.errors.push(format!("pull: {e}")),
        }

        // 3. Push dirty records
        match self.push_dirty(&uid).await {
            Ok(n) => report.pushed = n,
            Err(e) => report.errors.push(format!("push: {e}")),
        }

        // 4. File sync
        match self.sync_files(app_handle).await {
            Ok(stats) => report.files = stats,
            Err(e) => report.errors.push(format!("files: {e}")),
        }

        Ok(report)
    }

    /// Pull only (for manual refresh).
    pub async fn pull(&self) -> Result<PullStats, SyncError> {
        let (token, uid) = self.require_auth().await?;
        pull::discover_venues(&self.pool, self.remote.as_ref(), &uid, &token).await?;
        pull::pull_all(&self.pool, self.remote.as_ref(), &token).await
    }

    /// Enqueue dirty records and flush immediately.
    async fn push_dirty(&self, uid: &str) -> Result<usize, SyncError> {
        for table in registry::TABLES {
            let dirty_ids = find_dirty_record_ids(&self.pool, table.name, uid).await?;
            for record_id in &dirty_ids {
                if let Ok(payload) = read_record_as_json(
                    &self.pool,
                    table.name,
                    table.columns,
                    table.local_only,
                    record_id,
                )
                .await
                {
                    let payload_str = serde_json::to_string(&payload)
                        .map_err(|e| SyncError::Parse(e.to_string()))?;
                    pending::enqueue_upsert(
                        &self.pool,
                        table.name,
                        record_id,
                        &payload_str,
                        table.conflict_key,
                        table.tier,
                    )
                    .await?;
                }
            }
        }

        push::flush_pending(&self.pool, &self.state_pool, self.remote.as_ref()).await
    }

    /// File sync: upload pending files, download stubs.
    pub async fn sync_files(&self, app_handle: &AppHandle) -> Result<FileSyncStats, SyncError> {
        let (token, uid) = self.require_auth().await?;
        let mut stats = FileSyncStats::default();

        files::upload_pending_audio(&self.pool, self.remote.as_ref(), &uid, &token, &mut stats)
            .await?;
        files::upload_pending_stems(&self.pool, self.remote.as_ref(), &uid, &token, &mut stats)
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

        Ok(stats)
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Find dirty record IDs for a table (where updated_at > synced_at or synced_at is null).
/// Only returns records owned by the given uid.
pub async fn find_dirty_record_ids(
    pool: &SqlitePool,
    table_name: &str,
    uid: &str,
) -> Result<Vec<String>, SyncError> {
    match table_name {
        "track_stems" => {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT track_id, stem_name FROM track_stems
                 WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)",
            )
            .bind(uid)
            .fetch_all(pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|(tid, sn)| format!("{tid}:{sn}"))
                .collect())
        }
        "venue_implementation_overrides" => {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT venue_id, pattern_id FROM venue_implementation_overrides
                 WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)",
            )
            .bind(uid)
            .fetch_all(pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|(vid, pid)| format!("{vid}:{pid}"))
                .collect())
        }
        "fixture_group_members" => {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT fixture_id, group_id FROM fixture_group_members
                 WHERE synced_at IS NULL OR updated_at > synced_at",
            )
            .fetch_all(pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|(fid, gid)| format!("{fid}:{gid}"))
                .collect())
        }
        _ => {
            let pk_col = match table_name {
                "track_beats" | "track_roots" | "track_waveforms" => "track_id",
                _ => "id",
            };
            let sql = format!(
                "SELECT {pk_col} FROM {table_name}
                 WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)"
            );
            let ids: Vec<String> = sqlx::query_scalar(&sql).bind(uid).fetch_all(pool).await?;
            Ok(ids)
        }
    }
}

/// Read a single record from local SQLite and return it as a JSON Value.
/// `local_only` columns are excluded from the output.
pub async fn read_record_as_json(
    pool: &SqlitePool,
    table_name: &str,
    columns: &[&str],
    local_only: &[&str],
    record_id: &str,
) -> Result<serde_json::Value, SyncError> {
    let cols = columns.join(", ");

    let (where_clause, binds) = if let Some((a, b)) = record_id.split_once(':') {
        let (col_a, col_b) = match table_name {
            "track_stems" => ("track_id", "stem_name"),
            "venue_implementation_overrides" => ("venue_id", "pattern_id"),
            "fixture_group_members" => ("fixture_id", "group_id"),
            _ => ("id", "id"),
        };
        (
            format!("{col_a} = ? AND {col_b} = ?"),
            vec![a.to_string(), b.to_string()],
        )
    } else {
        let pk_col = match table_name {
            "track_beats" | "track_roots" | "track_waveforms" => "track_id",
            _ => "id",
        };
        (format!("{pk_col} = ?"), vec![record_id.to_string()])
    };

    let sql = format!("SELECT {cols} FROM {table_name} WHERE {where_clause}");
    let mut query = sqlx::query(&sql);
    for bind in &binds {
        query = query.bind(bind);
    }

    let row = query
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SyncError::NotFound {
            table: table_name.to_string(),
            id: record_id.to_string(),
        })?;

    use sqlx::Row;
    let mut map = serde_json::Map::new();
    for col in columns.iter().filter(|c| !local_only.contains(c)) {
        let val: serde_json::Value = if let Ok(s) = row.try_get::<Option<String>, _>(*col) {
            match s {
                Some(s) => serde_json::Value::String(s),
                None => serde_json::Value::Null,
            }
        } else if let Ok(i) = row.try_get::<i64, _>(*col) {
            serde_json::Value::Number(i.into())
        } else if let Ok(f) = row.try_get::<f64, _>(*col) {
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

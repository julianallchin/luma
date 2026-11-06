//! Media transfer: audio, stems, album art and agent figures, on its own clock.
//!
//! Bytes go through Supabase Storage rather than the row protocol, so this is a
//! second, slower loop: while a session exists, upload what has a local file
//! and no `storage_path`, download what has a `storage_path` and no local file.
//!
//! It writes `storage_path` through the app pool, so those writes are logged
//! and uploaded like any other edit — which is what tells the other device
//! where the bytes are.

use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::watch;

use crate::storage::StorageRoot;
use crate::sync::supabase::Storage;

use super::files::{self, FileSyncStats};
use super::progress::Progress;

/// How often the loop looks for work. Media is bulk, not interactive.
const INTERVAL: Duration = Duration::from_secs(60);

/// The media transfer loop and the progress it publishes.
#[derive(Clone)]
pub struct Media {
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<Storage>,
    progress: Arc<Progress>,
    /// Where downloaded stems and album art land, and figures are read from.
    storage: StorageRoot,
}

impl Media {
    #[must_use]
    pub fn new(pool: SqlitePool, state_pool: SqlitePool, storage: StorageRoot) -> Self {
        Self {
            pool,
            state_pool,
            remote: Arc::new(Storage::from_config()),
            progress: Arc::default(),
            storage,
        }
    }

    /// What the current transfer is doing, for the signed-in account only.
    #[must_use]
    pub fn progress(&self, uid: &str) -> Option<crate::models::sync::SyncProgress> {
        self.progress.snapshot(uid)
    }

    /// Transfer until `shutdown`. One pass per [`INTERVAL`], skipped while
    /// signed out — there is no bucket to talk to without a token.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                () = tokio::time::sleep(INTERVAL) => {}
            }
            if *shutdown.borrow() {
                break;
            }
            match self.pass().await {
                Ok(stats) if !stats.errors.is_empty() => {
                    for error in &stats.errors {
                        log::warn!("[media] {error}");
                    }
                }
                Ok(_) => {}
                Err(error) => log::debug!("[media] {error}"),
            }
        }
    }

    /// One transfer pass. Public so a test can drive it without the timer.
    ///
    /// # Errors
    ///
    /// If there is no session, or a database read fails. A per-file failure is
    /// collected into [`FileSyncStats::errors`] instead — one unreadable stem
    /// must not stop the rest.
    pub async fn pass(&self) -> Result<FileSyncStats, super::error::SyncError> {
        let auth = crate::database::local::auth::get_current_auth(&self.state_pool)
            .await?
            .ok_or(super::error::SyncError::AuthRequired)?;
        let (token, uid) = (auth.access_token, auth.principal.user_id);
        let _activity = self.progress.start(&uid);
        let mut stats = FileSyncStats::default();
        let remote = self.remote.as_ref();
        files::upload_pending_audio(&self.pool, remote, &uid, &token, &mut stats, &self.progress)
            .await?;
        files::upload_pending_stems(&self.pool, remote, &uid, &token, &mut stats, &self.progress)
            .await?;
        files::upload_pending_album_art(
            &self.pool,
            remote,
            &uid,
            &token,
            &mut stats,
            &self.progress,
        )
        .await?;
        files::upload_pending_agent_figures(
            &self.pool,
            remote,
            &uid,
            &token,
            &mut stats,
            &self.storage,
            &self.progress,
        )
        .await?;
        files::download_pending_stems(
            &self.pool,
            remote,
            &self.storage,
            &token,
            &mut stats,
            &self.progress,
        )
        .await?;
        files::download_pending_album_art(
            &self.pool,
            remote,
            &self.storage,
            &token,
            &mut stats,
            &self.progress,
        )
        .await?;
        Ok(stats)
    }
}

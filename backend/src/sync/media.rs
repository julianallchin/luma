//! Media transfer: audio, stems and album art, on its own clock.
//!
//! Records move through PowerSync. Bytes do not — Supabase Storage is a
//! separate service with its own bucket paths, and a 40 MB stem has nothing to
//! do with a row checkpoint. So this is a second, slower loop: while a session
//! exists, upload what has a local file and no `storage_path`, download what
//! has a `storage_path` and no local file, and say so through [`Progress`].
//!
//! The loop writes `tracks.storage_path` and `track_stems.storage_path`
//! through the app pool, so those writes are logged and uploaded like any
//! other edit — which is exactly right: a storage path is a fact about the
//! row, and the other device needs it to find the bytes.

use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::watch;

use crate::database::remote::common::SupabaseClient;

use super::files::{self, FileSyncStats};
use super::host::SyncHost;
use super::progress::Progress;

/// How often the loop looks for work. Media is bulk, not interactive.
const INTERVAL: Duration = Duration::from_secs(60);

/// The media transfer loop and the progress it publishes.
#[derive(Clone)]
pub struct Media {
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<SupabaseClient>,
    progress: Arc<Progress>,
}

impl Media {
    #[must_use]
    pub fn new(pool: SqlitePool, state_pool: SqlitePool) -> Self {
        Self {
            pool,
            state_pool,
            remote: Arc::new(SupabaseClient::new(
                crate::config::SUPABASE_URL.to_owned(),
                crate::config::supabase_anon_key(),
            )),
            progress: Arc::default(),
        }
    }

    /// What the current transfer is doing, for the signed-in account only.
    #[must_use]
    pub fn progress(&self, uid: &str) -> Option<crate::models::sync::SyncProgress> {
        self.progress.snapshot(uid)
    }

    /// Transfer until `shutdown`. One pass per [`INTERVAL`], skipped while
    /// signed out — there is no bucket to talk to without a token.
    pub async fn run(self, host: SyncHost, mut shutdown: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                () = tokio::time::sleep(INTERVAL) => {}
            }
            if *shutdown.borrow() {
                break;
            }
            match self.pass(&host).await {
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
    pub async fn pass(&self, host: &SyncHost) -> Result<FileSyncStats, super::error::SyncError> {
        let auth = crate::database::local::auth::get_current_auth(&self.state_pool)
            .await?
            .ok_or(super::error::SyncError::AuthRequired)?;
        let (token, uid) = (auth.access_token, auth.principal.user_id);
        let _activity = self.progress.start(&uid);
        let mut stats = FileSyncStats::default();
        let remote = self.remote.as_ref();
        files::upload_pending_audio(
            &self.pool,
            remote,
            &uid,
            &token,
            &mut stats,
            host,
            &self.progress,
        )
        .await?;
        files::upload_pending_stems(
            &self.pool,
            remote,
            &uid,
            &token,
            &mut stats,
            host,
            &self.progress,
        )
        .await?;
        files::upload_pending_album_art(
            &self.pool,
            remote,
            &uid,
            &token,
            &mut stats,
            host,
            &self.progress,
        )
        .await?;
        files::download_pending_stems(
            &self.pool,
            remote,
            host,
            &token,
            &mut stats,
            &self.progress,
        )
        .await?;
        files::download_pending_album_art(
            &self.pool,
            remote,
            host,
            &token,
            &mut stats,
            &self.progress,
        )
        .await?;
        if stats.stems_downloaded + stats.art_downloaded > 0 {
            host.events.emit("library-changed", ());
        }
        Ok(stats)
    }
}

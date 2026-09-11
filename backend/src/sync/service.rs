//! The sync service: connect while signed in, publish status, announce change.
//!
//! It owns three long-lived tasks over one `PowerSyncDatabase`:
//!
//! 1. the session watcher, which connects on sign-in and disconnects on
//!    sign-out — the connector's credentials are only valid for one account, so
//!    an identity change is a reconnect, not a token swap;
//! 2. the status watcher, which turns the SDK's status stream into the one
//!    [`SyncStatus`] the UI reads;
//! 3. the table watcher, which coalesces downloaded writes into a single
//!    `replica-changed` event naming the tables that moved.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{FutureExt, StreamExt};
use luma_sync::powersync::{sdk::SyncOptions, Database};
use sqlx::SqlitePool;
use tokio::sync::watch;

use crate::dispatch::Events;
use crate::models::sync::SyncStatus;

use super::connector::Connector;
use super::schema;

/// How often the session watcher re-reads the signed-in principal.
///
/// The session lives in the *state* database, which the SDK's update notifiers
/// do not see — it is a different file. A poll is the honest mechanism; five
/// seconds is far below the cost of a wrong answer (a sign-in that has not
/// started syncing yet).
const SESSION_POLL: Duration = Duration::from_secs(5);

/// How long writes are gathered before one `replica-changed` goes out.
const COALESCE: Duration = Duration::from_millis(100);

/// What the status watcher learned, shared with `status()`.
#[derive(Default)]
struct Observed {
    connected: bool,
    uploading: bool,
    downloading: bool,
    last_synced_at: Option<String>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct Service {
    database: Arc<Database>,
    observed: Arc<Mutex<Observed>>,
    shutdown: watch::Sender<bool>,
}

impl Service {
    /// Start syncing `database`, reading sessions from `state_pool` and
    /// emitting through `events`.
    ///
    /// # Errors
    ///
    /// If the upload connector cannot be built.
    pub fn start(
        database: Database,
        state_pool: SqlitePool,
        events: Events,
    ) -> Result<Self, String> {
        let database = Arc::new(database);
        let (shutdown, _) = watch::channel(false);
        let service = Self {
            database: Arc::clone(&database),
            observed: Arc::default(),
            shutdown: shutdown.clone(),
        };
        tokio::spawn(sessions(
            Arc::clone(&database),
            state_pool,
            shutdown.subscribe(),
        ));
        tokio::spawn(statuses(
            Arc::clone(&database),
            Arc::clone(&service.observed),
            shutdown.subscribe(),
        ));
        tokio::spawn(replicas(database, events, shutdown.subscribe()));
        Ok(service)
    }

    /// The app database pool. Writers acquired from it carry the upload
    /// triggers, so this is the only pool application code may write through.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.database.sql
    }

    /// What `get_sync_status` answers.
    ///
    /// `pending_uploads` is counted rather than remembered: `ps_crud` is the
    /// queue, and any other number would be a second opinion about it.
    ///
    /// # Errors
    ///
    /// If the queue cannot be counted.
    pub async fn status(&self) -> Result<SyncStatus, String> {
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ps_crud")
            .fetch_one(&self.database.sql)
            .await
            .map_err(|error| error.to_string())?;
        let observed = self.observed.lock().expect("poisoned");
        Ok(SyncStatus {
            connected: observed.connected,
            uploading: observed.uploading,
            downloading: observed.downloading,
            last_synced_at: observed.last_synced_at.clone(),
            pending_uploads: usize::try_from(pending).unwrap_or(0),
            error: observed.error.clone(),
        })
    }

    /// Stop the tasks. Dropping the last `Service` does the same.
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // Only the last handle: `shutdown` is cloned into every task, so this
        // is a no-op while any of them is still alive and holding one.
        if self.shutdown.receiver_count() == 0 {
            let _ = self.shutdown.send(true);
        }
    }
}

/// Connect while a session exists, disconnect when it does not.
async fn sessions(
    database: Arc<Database>,
    state_pool: SqlitePool,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut current: Option<String> = None;
    loop {
        let signed_in = match crate::database::local::auth::get_current_user_id(&state_pool).await {
            Ok(user) => user,
            Err(error) => {
                log::warn!("[sync] could not read the session: {error}");
                None
            }
        };
        if signed_in != current {
            database.sync.disconnect().await;
            current = signed_in;
            if current.is_some() {
                match Connector::new(
                    database.sync.clone(),
                    database.sql.clone(),
                    state_pool.clone(),
                ) {
                    // The sync rules declare `owned`, `venue` and `library`
                    // with `auto_subscribe: true`, and `SyncOptions` includes
                    // default streams already — there is nothing to subscribe
                    // to by hand.
                    Ok(connector) => database.sync.connect(SyncOptions::new(connector)).await,
                    Err(error) => log::error!("[sync] {error}"),
                }
            }
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            () = tokio::time::sleep(SESSION_POLL) => {}
        }
        if *shutdown.borrow() {
            break;
        }
    }
    database.sync.disconnect().await;
}

/// Mirror the SDK's status into the shape the UI reads.
async fn statuses(
    database: Arc<Database>,
    observed: Arc<Mutex<Observed>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let updates = database.sync.watch_status();
    futures_util::pin_mut!(updates);
    loop {
        let status = tokio::select! {
            _ = shutdown.changed() => break,
            status = updates.next() => match status {
                Some(status) => status,
                None => break,
            },
        };
        let mut observed = observed.lock().expect("poisoned");
        let was_busy = observed.downloading || observed.uploading;
        observed.connected = status.is_connected();
        observed.downloading = status.is_downloading();
        observed.uploading = status.is_uploading();
        observed.error = status
            .download_error()
            .or_else(|| status.upload_error())
            .map(ToString::to_string);
        // "Last synced" is the moment the two directions went quiet while
        // connected. The SDK reports it per stream; this is the whole database.
        if was_busy && observed.connected && !observed.downloading && !observed.uploading {
            observed.last_synced_at = Some(chrono::Utc::now().to_rfc3339());
        }
    }
}

/// Coalesce writes to synced tables into one `replica-changed` event.
async fn replicas(database: Arc<Database>, events: Events, mut shutdown: watch::Receiver<bool>) {
    let updates = database.sync.watch_all_updates();
    futures_util::pin_mut!(updates);
    let mut changed = BTreeSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            tables = updates.next() => match tables {
                Some(tables) => changed.extend(
                    tables
                        .into_iter()
                        .filter(|table| schema::table(table).is_some()),
                ),
                None => break,
            },
        }
        if changed.is_empty() {
            continue;
        }
        // Drain whatever else lands inside the window, then announce once.
        tokio::time::sleep(COALESCE).await;
        while let Some(Some(tables)) = updates.next().now_or_never() {
            changed.extend(
                tables
                    .into_iter()
                    .filter(|table| schema::table(table).is_some()),
            );
        }
        events.emit(
            "replica-changed",
            serde_json::json!({ "tables": changed.iter().collect::<Vec<_>>() }),
        );
        changed.clear();
    }
}

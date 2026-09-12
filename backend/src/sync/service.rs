//! Connect while signed in, publish status, announce change.
//!
//! Three long-lived tasks over one `PowerSyncDatabase`: the session watcher
//! (credentials are valid for one account, so an identity change is a
//! reconnect, not a token swap), the status watcher, and the table watcher that
//! coalesces downloaded writes into one `replica-changed` event.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{FutureExt, StreamExt};
use luma_sync::powersync::{sdk::SyncOptions, Connections, Database, HttpClient};
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
    /// Start PowerSync on the connection pair `open_app_db_at` returned.
    ///
    /// The whole seam: a host hands over the connections, the state pool and
    /// its event bus, and gets back something that connects when somebody
    /// signs in. Everything about *what* syncs is [`super::schema`], and it is
    /// read from here so a host never names a table.
    ///
    /// # Errors
    ///
    /// If the SDK cannot start on those connections.
    pub async fn open(
        connections: Connections,
        state_pool: SqlitePool,
        events: Events,
    ) -> Result<Self, String> {
        let database = connections
            .start(super::schema::schema(), HttpClient::new())
            .await
            .map_err(|error| format!("could not start PowerSync: {error}"))?;
        Self::start(database, state_pool, events)
    }

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
        let mut diagnose = false;
        {
            let mut observed = observed.lock().expect("poisoned");
            let was_busy = observed.downloading || observed.uploading;
            observed.connected = status.is_connected();
            observed.downloading = status.is_downloading();
            observed.uploading = status.is_uploading();
            // The SDK retries forever and reports the failure on one tick. Keeping
            // the last one is the difference between a user seeing "sync is
            // rejecting your sign-in" and seeing nothing at all while nothing
            // syncs; it is cleared when a direction actually succeeds.
            match status
                .download_error()
                .or_else(|| status.upload_error())
                .map(|error| readable(&error.to_string()))
            {
                Some(error) => {
                    if observed.error.as_deref() != Some(error.as_str()) {
                        log::warn!("[sync] {error}");
                        if error.contains("CONSTRAINT") {
                            diagnose = true;
                        }
                    }
                    observed.error = Some(error);
                }
                None if observed.connected && !observed.downloading && !observed.uploading => {
                    observed.error = None;
                }
                None => {}
            }
            // "Last synced" is the moment the two directions went quiet while
            // connected. The SDK reports it per stream; this is the whole database.
            if was_busy && observed.connected && !observed.downloading && !observed.uploading {
                observed.last_synced_at = Some(chrono::Utc::now().to_rfc3339());
            }
        }
        if diagnose {
            log::warn!("[sync] {}", refused(&database.sql).await);
        }
    }
}

/// Which downloaded row the local schema refuses, named.
///
/// A checkpoint is one transaction: one row the local schema will not take
/// rolls the whole thing back, so nothing arrives — and the SDK reports only
/// `CONSTRAINT`, without saying which of thirty tables. This replays the
/// waiting oplog through the same generated statements, in one transaction it
/// then throws away, and stops at the first refusal. Only ever run after a
/// failure, because it is the failure that needs a name.
async fn refused(pool: &SqlitePool) -> String {
    let rows: Vec<(String, String)> =
        match sqlx::query_as("SELECT row_type, data FROM ps_oplog WHERE data IS NOT NULL")
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(error) => return format!("the waiting download is unreadable: {error}"),
        };
    let mut connection = match pool.acquire().await {
        Ok(connection) => connection,
        Err(error) => return format!("could not replay the download: {error}"),
    };
    let _ = sqlx::query("SAVEPOINT diagnose")
        .execute(&mut *connection)
        .await;
    let mut verdict = "the waiting download applies; the refusal is elsewhere".to_owned();
    for (name, data) in rows {
        let Some(table) = schema::table(&name) else {
            continue;
        };
        let Ok(row) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
        else {
            continue;
        };
        let (put, _) = schema::statements(table);
        let mut query = sqlx::query(sqlx::AssertSqlSafe(put));
        for column in table.columns {
            query = query.bind(match row.get(*column) {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(text)) => Some(text.clone()),
                Some(value) => Some(value.to_string()),
            });
        }
        let id = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        for _ in 0..table
            .local_defaults
            .iter()
            .flat_map(|(_, fill)| fill.matches('?'))
            .count()
        {
            query = query.bind(Some(id.to_owned()));
        }
        if let Err(error) = query.execute(&mut *connection).await {
            verdict = format!("{name} {id} cannot be written here: {error}");
            break;
        }
    }
    let _ = sqlx::query("ROLLBACK TO diagnose; RELEASE diagnose")
        .execute(&mut *connection)
        .await;
    verdict
}

/// Say what a transport failure means, where the wire says it in codes.
///
/// A 401 is the one a person can act on: the token this build mints is not one
/// the sync service will accept, which is a sign-in problem however it is
/// spelled downstream. Everything else is passed through — an unfamiliar error
/// in full is more use than a familiar one in the wrong words.
fn readable(error: &str) -> String {
    if error.contains("401") || error.to_lowercase().contains("unauthor") {
        return format!("Sync rejected this sign-in. {error}");
    }
    error.to_owned()
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

//! PowerSync and SQLx connections to the same local database.

use std::{collections::HashSet, future::Future, path::Path, pin::Pin, time::Duration};

pub use ::powersync as sdk;
pub use powersync_http::Client as HttpClient;
use sdk::{env::PowerSyncEnvironment, schema::Schema, ConnectionPool, PowerSyncDatabase};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqliteConnection, SqlitePool,
};
mod tasks;
use tasks::Task;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] sqlx::Error),
    #[error(transparent)]
    PowerSync(#[from] sdk::error::PowerSyncError),
    #[error("loading the PowerSync core extension failed: {0}")]
    Extension(String),
}

/// Register the core extension with SQLite, once per process.
///
/// `powersync_auto_extension` appends to SQLite's global auto-extension list.
/// Calling it per database — which a test binary opening thirty of them does —
/// runs the initializer that many times on every new connection, and the
/// second run frees what the first one owns. The process only ever needs one.
fn register_extension() -> Result<(), Error> {
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            PowerSyncEnvironment::powersync_auto_extension().map_err(|error| error.to_string())
        })
        .clone()
        .map_err(Error::Extension)
}

/// Prepare the application schema before starting PowerSync on these connections.
pub struct Connections {
    sql: SqlitePool,
    sync: ConnectionPool,
}

pub type Initialize =
    for<'c> fn(
        &'c mut SqliteConnection,
    ) -> Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + Send + 'c>>;

impl Connections {
    pub async fn open(path: &Path, max_connections: u32) -> Result<Self, Error> {
        Self::open_with(path, max_connections, |_| Box::pin(async { Ok(()) })).await
    }

    pub async fn open_with(
        path: &Path,
        max_connections: u32,
        initialize: Initialize,
    ) -> Result<Self, Error> {
        register_extension()?;
        let sync = ConnectionPool::open(path)?;
        let notifiers = sync.update_notifiers().clone();
        let sql = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    sqlx::query("SELECT powersync_update_hooks('install')")
                        .execute(&mut *connection)
                        .await?;
                    initialize(connection).await
                })
            })
            .after_release(move |connection, _| {
                let notifiers = notifiers.clone();
                Box::pin(async move {
                    let json: String = sqlx::query_scalar("SELECT powersync_update_hooks('get')")
                        .fetch_one(connection)
                        .await?;
                    let tables: HashSet<String> = serde_json::from_str(&json)
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
                    notifiers.notify_updates(&tables);
                    Ok(true)
                })
            })
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await?;
        Ok(Self { sql, sync })
    }

    /// Writers release their connection after committing to publish notifications.
    pub fn sql(&self) -> &SqlitePool {
        &self.sql
    }

    pub async fn start(self, schema: Schema, http: HttpClient) -> Result<Database, Error> {
        let sync = PowerSyncDatabase::new(
            PowerSyncEnvironment::custom(http, self.sync, PowerSyncEnvironment::tokio_timer()),
            schema,
        );
        // Initialize managed views and raw-table mappings before any actors run.
        drop(sync.reader().await?);
        let tasks = sync.async_tasks().spawn_with(Task::spawn);
        Ok(Database {
            sql: self.sql,
            sync,
            tasks,
        })
    }
}

/// Owns the sync tasks; dropping it stops them even when a connector holds a clone.
pub struct Database {
    pub sql: SqlitePool,
    pub sync: PowerSyncDatabase,
    tasks: Vec<Task>,
}

impl Database {
    pub async fn close(mut self) {
        self.sync.disconnect().await;
        self.sql.close().await;
        for task in self.tasks.drain(..) {
            task.stop().await;
        }
    }
}

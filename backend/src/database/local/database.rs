use luma_sync::powersync::Connections;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use std::path::Path;

/// `SqlitePool` is itself a shared handle, so cloning `Db` hands out another
/// reference to the same pool, never a second pool.
#[derive(Clone)]
pub struct Db(pub SqlitePool);

/// Create the directory, upgrade legacy payloads and run every migration.
///
/// Migrations run on their own connection with foreign keys off — several of
/// them rebuild a table and would trip over their own child rows otherwise —
/// and that connection is closed before anything else opens the file. Returns
/// where the database is.
async fn migrate_app_db_at(app_dir: &Path) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(app_dir)
        .map_err(|error| format!("Failed to create app config dir {}: {error}", app_dir.display()))?;
    let db_path = app_dir.join("luma.db");
    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .map_err(|error| {
            format!(
                "Failed to connect to database at {}: {error}",
                db_path.display()
            )
        })?;

    // Some relational graph payloads predate the current canonical document
    // format. Upgrade those explicit legacy shapes before migrations install
    // admission guards and before authored-state root import.
    super::legacy_graph_upgrade::upgrade_legacy_graph_json(&migrate_pool).await?;

    sqlx::migrate!("./migrations")
        .run(&migrate_pool)
        .await
        .map_err(|error| format!("Failed to run app migrations: {error}"))?;
    migrate_pool.close().await;
    Ok(db_path)
}

/// Migrate and open the app database without the sync SDK.
///
/// The same migrations and the same change log on every writer, on an ordinary
/// SQLx pool. What it does not get is the SDK's connection pair, and so no
/// upload queue: see [`crate::sync::triggers::install`]. A test that wants one
/// makes its own; a host that wants to sync calls [`open_app_db_at`].
///
/// # Errors
///
/// If the directory cannot be created, a migration fails, or the file cannot
/// be opened.
pub async fn init_app_db_at(app_dir: &Path) -> Result<Db, String> {
    let db_path = migrate_app_db_at(app_dir).await?;
    let pool = SqlitePoolOptions::new()
        .max_connections(16)
        .after_connect(|connection, _| {
            Box::pin(async move {
                crate::sync::triggers::install(connection)
                    .await
                    .map_err(|error| sqlx::Error::Configuration(error.into()))
            })
        })
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .map_err(|error| {
            format!(
                "Failed to connect to database at {}: {error}",
                db_path.display()
            )
        })?;
    Ok(Db(pool))
}

/// Migrate and open the app database on the connection pair PowerSync needs.
///
/// One SQLite file, two pools: the SQLx pool every writer uses, and the SDK's
/// own. A host that syncs hands the returned [`Connections`] to
/// `Connections::start` and then to `sync::service::Service`; the pool inside
/// it *is* `Db`'s pool, so there is never a second view of the database.
///
/// # Errors
///
/// If the directory cannot be created, a migration fails, or the file cannot
/// be opened.
pub async fn open_app_db_at(app_dir: &Path) -> Result<(Db, Connections), String> {
    let db_path = migrate_app_db_at(app_dir).await?;

    // Now open the real pool WITH foreign_keys enabled.
    //
    // Every pooled connection is a writer, so every one carries the change log
    // and the PowerSync upload queue. Both trigger sets are TEMP, so they
    // belong to the connection and never to the file: the sync SDK holds its
    // own pool and deliberately has neither, which is what keeps a downloaded
    // row out of this database's history and out of the upload queue.
    let connections = Connections::open_with(&db_path, 16, |connection| {
        Box::pin(async move {
            crate::sync::triggers::install(connection)
                .await
                .map_err(|error| sqlx::Error::Configuration(error.into()))
        })
    })
    .await
    .map_err(|error| {
        format!(
            "Failed to connect to database at {}: {error}",
            db_path.display()
        )
    })?;

    Ok((Db(connections.sql().clone()), connections))
}

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

/// Migrate and open the app database, discarding the sync half.
///
/// The pool is the same one [`open_app_db_at`] builds — PowerSync extension
/// loaded, both trigger sets on every connection — so a test writes through
/// exactly what the app writes through. What it does not get is a
/// [`Connections`] to start syncing with, which a test has no server for.
///
/// # Errors
///
/// If the directory cannot be created, a migration fails, or the file cannot
/// be opened.
pub async fn init_app_db_at(app_dir: &Path) -> Result<Db, String> {
    let (db, connections) = open_app_db_at(app_dir).await?;
    // The SDK's own pool closes with this; `db` holds its own handle on the
    // SQLx pool, which is what every caller here actually writes through.
    drop(connections);
    Ok(db)
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
    std::fs::create_dir_all(app_dir).map_err(|e| {
        format!(
            "Failed to create app config dir {}: {}",
            app_dir.display(),
            e
        )
    })?;

    let db_path = app_dir.join("luma.db");
    // Connect WITHOUT foreign_keys for migrations (some migrations need FK checks off)
    let migrate_options = SqliteConnectOptions::new()
        .filename(&db_path)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true)
        .foreign_keys(false);

    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(migrate_options)
        .await
        .map_err(|e| {
            format!(
                "Failed to connect to database at {}: {}",
                db_path.display(),
                e
            )
        })?;

    // Some relational graph payloads predate the current canonical document
    // format. Upgrade those explicit legacy shapes before migrations install
    // admission guards and before authored-state root import.
    super::legacy_graph_upgrade::upgrade_legacy_graph_json(&migrate_pool).await?;

    sqlx::migrate!("./migrations")
        .run(&migrate_pool)
        .await
        .map_err(|e| format!("Failed to run app migrations: {}", e))?;

    migrate_pool.close().await;

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

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use std::path::Path;

/// `SqlitePool` is itself a shared handle, so cloning `Db` hands out another
/// reference to the same pool, never a second pool.
#[derive(Clone)]
pub struct Db(pub SqlitePool);

pub async fn init_app_db_at(app_dir: &Path) -> Result<Db, String> {
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

    // Now open the real pool WITH foreign_keys enabled
    let connect_options = SqliteConnectOptions::new()
        .filename(&db_path)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true)
        .foreign_keys(true);

    // Every writer connection carries the upload-capture triggers. They are
    // TEMP, so they belong to the connection and never to the file: the SDK's
    // own connection applies downloads without them, which is what keeps a
    // downloaded row from being uploaded straight back.
    let pool = SqlitePoolOptions::new()
        .max_connections(16)
        .after_connect(|connection, _| {
            Box::pin(async move { crate::sync::triggers::install_crud_triggers(connection).await })
        })
        .connect_with(connect_options)
        .await
        .map_err(|e| {
            format!(
                "Failed to connect to database at {}: {}",
                db_path.display(),
                e
            )
        })?;

    Ok(Db(pool))
}
